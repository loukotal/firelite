use anyhow::Context;
use clap::{Parser, Subcommand};
use firelite::{config::DaemonConfig, functions::FunctionsConfig, server};
use std::{net::SocketAddr, path::PathBuf};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

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
    /// Register a checkout/workdir against a project namespace.
    Attach {
        #[arg(long)]
        project: String,
        #[arg(long)]
        workdir: PathBuf,
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
        /// Command to run in the watched functions directory before loading/reloading workers.
        #[arg(long)]
        build_command: Option<String>,
        /// Function export/name filter. Can be repeated.
        #[arg(long = "filter")]
        filters: Vec<String>,
    },
    /// Run Auth, Storage, and Cloud Functions emulators together.
    Emulators {
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 9099)]
        auth_port: u16,
        #[arg(long, default_value_t = 9199)]
        storage_port: u16,
        #[arg(long, default_value_t = 5001)]
        functions_port: u16,
        #[arg(long)]
        watch: PathBuf,
        /// Command to run in the watched functions directory before loading/reloading workers.
        #[arg(long)]
        build_command: Option<String>,
        /// Function export/name filter. Can be repeated.
        #[arg(long = "filter")]
        filters: Vec<String>,
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
        Command::Attach { project, workdir } => {
            println!(
                "attach is scaffolded: project={project} workdir={}",
                workdir.display()
            );
            Ok(())
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
            build_command,
            filters,
        } => {
            let addr = parse_addr("functions", &host, port)?;
            firelite::functions::serve(FunctionsConfig {
                project_id: project,
                source_dir: watch,
                addr,
                build_command,
                filters,
            })
            .await
        }
        Command::Emulators {
            project,
            host,
            auth_port,
            storage_port,
            functions_port,
            watch,
            build_command,
            filters,
        } => {
            let state = server::app_state();
            let daemon_addr = parse_addr("auth daemon", &host, auth_port)?;
            let functions_addr = parse_addr("functions", &host, functions_port)?;

            let daemon = server::serve_with_state(
                "firelite auth emulator",
                DaemonConfig { addr: daemon_addr },
                state.clone(),
            );
            let functions = firelite::functions::serve(FunctionsConfig {
                project_id: project,
                source_dir: watch,
                addr: functions_addr,
                build_command,
                filters,
            });

            if storage_port == auth_port {
                tokio::try_join!(daemon, functions)?;
            } else {
                let storage_addr = parse_addr("storage", &host, storage_port)?;
                let storage = server::serve_storage_with_state(
                    DaemonConfig { addr: storage_addr },
                    state.clone(),
                );
                tokio::try_join!(daemon, storage, functions)?;
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
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("firelite=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();
}
