use anyhow::Context;
use clap::{Parser, Subcommand};
use firelite::{
    config::DaemonConfig,
    control::{AttachRequest, AttachmentsResponse},
    functions::FunctionsConfig,
    server,
};
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
        #[arg(long, default_value = "127.0.0.1")]
        daemon_host: String,
        #[arg(long, default_value_t = 9099)]
        daemon_port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        functions_host: String,
        #[arg(long, default_value_t = 5001)]
        functions_port: u16,
        /// Function export/name filter. Can be repeated.
        #[arg(long = "filter")]
        filters: Vec<String>,
    },
    /// List functions workers attached to the daemon.
    Attachments {
        #[arg(long, default_value = "127.0.0.1")]
        daemon_host: String,
        #[arg(long, default_value_t = 9099)]
        daemon_port: u16,
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
        /// Register this functions worker with a running daemon.
        #[arg(long)]
        attach: bool,
        #[arg(long, default_value = "127.0.0.1")]
        daemon_host: String,
        #[arg(long, default_value_t = 9099)]
        daemon_port: u16,
    },
    /// Run Auth, Storage, Cloud Tasks, and Cloud Functions emulators together.
    Emulators {
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 9099)]
        auth_port: u16,
        #[arg(long, default_value_t = 9199)]
        storage_port: u16,
        #[arg(long, default_value_t = 9499)]
        tasks_port: u16,
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
        Command::Attach {
            project,
            workdir,
            daemon_host,
            daemon_port,
            functions_host,
            functions_port,
            filters,
        } => {
            attach_worker(
                project,
                workdir,
                daemon_host,
                daemon_port,
                functions_host,
                functions_port,
                filters,
            )
            .await
        }
        Command::Attachments {
            daemon_host,
            daemon_port,
        } => list_attachments(daemon_host, daemon_port).await,
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
            attach,
            daemon_host,
            daemon_port,
        } => {
            let addr = parse_addr("functions", &host, port)?;
            if attach {
                attach_worker(
                    project.clone(),
                    watch.clone(),
                    daemon_host,
                    daemon_port,
                    host.clone(),
                    port,
                    filters.clone(),
                )
                .await?;
            }
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
            tasks_port,
            functions_port,
            watch,
            build_command,
            filters,
        } => {
            let state = server::app_state();
            let daemon_addr = parse_addr("auth daemon", &host, auth_port)?;
            let tasks_addr = parse_addr("cloud tasks", &host, tasks_port)?;
            let functions_addr = parse_addr("functions", &host, functions_port)?;
            let workdir = std::fs::canonicalize(&watch).unwrap_or_else(|_| watch.clone());

            firelite::control::register_attachment(
                &state,
                AttachRequest {
                    project_id: project.clone(),
                    workdir: workdir.display().to_string(),
                    functions_host: host.clone(),
                    functions_port,
                    filters: filters.clone(),
                },
            )
            .map_err(|status| anyhow::anyhow!("failed to register functions worker: {status}"))?;

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
            let tasks =
                server::serve_tasks_with_state(DaemonConfig { addr: tasks_addr }, state.clone());

            if storage_port == auth_port {
                tokio::try_join!(daemon, tasks, functions)?;
            } else {
                let storage_addr = parse_addr("storage", &host, storage_port)?;
                let storage = server::serve_storage_with_state(
                    DaemonConfig { addr: storage_addr },
                    state.clone(),
                );
                tokio::try_join!(daemon, storage, tasks, functions)?;
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

async fn attach_worker(
    project: String,
    workdir: PathBuf,
    daemon_host: String,
    daemon_port: u16,
    functions_host: String,
    functions_port: u16,
    filters: Vec<String>,
) -> anyhow::Result<()> {
    let workdir = std::fs::canonicalize(&workdir).unwrap_or(workdir);
    let daemon_url = daemon_base_url(&daemon_host, daemon_port);
    let request = AttachRequest {
        project_id: project,
        workdir: workdir.display().to_string(),
        functions_host,
        functions_port,
        filters,
    };

    let attachment = reqwest::Client::new()
        .post(format!("{daemon_url}/__/control/attachments"))
        .json(&request)
        .send()
        .await
        .with_context(|| format!("failed to reach firelite daemon at {daemon_url}"))?
        .error_for_status()
        .context("firelite daemon rejected attach request")?
        .json::<firelite::control::FunctionAttachment>()
        .await
        .context("failed to parse attach response")?;

    println!(
        "attached functions worker: id={} project={} workdir={} functions={}:{} filters={}",
        attachment.id,
        attachment.project_id,
        attachment.workdir,
        attachment.functions_host,
        attachment.functions_port,
        render_filters(&attachment.filters)
    );

    Ok(())
}

async fn list_attachments(daemon_host: String, daemon_port: u16) -> anyhow::Result<()> {
    let daemon_url = daemon_base_url(&daemon_host, daemon_port);
    let response = reqwest::Client::new()
        .get(format!("{daemon_url}/__/control/attachments"))
        .send()
        .await
        .with_context(|| format!("failed to reach firelite daemon at {daemon_url}"))?
        .error_for_status()
        .context("firelite daemon rejected attachments request")?
        .json::<AttachmentsResponse>()
        .await
        .context("failed to parse attachments response")?;

    if response.attachments.is_empty() {
        println!("no attached functions workers");
        return Ok(());
    }

    for attachment in response.attachments {
        println!(
            "{} project={} workdir={} functions={}:{} filters={}",
            attachment.id,
            attachment.project_id,
            attachment.workdir,
            attachment.functions_host,
            attachment.functions_port,
            render_filters(&attachment.filters)
        );
    }

    Ok(())
}

fn daemon_base_url(host: &str, port: u16) -> String {
    let host = host.trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        let authority = host.split("://").nth(1).unwrap_or(host);
        if authority.contains(':') {
            host.to_string()
        } else {
            format!("{host}:{port}")
        }
    } else {
        format!("http://{host}:{port}")
    }
}

fn render_filters(filters: &[String]) -> String {
    if filters.is_empty() {
        "all".to_string()
    } else {
        filters.join(",")
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("firelite=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();
}
