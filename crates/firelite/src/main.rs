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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    match Cli::parse().command {
        Command::Daemon { host, port } => {
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .with_context(|| format!("invalid daemon address {host}:{port}"))?;
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
        } => {
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .with_context(|| format!("invalid functions address {host}:{port}"))?;
            firelite::functions::serve(FunctionsConfig {
                project_id: project,
                source_dir: watch,
                addr,
            })
            .await
        }
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
