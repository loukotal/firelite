use crate::{auth, config::DaemonConfig};
use anyhow::Context;
use axum::{routing::get, Router};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub auth: auth::AuthState,
}

pub fn app() -> Router {
    let state = Arc::new(AppState {
        auth: auth::AuthState::default(),
    });

    Router::new()
        .route("/", get(root))
        .route("/__/health", get(health))
        .merge(auth::router())
        .with_state(state)
}

pub async fn serve(config: DaemonConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("failed to bind {}", config.addr))?;

    info!(addr = %config.addr, "firelite daemon listening");
    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("firelite daemon stopped unexpectedly")
}

async fn root() -> &'static str {
    "firelite"
}

async fn health() -> &'static str {
    "ok"
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
