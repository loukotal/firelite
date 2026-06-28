use crate::{auth, config::DaemonConfig, control, storage, web_ui};
use anyhow::Context;
use axum::{
    extract::Request,
    http::{
        header::{
            ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        },
        HeaderValue, Method, StatusCode,
    },
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::net::TcpListener;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub auth: auth::AuthState,
    pub storage: storage::StorageState,
    pub attachments: Arc<std::sync::RwLock<BTreeMap<String, control::FunctionAttachment>>>,
    pub http_client: reqwest::Client,
}

pub fn app_state() -> Arc<AppState> {
    Arc::new(AppState {
        auth: auth::AuthState::default(),
        storage: storage::StorageState::default(),
        attachments: Arc::new(std::sync::RwLock::new(BTreeMap::new())),
        http_client: reqwest::Client::new(),
    })
}

pub fn app() -> Router {
    app_with_state(app_state())
}

pub fn app_with_state(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/__/health", get(health))
        .route("/__/ui", get(web_ui::console))
        .merge(control::router())
        .merge(auth::router())
        .merge(storage::router())
        .fallback(fallback)
        .layer(middleware::from_fn(add_cors_headers))
        .with_state(state)
}

pub fn storage_app_with_state(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/__/health", get(health))
        .merge(storage::router())
        .fallback(fallback)
        .layer(middleware::from_fn(add_cors_headers))
        .with_state(state)
}

pub async fn serve(config: DaemonConfig) -> anyhow::Result<()> {
    serve_router("firelite daemon", config, app()).await
}

pub async fn serve_with_state(
    name: &'static str,
    config: DaemonConfig,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    serve_router(name, config, app_with_state(state)).await
}

pub async fn serve_storage_with_state(
    config: DaemonConfig,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    serve_router(
        "firelite storage emulator",
        config,
        storage_app_with_state(state),
    )
    .await
}

async fn serve_router(
    name: &'static str,
    config: DaemonConfig,
    router: Router,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("failed to bind {}", config.addr))?;

    info!(addr = %config.addr, "{name} listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .with_context(|| format!("{name} stopped unexpectedly"))
}

async fn root() -> &'static str {
    "firelite"
}

async fn health() -> &'static str {
    "ok"
}

async fn fallback(method: Method) -> StatusCode {
    if method == Method::OPTIONS {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn add_cors_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PUT,PATCH,DELETE,OPTIONS"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(
            "authorization,content-type,x-client-version,x-firebase-appcheck,x-firebase-client,x-firebase-client-log-type,x-firebase-gmpid,x-firebase-locale,x-firebase-storage-version,x-goog-api-client,x-goog-upload-command,x-goog-upload-header-content-length,x-goog-upload-header-content-type,x-goog-upload-protocol,x-goog-user-project",
        ),
    );
    response
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
