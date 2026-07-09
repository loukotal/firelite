use anyhow::Context;
use clap::{Parser, Subcommand};
use firelite::{config::DaemonConfig, functions::FunctionsConfig, server, tasks::FunctionsTarget};
use std::{fmt, io::IsTerminal, net::SocketAddr, path::PathBuf};
use tracing::{field::Visit, Event, Level, Subscriber};
use tracing_subscriber::{
    fmt::{
        format::{FormatEvent, FormatFields, Writer},
        time::{FormatTime, SystemTime},
        FmtContext,
    },
    registry::LookupSpan,
};

#[derive(Debug, Parser)]
#[command(name = "firelite")]
#[command(about = "Lightweight Firebase Emulator Suite-compatible local services")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the shared lightweight backend daemon.
    Daemon {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 9099)]
        port: u16,
    },
    /// Reset state for a project namespace.
    Reset {
        #[arg(long)]
        project: String,
    },
    /// Run or watch checkout-specific Cloud Functions workers.
    Functions {
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 5001)]
        port: u16,
        #[arg(long)]
        watch: PathBuf,
        /// Function export/name filter. Can be repeated.
        #[arg(long = "filter")]
        filters: Vec<String>,
        /// Disable source polling and automatic worker reloads.
        #[arg(long)]
        no_reload: bool,
    },
    /// Run Auth, Storage, Pub/Sub, Cloud Tasks, and Cloud Functions emulators together.
    Emulators {
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 9099)]
        auth_port: u16,
        #[arg(long, default_value_t = 9199)]
        storage_port: u16,
        #[arg(long, default_value_t = 8085)]
        pubsub_port: u16,
        #[arg(long, default_value_t = 9899)]
        tasks_port: u16,
        #[arg(long, default_value_t = 5001)]
        functions_port: u16,
        #[arg(long)]
        watch: PathBuf,
        /// Function export/name filter. Can be repeated.
        #[arg(long = "filter")]
        filters: Vec<String>,
        /// Disable source polling and automatic worker reloads. Recommended in CI.
        #[arg(long)]
        no_reload: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    match Cli::parse().command {
        Command::Daemon { host, port } => {
            let addr = parse_addr("daemon", &host, port)?;
            server::serve(DaemonConfig { addr }).await
        }
        Command::Reset { project } => {
            println!("reset is scaffolded: project={project}");
            Ok(())
        }
        Command::Functions {
            project,
            host,
            port,
            watch,
            filters,
            no_reload,
        } => {
            let addr = parse_addr("functions", &host, port)?;
            firelite::functions::serve(FunctionsConfig {
                project_id: project,
                source_dir: watch,
                addr,
                filters,
                reload_on_change: !no_reload,
            })
            .await
        }
        Command::Emulators {
            project,
            host,
            auth_port,
            storage_port,
            pubsub_port,
            tasks_port,
            functions_port,
            watch,
            filters,
            no_reload,
        } => {
            let state = server::app_state();
            let daemon_addr = parse_addr("auth daemon", &host, auth_port)?;
            let pubsub_addr = parse_addr("pubsub", &host, pubsub_port)?;
            let tasks_addr = parse_addr("cloud tasks", &host, tasks_port)?;
            let functions_addr = parse_addr("functions", &host, functions_port)?;

            let functions = firelite::functions::prepare(FunctionsConfig {
                project_id: project.clone(),
                source_dir: watch,
                addr: functions_addr,
                filters: filters.clone(),
                reload_on_change: !no_reload,
            })
            .await?;

            state.tasks.set_functions_target(FunctionsTarget {
                project_id: project,
                functions_host: host.clone(),
                functions_port,
                filters,
            });

            let daemon = server::serve_with_state(
                "firelite auth emulator",
                DaemonConfig { addr: daemon_addr },
                state.clone(),
            );
            let functions = functions.serve();
            let tasks =
                server::serve_tasks_with_state(DaemonConfig { addr: tasks_addr }, state.clone());
            let pubsub =
                server::serve_pubsub_with_state(DaemonConfig { addr: pubsub_addr }, state.clone());

            if storage_port == auth_port {
                tokio::try_join!(daemon, pubsub, tasks, functions)?;
            } else {
                let storage_addr = parse_addr("storage", &host, storage_port)?;
                let storage = server::serve_storage_with_state(
                    DaemonConfig { addr: storage_addr },
                    state.clone(),
                );
                tokio::try_join!(daemon, storage, pubsub, tasks, functions)?;
            }
            Ok(())
        }
    }
}

fn parse_addr(label: &str, host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid {label} address {host}:{port}"))
}

fn init_tracing() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| parse_log_level(&value))
        .unwrap_or(Level::INFO);
    tracing_subscriber::fmt()
        .event_format(CompactLogFormatter {
            color: terminal_colors_enabled(),
        })
        .with_max_level(level)
        .init();
}

struct CompactLogFormatter {
    color: bool,
}

impl<S, N> FormatEvent<S, N> for CompactLogFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        if self.color {
            write!(writer, "\u{1b}[2;90m")?;
        }
        SystemTime.format_time(&mut writer)?;
        if self.color {
            write!(writer, "\u{1b}[0m")?;
        }
        writer.write_char(' ')?;

        if event.metadata().target() == "firelite_worker" {
            let mut visitor = WorkerMessageVisitor::default();
            event.record(&mut visitor);
            if let Some(message) = visitor.message {
                return writeln!(writer, "{message}");
            }
        }

        context
            .field_format()
            .format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

fn terminal_colors_enabled() -> bool {
    let forced = std::env::var("FORCE_COLOR")
        .ok()
        .is_some_and(|value| value != "0");
    forced
        || (std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").ok().as_deref() != Some("dumb")
            && (std::io::stderr().is_terminal() || std::env::var_os("HERDR_ENV").is_some()))
}

#[derive(Default)]
struct WorkerMessageVisitor {
    message: Option<String>,
}

impl Visit for WorkerMessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "worker_message" {
            self.message = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "worker_message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }
}

fn parse_log_level(value: &str) -> Option<Level> {
    let mut global = None;
    let mut firelite = None;

    for directive in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match directive.split_once('=') {
            Some((target, level)) if target.trim() == "firelite" => {
                firelite = level_from_str(level);
            }
            None => global = level_from_str(directive),
            _ => {}
        }
    }

    firelite.or(global)
}

fn level_from_str(value: &str) -> Option<Level> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(Level::TRACE),
        "debug" => Some(Level::DEBUG),
        "info" => Some(Level::INFO),
        "warn" => Some(Level::WARN),
        "error" => Some(Level::ERROR),
        _ => None,
    }
}
