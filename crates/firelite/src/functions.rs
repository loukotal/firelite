use anyhow::{anyhow, Context};
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    io::IsTerminal,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::{Child, Command},
    sync::{mpsc, RwLock},
    time::{sleep, timeout, Duration},
};
use tracing::{error, event, info, warn, Level};

#[derive(Debug, Clone)]
pub struct FunctionsConfig {
    pub project_id: String,
    pub source_dir: PathBuf,
    pub addr: SocketAddr,
    pub filters: Vec<String>,
    pub reload_on_change: bool,
}

pub struct PreparedFunctions {
    config: FunctionsConfig,
    listener: TcpListener,
    state: Arc<FunctionsState>,
    started: StartedWorker,
}

#[derive(Debug, Clone)]
struct FunctionsState {
    project_id: String,
    active: Arc<RwLock<Option<ActiveWorker>>>,
    client: reqwest::Client,
}

#[derive(Debug, Clone)]
struct ActiveWorker {
    generation: u64,
    base_url: String,
    functions: Vec<FunctionDescriptor>,
    http_functions: HashMap<FunctionKey, FunctionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FunctionKey {
    region: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDescriptor {
    pub entry_id: String,
    pub name: String,
    pub region: String,
    pub trigger: TriggerKind,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TriggerKind {
    Https {
        callable: bool,
    },
    Schedule {
        schedule: Option<serde_json::Value>,
        #[serde(rename = "timeZone")]
        time_zone: Option<String>,
        #[serde(rename = "retryConfig")]
        retry_config: Option<serde_json::Value>,
        topic: Option<String>,
    },
    Event {
        event_type: Option<String>,
        resource: Option<serde_json::Value>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WorkerMessage {
    Ready {
        port: u16,
        functions: Vec<FunctionDescriptor>,
    },
    Error {
        message: String,
    },
}

struct StartedWorker {
    child: Child,
    active: ActiveWorker,
}

pub async fn serve(config: FunctionsConfig) -> anyhow::Result<()> {
    prepare(config).await?.serve().await
}

pub async fn prepare(config: FunctionsConfig) -> anyhow::Result<PreparedFunctions> {
    let started = start_worker(&config.project_id, &config.source_dir, &config.filters, 1)
        .await
        .context("failed to load initial functions worker")?;
    log_loaded_worker(&started.active);

    let listener = TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("failed to bind functions emulator {}", config.addr))?;
    let state = Arc::new(FunctionsState {
        project_id: config.project_id.clone(),
        active: Arc::new(RwLock::new(Some(started.active.clone()))),
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to configure functions HTTP client")?,
    });

    Ok(PreparedFunctions {
        config,
        listener,
        state,
        started,
    })
}

impl PreparedFunctions {
    pub async fn serve(self) -> anyhow::Result<()> {
        let Self {
            config,
            listener,
            state,
            started,
        } = self;
        let bound_addr = listener.local_addr().context("missing listener addr")?;
        let (reload_tx, reload_rx) = mpsc::channel(8);

        let supervisor = tokio::spawn(supervise_workers(
            config.project_id.clone(),
            config.source_dir.clone(),
            config.filters.clone(),
            state.active.clone(),
            reload_rx,
            started.child,
            1,
        ));
        let (watcher, reload_guard) = if config.reload_on_change {
            (
                Some(tokio::spawn(watch_source(
                    config.source_dir.clone(),
                    reload_tx,
                ))),
                None,
            )
        } else {
            (None, Some(reload_tx))
        };

        info!(
            addr = %bound_addr,
            project = %config.project_id,
            source = %config.source_dir.display(),
            reload_on_change = config.reload_on_change,
            "firelite functions emulator listening"
        );

        let result = axum::serve(listener, app(state))
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("functions emulator stopped unexpectedly");

        drop(reload_guard);
        if let Some(watcher) = watcher {
            watcher.abort();
        }
        supervisor.abort();
        let _ = supervisor.await;

        result
    }
}

fn app(state: Arc<FunctionsState>) -> Router {
    Router::new()
        .route("/__/health", get(functions_health))
        .route("/*path", any(proxy_request))
        .with_state(state)
}

async fn functions_health(State(state): State<Arc<FunctionsState>>) -> Response {
    if state.active.read().await.is_some() {
        (StatusCode::OK, "ok").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "functions worker unavailable",
        )
            .into_response()
    }
}

async fn proxy_request(
    State(state): State<Arc<FunctionsState>>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match proxy_request_inner(state, uri.path(), uri.query(), method, headers, body).await {
        Ok(response) => response,
        Err(response) => response,
    }
}

async fn proxy_request_inner(
    state: Arc<FunctionsState>,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Response> {
    let route = parse_function_route(path).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "expected /{project}/{region}/{functionName}",
        )
            .into_response()
    })?;

    if route.project_id != state.project_id {
        return Err((StatusCode::NOT_FOUND, "project not served by this worker").into_response());
    }

    let active = state.active.read().await.clone().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "functions worker is still loading",
        )
            .into_response()
    })?;
    let key = FunctionKey {
        region: route.region,
        name: route.name,
    };
    let descriptor = active
        .http_functions
        .get(&key)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "function not found").into_response())?;

    let mut target = format!(
        "{}/__firelite__/invoke/{}{}",
        active.base_url,
        percent_encode_path_segment(&descriptor.entry_id),
        route.suffix
    );
    if let Some(query) = query {
        target.push('?');
        target.push_str(query);
    }

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid method").into_response())?;
    let mut request = state.client.request(reqwest_method, target).body(body);
    for (name, value) in headers.iter() {
        if name.as_str().eq_ignore_ascii_case("host")
            || name.as_str().eq_ignore_ascii_case("content-length")
        {
            continue;
        }
        request = request.header(name, value);
    }

    let proxied = request.send().await.map_err(|error| {
        error!(%error, generation = active.generation, "function worker request failed");
        (StatusCode::BAD_GATEWAY, "function worker request failed").into_response()
    })?;
    let status = proxied.status();
    let response_headers = proxied.headers().clone();
    let response_body = proxied.bytes().await.map_err(|error| {
        error!(%error, generation = active.generation, "function worker response failed");
        (StatusCode::BAD_GATEWAY, "function worker response failed").into_response()
    })?;

    let mut builder = Response::builder().status(status);
    for (name, value) in response_headers.iter() {
        if is_hop_by_hop_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(axum::body::Body::from(response_body))
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid function response",
            )
                .into_response()
        })
}

pub(crate) fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[derive(Debug)]
pub(crate) struct ParsedRoute {
    pub(crate) project_id: String,
    pub(crate) region: String,
    pub(crate) name: String,
    pub(crate) suffix: String,
}

pub(crate) fn parse_function_route(path: &str) -> Option<ParsedRoute> {
    let mut parts = path.trim_start_matches('/').split('/');
    let project_id = parts.next()?.to_string();
    let region = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    if project_id.is_empty() || region.is_empty() || name.is_empty() {
        return None;
    }

    let rest = parts.collect::<Vec<_>>();
    let suffix = if rest.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", rest.join("/"))
    };

    Some(ParsedRoute {
        project_id,
        region,
        name,
        suffix,
    })
}

async fn supervise_workers(
    project_id: String,
    source_dir: PathBuf,
    filters: Vec<String>,
    active: Arc<RwLock<Option<ActiveWorker>>>,
    mut reload_rx: mpsc::Receiver<()>,
    mut current: Child,
    mut generation: u64,
) {
    let mut worker_running = true;
    let mut reload_channel_open = true;
    let mut restart_delay = Duration::from_millis(250);

    loop {
        let reload_requested = if worker_running {
            tokio::select! {
                reload = reload_rx.recv(), if reload_channel_open => {
                    if reload.is_none() {
                        reload_channel_open = false;
                        continue;
                    }
                    true
                }
                status = current.wait() => {
                    match status {
                        Ok(status) => warn!(%status, generation, "functions worker exited"),
                        Err(error) => error!(%error, generation, "failed to wait for functions worker"),
                    }
                    *active.write().await = None;
                    worker_running = false;
                    false
                }
            }
        } else {
            tokio::select! {
                reload = reload_rx.recv(), if reload_channel_open => {
                    if reload.is_none() {
                        reload_channel_open = false;
                        continue;
                    }
                    true
                }
                _ = sleep(restart_delay) => false,
            }
        };

        if reload_requested {
            while reload_rx.try_recv().is_ok() {}
        }
        generation += 1;

        match start_worker(&project_id, &source_dir, &filters, generation).await {
            Ok(started) => {
                log_loaded_worker(&started.active);
                let old = std::mem::replace(&mut current, started.child);
                *active.write().await = Some(started.active);
                if worker_running {
                    let mut child = old;
                    if let Err(error) = child.kill().await {
                        warn!(%error, "failed to stop previous functions worker");
                    }
                }
                worker_running = true;
                restart_delay = Duration::from_millis(250);
            }
            Err(error) => {
                if worker_running {
                    error!(%error, generation, "failed to reload functions worker; keeping previous worker");
                } else {
                    error!(%error, generation, "failed to restart functions worker; retrying");
                    restart_delay = restart_delay.saturating_mul(2).min(Duration::from_secs(5));
                }
            }
        }
    }
}

async fn start_worker(
    project_id: &str,
    source_dir: &Path,
    filters: &[String],
    generation: u64,
) -> anyhow::Result<StartedWorker> {
    let worker_path = materialize_functions_worker().await?;
    let mut child = Command::new("node")
        .arg(worker_path)
        .arg(source_dir)
        .arg(project_id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| "failed to start Node functions worker")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("missing worker stdout"))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(log_worker_stderr(stderr));
    }

    let mut lines = BufReader::new(stdout).lines();
    let line = timeout(Duration::from_secs(15), lines.next_line())
        .await
        .context("timed out waiting for worker ready message")?
        .context("failed to read worker ready message")?
        .ok_or_else(|| anyhow!("worker exited before ready message"))?;
    tokio::spawn(log_worker_stdout(lines));

    let message: WorkerMessage =
        serde_json::from_str(&line).with_context(|| format!("invalid worker message: {line}"))?;
    let (port, descriptors) = match message {
        WorkerMessage::Ready { port, functions } => (port, functions),
        WorkerMessage::Error { message } => return Err(anyhow!(message)),
    };
    let descriptors = filter_descriptors(descriptors, filters);
    let http_functions = descriptors
        .iter()
        .filter(|descriptor| matches!(descriptor.trigger, TriggerKind::Https { .. }))
        .cloned()
        .map(|descriptor| {
            (
                FunctionKey {
                    region: descriptor.region.clone(),
                    name: descriptor.name.clone(),
                },
                descriptor,
            )
        })
        .collect();

    Ok(StartedWorker {
        child,
        active: ActiveWorker {
            generation,
            base_url: format!("http://127.0.0.1:{port}"),
            functions: descriptors,
            http_functions,
        },
    })
}

fn log_loaded_worker(active: &ActiveWorker) {
    let names = active
        .http_functions
        .keys()
        .map(|key| format!("{}/{}", key.region, key.name))
        .collect::<Vec<_>>();
    info!(
        generation = active.generation,
        registered_functions = active.functions.len(),
        functions = ?names,
        "loaded functions worker"
    );
}

const FUNCTIONS_WORKER: &[u8] = include_bytes!("functions_worker.cjs");

async fn materialize_functions_worker() -> anyhow::Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(FUNCTIONS_WORKER);
    let hash = format!("{:x}", hasher.finalize());
    let worker_dir = std::env::temp_dir().join("firelite");
    let worker_path = worker_dir.join(format!(
        "functions_worker-{}-{}.cjs",
        env!("CARGO_PKG_VERSION"),
        &hash[..16]
    ));

    tokio::fs::create_dir_all(&worker_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create worker asset directory {}",
                worker_dir.display()
            )
        })?;

    let needs_write = match tokio::fs::read(&worker_path).await {
        Ok(existing) => existing != FUNCTIONS_WORKER,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read worker asset {}", worker_path.display()));
        }
    };
    if needs_write {
        tokio::fs::write(&worker_path, FUNCTIONS_WORKER)
            .await
            .with_context(|| format!("failed to write worker asset {}", worker_path.display()))?;
    }

    Ok(worker_path)
}

fn filter_descriptors(
    descriptors: Vec<FunctionDescriptor>,
    filters: &[String],
) -> Vec<FunctionDescriptor> {
    if filters.is_empty() {
        return descriptors;
    }

    descriptors
        .into_iter()
        .filter(|descriptor| {
            filters
                .iter()
                .any(|filter| descriptor_matches_filter(descriptor, filter))
        })
        .collect()
}

fn descriptor_matches_filter(descriptor: &FunctionDescriptor, filter: &str) -> bool {
    function_id_matches_filter(&descriptor.entry_id, filter)
        || function_id_matches_filter(&descriptor.name, filter)
}

pub(crate) fn function_id_matches_filter(value: &str, filter: &str) -> bool {
    value == filter
        || value
            .strip_prefix(filter)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('/'))
}

async fn log_worker_stdout(lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) {
    log_worker_stream(lines, Level::INFO).await;
}

async fn log_worker_stderr(stderr: tokio::process::ChildStderr) {
    log_worker_stream(BufReader::new(stderr).lines(), Level::WARN).await;
}

async fn log_worker_stream<R>(mut lines: tokio::io::Lines<R>, fallback_level: Level)
where
    R: AsyncBufRead + Unpin,
{
    let mut pending = None::<String>;

    loop {
        if pending.is_none() {
            let Ok(Some(line)) = lines.next_line().await else {
                break;
            };
            pending = Some(line);
            continue;
        }

        tokio::select! {
            result = lines.next_line() => {
                match result {
                    Ok(Some(line)) if is_worker_record_continuation(&line) => {
                        let record = pending.as_mut().expect("pending worker record");
                        record.push('\n');
                        record.push_str(&line);
                    }
                    Ok(Some(line)) => {
                        flush_worker_record(&mut pending, fallback_level);
                        pending = Some(line);
                    }
                    _ => {
                        flush_worker_record(&mut pending, fallback_level);
                        break;
                    }
                }
            }
            _ = sleep(Duration::from_millis(20)) => {
                flush_worker_record(&mut pending, fallback_level);
            }
        }
    }
}

fn is_worker_record_continuation(line: &str) -> bool {
    line.chars().next().is_none_or(char::is_whitespace)
}

fn flush_worker_record(pending: &mut Option<String>, fallback_level: Level) {
    if let Some(record) = pending.take() {
        let record = expand_escaped_whitespace(record);
        log_worker_line(&compact_worker_record(&record), fallback_level);
    }
}

fn compact_worker_record(record: &str) -> String {
    let heading = record.lines().next().unwrap_or_default().trim();
    let fields = record
        .lines()
        .skip(1)
        .filter_map(parse_worker_record_field)
        .collect::<Vec<_>>();
    if !fields
        .iter()
        .any(|(key, _)| is_request_context_metadata(key))
    {
        return record.to_string();
    }

    let details = fields
        .into_iter()
        .filter(|(key, _)| !is_request_context_metadata(key))
        .filter_map(|(key, value)| format_request_context_field(&key, &value))
        .collect::<Vec<_>>();
    if details.is_empty() {
        heading.to_string()
    } else {
        format!("{heading} {}", details.join(" "))
    }
}

fn parse_worker_record_field(line: &str) -> Option<(String, String)> {
    let (key, value) = line.trim().trim_end_matches(',').split_once(':')?;
    let key = key.trim().trim_matches('"');
    let value = value.trim().trim_end_matches(',').trim_matches('"');
    (!key.is_empty() && !value.is_empty()).then(|| (key.to_string(), value.to_string()))
}

fn is_request_context_metadata(key: &str) -> bool {
    matches!(key, "executionId" | "sessionId" | "userId")
}

fn format_request_context_field(key: &str, value: &str) -> Option<String> {
    if value == "{" || value == "}" {
        return None;
    }

    Some(match key {
        "durationMillis" => format!("duration={value}ms"),
        "statusCode" => format!("status={value}"),
        _ => format!("{key}={value}"),
    })
}

fn log_worker_line(line: &str, fallback_level: Level) {
    let formatted = format_worker_log_line(line, fallback_level);
    let message = decorate_worker_message(
        &formatted.message,
        formatted.level,
        formatted.structured,
        colors_enabled(),
    );

    match formatted.level {
        Level::ERROR => {
            event!(target: "firelite_worker", Level::ERROR, worker_message = message.as_str())
        }
        Level::WARN => {
            event!(target: "firelite_worker", Level::WARN, worker_message = message.as_str())
        }
        Level::INFO => {
            event!(target: "firelite_worker", Level::INFO, worker_message = message.as_str())
        }
        Level::DEBUG => {
            event!(target: "firelite_worker", Level::DEBUG, worker_message = message.as_str())
        }
        Level::TRACE => {
            event!(target: "firelite_worker", Level::TRACE, worker_message = message.as_str())
        }
    }
}

fn colors_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let forced = std::env::var("FORCE_COLOR")
            .ok()
            .is_some_and(|value| value != "0");
        forced
            || (std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").ok().as_deref() != Some("dumb")
                && (std::io::stderr().is_terminal() || std::env::var_os("HERDR_ENV").is_some()))
    })
}

fn decorate_worker_message(message: &str, level: Level, structured: bool, enabled: bool) -> String {
    if !enabled {
        return message.to_string();
    }

    if structured {
        return colorize_structured_log(message, level);
    }

    let mut decorated = colorize_http_method(message);
    colorize_status_field(&mut decorated);
    decorated
}

fn colorize_structured_log(message: &str, level: Level) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
        let mut output = String::new();
        write_colored_json(&value, 0, &mut output);
        return output;
    }

    let Some((summary, fields)) = message.split_once(" | ") else {
        return colorize_log_summary(message, level);
    };
    let summary = colorize_log_summary(summary, level);
    let fields = fields
        .split(' ')
        .map(|field| {
            let Some((key, value)) = field.split_once('=') else {
                return field.to_string();
            };
            format!("\u{1b}[36m{key}\u{1b}[0m=\u{1b}[32m{value}\u{1b}[0m")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{summary} \u{1b}[2m|\u{1b}[0m {fields}")
}

fn colorize_log_summary(message: &str, level: Level) -> String {
    let color = match level {
        Level::ERROR => "1;31",
        Level::WARN => "1;33",
        Level::DEBUG | Level::TRACE => "2",
        Level::INFO => return message.to_string(),
    };
    format!("\u{1b}[{color}m{message}\u{1b}[0m")
}

fn write_colored_json(value: &serde_json::Value, indent: usize, output: &mut String) {
    const RESET: &str = "\u{1b}[0m";
    const PUNCTUATION: &str = "\u{1b}[2m";
    match value {
        serde_json::Value::Object(object) => {
            output.push_str(PUNCTUATION);
            output.push('{');
            output.push_str(RESET);
            if !object.is_empty() {
                output.push('\n');
            }
            for (index, (key, value)) in object.iter().enumerate() {
                output.push_str(&" ".repeat(indent + 2));
                output.push_str("\u{1b}[36m");
                output
                    .push_str(&serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\"")));
                output.push_str(RESET);
                output.push_str(PUNCTUATION);
                output.push_str(": ");
                output.push_str(RESET);
                write_colored_json(value, indent + 2, output);
                if index + 1 != object.len() {
                    output.push_str(PUNCTUATION);
                    output.push(',');
                    output.push_str(RESET);
                }
                output.push('\n');
            }
            if !object.is_empty() {
                output.push_str(&" ".repeat(indent));
            }
            output.push_str(PUNCTUATION);
            output.push('}');
            output.push_str(RESET);
        }
        serde_json::Value::Array(values) => {
            output.push_str(PUNCTUATION);
            output.push('[');
            output.push_str(RESET);
            if !values.is_empty() {
                output.push('\n');
            }
            for (index, value) in values.iter().enumerate() {
                output.push_str(&" ".repeat(indent + 2));
                write_colored_json(value, indent + 2, output);
                if index + 1 != values.len() {
                    output.push_str(PUNCTUATION);
                    output.push(',');
                    output.push_str(RESET);
                }
                output.push('\n');
            }
            if !values.is_empty() {
                output.push_str(&" ".repeat(indent));
            }
            output.push_str(PUNCTUATION);
            output.push(']');
            output.push_str(RESET);
        }
        serde_json::Value::String(value) => {
            output.push_str("\u{1b}[32m");
            output
                .push_str(&serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\"")));
            output.push_str(RESET);
        }
        serde_json::Value::Number(value) => {
            output.push_str("\u{1b}[33m");
            output.push_str(&value.to_string());
            output.push_str(RESET);
        }
        serde_json::Value::Bool(value) => {
            output.push_str("\u{1b}[35m");
            output.push_str(if *value { "true" } else { "false" });
            output.push_str(RESET);
        }
        serde_json::Value::Null => output.push_str("\u{1b}[2mnull\u{1b}[0m"),
    }
}

fn colorize_http_method(message: &str) -> String {
    let Some((method, rest)) = message.split_once(' ') else {
        return message.to_string();
    };
    let color = match method {
        "GET" | "HEAD" => "36",
        "POST" => "32",
        "PUT" => "33",
        "PATCH" => "35",
        "DELETE" => "31",
        "OPTIONS" => "34",
        _ => return message.to_string(),
    };
    format!("\u{1b}[1;{color}m{method}\u{1b}[0m {rest}")
}

fn colorize_status_field(message: &mut String) {
    let Some(start) = message.find("status=") else {
        return;
    };
    let value_start = start + "status=".len();
    let value_end = message[value_start..]
        .find(|ch: char| !ch.is_ascii_digit())
        .map_or(message.len(), |offset| value_start + offset);
    let status = &message[value_start..value_end];
    let color = match status.as_bytes().first() {
        Some(b'2' | b'3') => "32",
        Some(b'4') => "33",
        Some(b'5') => "31",
        _ => return,
    };
    let replacement = format!("\u{1b}[{color}m{status}\u{1b}[0m");
    message.replace_range(value_start..value_end, &replacement);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormattedWorkerLog {
    level: Level,
    message: String,
    structured: bool,
}

fn format_worker_log_line(line: &str, fallback_level: Level) -> FormattedWorkerLog {
    let line = strip_ansi_sequences(line);
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<serde_json::Value>(&line)
    else {
        if let Some(formatted) = format_text_worker_log_line(&line, fallback_level) {
            return formatted;
        }

        return FormattedWorkerLog {
            level: fallback_level,
            message: expand_escaped_whitespace(line),
            structured: false,
        };
    };

    let level = object
        .get("severity")
        .and_then(|value| value.as_str())
        .and_then(level_from_severity)
        .unwrap_or(fallback_level);

    let primary_message = object.get("message").or_else(|| object.get("textPayload"));
    let mut message = primary_message
        .map(render_log_value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            serde_json::to_string_pretty(&serde_json::Value::Object(object.clone()))
                .unwrap_or_else(|_| line.clone())
        });

    let fields = if primary_message.is_some() {
        object
            .iter()
            .filter(|(key, _)| !is_structured_log_metadata(key))
            .map(|(key, value)| format!("{key}={}", render_log_value(value)))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if !fields.is_empty() {
        message.push_str(" | ");
        message.push_str(&fields.join(" "));
    }

    FormattedWorkerLog {
        level,
        message,
        structured: true,
    }
}

fn format_text_worker_log_line(line: &str, fallback_level: Level) -> Option<FormattedWorkerLog> {
    let rest = line.strip_prefix('[')?;
    let (_, rest) = rest.split_once("] ")?;
    let (severity, message) = rest.split_once(": ")?;
    let level = level_from_severity(severity).unwrap_or(fallback_level);

    Some(FormattedWorkerLog {
        level,
        message: expand_escaped_whitespace(message.to_string()),
        structured: false,
    })
}

fn expand_escaped_whitespace(value: String) -> String {
    value.replace("\\n", "\n").replace("\\t", "\t")
}

fn strip_ansi_sequences(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        let is_real_escape = ch == '\u{1b}';
        let is_escaped_escape = ch == '\\' && chars.peek() == Some(&'x');

        if is_real_escape || is_escaped_escape {
            if is_escaped_escape {
                let mut clone = chars.clone();
                if clone.next() != Some('x')
                    || clone.next() != Some('1')
                    || clone.next() != Some('b')
                    || clone.next() != Some('[')
                {
                    output.push(ch);
                    continue;
                }
                chars.next();
                chars.next();
                chars.next();
            }

            if is_real_escape && chars.peek() != Some(&'[') {
                continue;
            }
            if chars.peek() == Some(&'[') {
                chars.next();
                for code in chars.by_ref() {
                    if code.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }

        output.push(ch);
    }

    output
}

fn level_from_severity(severity: &str) -> Option<Level> {
    match severity.to_ascii_uppercase().as_str() {
        "DEBUG" => Some(Level::DEBUG),
        "INFO" | "NOTICE" => Some(Level::INFO),
        "WARNING" | "WARN" => Some(Level::WARN),
        "ERROR" | "CRITICAL" | "ALERT" | "EMERGENCY" => Some(Level::ERROR),
        "TRACE" => Some(Level::TRACE),
        _ => None,
    }
}

fn is_structured_log_metadata(key: &str) -> bool {
    matches!(
        key,
        "severity"
            | "message"
            | "textPayload"
            | "time"
            | "timestamp"
            | "logging.googleapis.com/labels"
            | "logging.googleapis.com/sourceLocation"
            | "logging.googleapis.com/trace"
    )
}

fn render_log_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => expand_escaped_whitespace(value.clone()),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => "null".to_string(),
        value => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

async fn watch_source(source_dir: PathBuf, reload_tx: mpsc::Sender<()>) {
    let Some(mut previous) = scan_source_async(&source_dir).await else {
        return;
    };

    loop {
        sleep(Duration::from_millis(500)).await;
        let Some(current) = scan_source_async(&source_dir).await else {
            return;
        };
        if current != previous {
            previous = current;
            sleep(Duration::from_millis(150)).await;
            if reload_tx.send(()).await.is_err() {
                break;
            }
        }
    }
}

async fn scan_source_async(source_dir: &Path) -> Option<BTreeMap<PathBuf, SystemTime>> {
    let source_dir = source_dir.to_path_buf();
    match tokio::task::spawn_blocking(move || scan_source(&source_dir)).await {
        Ok(files) => Some(files),
        Err(error) => {
            error!(%error, "functions source watcher failed");
            None
        }
    }
}

fn scan_source(source_dir: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let mut files = BTreeMap::new();
    scan_dir(source_dir, source_dir, &mut files);
    files
}

fn scan_dir(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, SystemTime>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        if metadata.is_dir() {
            if should_ignore_dir(&path) {
                continue;
            }
            scan_dir(root, &path, files);
            continue;
        }

        if !metadata.is_file() || !should_watch_file(&path) {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.insert(relative, modified);
    }
}

fn should_ignore_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("node_modules" | ".git" | ".firebase" | "coverage")
    )
}

fn should_watch_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("js" | "cjs" | "mjs" | "json" | "ts" | "tsx")
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_function_route_with_suffix() {
        let route = parse_function_route("/demo-firelite/us-central1/api/users/1").unwrap();

        assert_eq!(route.project_id, "demo-firelite");
        assert_eq!(route.region, "us-central1");
        assert_eq!(route.name, "api");
        assert_eq!(route.suffix, "/users/1");
    }

    #[tokio::test]
    async fn starts_worker_and_discovers_http_function() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.api = (req, res) => res.end("ok");
exports.api.__trigger = {
  name: "api",
  regions: ["us-central1"],
  httpsTrigger: {}
};
exports.circular = {};
exports.circular.self = exports.circular;
"#,
        );

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        assert!(worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "api".to_string(),
        }));
        worker.child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn filters_registered_functions_by_export_or_name() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.api = (req, res) => res.end("api");
exports.api.__trigger = {
  name: "api",
  regions: ["us-central1"],
  httpsTrigger: {}
};
exports.admin = (req, res) => res.end("admin");
exports.admin.__trigger = {
  name: "admin",
  regions: ["us-central1"],
  httpsTrigger: {}
};
exports.nested = {
  api: (req, res) => res.end("nested")
};
exports.nested.api.__trigger = {
  name: "nestedApi",
  regions: ["us-central1"],
  httpsTrigger: {}
};
"#,
        );

        let filters = vec!["api".to_string()];
        let mut worker = start_worker("demo-firelite", &dir, &filters, 1)
            .await
            .unwrap();
        assert!(worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "api".to_string(),
        }));
        assert!(!worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "admin".to_string(),
        }));
        assert!(!worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "nestedApi".to_string(),
        }));
        worker.child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn proxies_get_and_post_requests_to_http_function() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.api = (req, res) => {
  let body = "";
  req.on("data", chunk => body += chunk);
  req.on("end", () => {
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({
      method: req.method,
      url: req.url,
      body,
      header: req.headers["x-firelite-test"] || null
    }));
  });
};
exports.api.__trigger = {
  name: "api",
  regions: ["us-central1"],
  httpsTrigger: {}
};
"#,
        );

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        let state = Arc::new(FunctionsState {
            project_id: "demo-firelite".to_string(),
            active: Arc::new(RwLock::new(Some(worker.active.clone()))),
            client: reqwest::Client::new(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });

        let client = reqwest::Client::new();
        let health = client
            .get(format!("{base_url}/__/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let got: serde_json::Value = client
            .get(format!(
                "{base_url}/demo-firelite/us-central1/api/users/1?debug=true"
            ))
            .header("x-firelite-test", "get-header")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(got["method"], "GET");
        assert_eq!(got["url"], "/users/1?debug=true");
        assert_eq!(got["header"], "get-header");

        let posted: serde_json::Value = client
            .post(format!("{base_url}/demo-firelite/us-central1/api"))
            .header("x-firelite-test", "post-header")
            .body(r#"{"ok":true}"#)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(posted["method"], "POST");
        assert_eq!(posted["url"], "/");
        assert_eq!(posted["body"], r#"{"ok":true}"#);
        assert_eq!(posted["header"], "post-header");

        worker.child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn restarts_worker_after_unexpected_exit() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
const fs = require("node:fs");
const path = require("node:path");
const marker = path.join(__dirname, ".crashed-once");
if (!fs.existsSync(marker)) {
  fs.writeFileSync(marker, "yes");
  setTimeout(() => process.exit(17), 50);
}
exports.api = (req, res) => res.end("ok");
exports.api.__trigger = {
  name: "api",
  regions: ["us-central1"],
  httpsTrigger: {}
};
"#,
        );

        let started = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        let active = Arc::new(RwLock::new(Some(started.active)));
        let (reload_tx, reload_rx) = mpsc::channel(1);
        let supervisor = tokio::spawn(supervise_workers(
            "demo-firelite".to_string(),
            dir,
            Vec::new(),
            active.clone(),
            reload_rx,
            started.child,
            1,
        ));

        timeout(Duration::from_secs(3), async {
            loop {
                if active
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|worker| worker.generation >= 2)
                {
                    break;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("worker should restart");

        drop(reload_tx);
        supervisor.abort();
        let _ = supervisor.await;
    }

    #[tokio::test]
    async fn discovers_scheduled_functions_without_routing_them_as_http() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.cleanup = () => {};
exports.cleanup.__trigger = {
  name: "cleanup",
  regions: ["us-central1"],
  eventTrigger: {
    eventType: "google.pubsub.topic.publish",
    resource: "projects/demo-firelite/topics/firebase-schedule-cleanup-us-central1"
  }
};
exports.cleanup.__schedule = {
  schedule: "every 5 minutes",
  timeZone: "UTC"
};
"#,
        );

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        let scheduled = worker
            .active
            .functions
            .iter()
            .find(|descriptor| descriptor.name == "cleanup")
            .unwrap();
        match &scheduled.trigger {
            TriggerKind::Schedule {
                schedule,
                time_zone,
                topic,
                ..
            } => {
                assert_eq!(
                    schedule.as_ref().and_then(|value| value.as_str()),
                    Some("every 5 minutes")
                );
                assert_eq!(time_zone.as_deref(), Some("UTC"));
                assert_eq!(
                    topic.as_deref(),
                    Some("projects/demo-firelite/topics/firebase-schedule-cleanup-us-central1")
                );
            }
            other => panic!("expected schedule trigger, got {other:?}"),
        }
        assert!(!worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "cleanup".to_string(),
        }));
        worker.child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn discovers_task_queue_functions_as_http_routes() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.jobs = {
  run: (req, res) => res.end("task")
};
exports.jobs.run.__trigger = {
  platform: "gcfv2",
  regions: ["us-central1"],
  taskQueueTrigger: {}
};
exports.jobs.run.__endpoint = {
  platform: "gcfv2",
  region: ["us-central1"],
  taskQueueTrigger: {}
};
"#,
        );

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        assert!(worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "jobs-run".to_string(),
        }));
        worker.child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn invokes_task_queue_functions_with_parsed_json_body() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir.join("index.js"),
            r#"
exports.tasks = {
  runJob: (req, res) => {
    if (
      req.body &&
      req.body.data &&
      req.body.data.jobTaskId === "task-1" &&
      req.header("content-type").includes("application/json")
    ) {
      res.status(204).send();
      return;
    }
    res.status(400).send({ body: req.body || null });
  }
};
exports.tasks.runJob.__trigger = {
  platform: "gcfv2",
  regions: ["us-central1"],
  taskQueueTrigger: {}
};
exports.tasks.runJob.__endpoint = {
  platform: "gcfv2",
  region: ["us-central1"],
  taskQueueTrigger: {}
};
"#,
        );

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        let state = Arc::new(FunctionsState {
            project_id: "demo-firelite".to_string(),
            active: Arc::new(RwLock::new(Some(worker.active.clone()))),
            client: reqwest::Client::new(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });

        reqwest::Client::new()
            .post(format!("{base_url}/demo-firelite/us-central1/tasks-runJob"))
            .header("content-type", "application/json")
            .body(r#"{"data":{"jobTaskId":"task-1"}}"#)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        worker.child.kill().await.unwrap();
    }

    #[test]
    fn watches_typescript_inputs() {
        assert!(should_watch_file(Path::new("index.ts")));
        assert!(should_watch_file(Path::new("src/index.tsx")));
        assert!(should_watch_file(Path::new("lib/index.js")));
    }

    #[tokio::test]
    async fn materializes_embedded_worker_outside_cargo_source_dir() {
        let worker_path = materialize_functions_worker().await.unwrap();
        assert!(worker_path.exists());
        assert_eq!(std::fs::read(&worker_path).unwrap(), FUNCTIONS_WORKER);
        assert!(!worker_path.starts_with(env!("CARGO_MANIFEST_DIR")));
    }

    #[test]
    fn formats_firebase_structured_logs_for_terminal_output() {
        let formatted = format_worker_log_line(
            r#"{"severity":"INFO","message":"created user","uid":"alice","attempt":2}"#,
            Level::WARN,
        );

        assert_eq!(
            formatted,
            FormattedWorkerLog {
                level: Level::INFO,
                message: "created user | attempt=2 uid=alice".to_string(),
                structured: true,
            }
        );
    }

    #[test]
    fn preserves_plain_worker_output() {
        let formatted = format_worker_log_line("plain stderr line", Level::WARN);

        assert_eq!(
            formatted,
            FormattedWorkerLog {
                level: Level::WARN,
                message: "plain stderr line".to_string(),
                structured: false,
            }
        );
    }

    #[test]
    fn expands_escaped_stack_trace_lines() {
        let formatted = format_worker_log_line(
            r#"Error: failed\n    at handler (index.js:10:2)\n    at processTask (index.js:20:4)"#,
            Level::ERROR,
        );

        assert_eq!(
            formatted.message,
            "Error: failed\n    at handler (index.js:10:2)\n    at processTask (index.js:20:4)"
        );
    }

    #[test]
    fn pretty_prints_structured_logs_without_a_message_field() {
        let formatted = format_worker_log_line(
            r#"{"executionId":"request-1","data":{"durationMillis":1,"statusCode":400}}"#,
            Level::INFO,
        );

        assert_eq!(
            formatted.message,
            "{\n  \"data\": {\n    \"durationMillis\": 1,\n    \"statusCode\": 400\n  },\n  \"executionId\": \"request-1\"\n}"
        );
    }

    #[test]
    fn compacts_trpc_request_context() {
        let record = r#"trpc endpoint success
  executionId: "emulatorRequest:abc",
  sessionId: "session-1",
  userId: "user-1",
  data: {
    "path": "admin.userRent.listUserRents",
    "type": "mutation",
    "durationMillis": 348
  }
}"#;

        assert_eq!(
            compact_worker_record(record),
            "trpc endpoint success path=admin.userRent.listUserRents type=mutation duration=348ms"
        );
    }

    #[test]
    fn compacts_request_context_with_escaped_newlines() {
        let record = expand_escaped_whitespace(
            r#"trpc endpoint success\n  executionId: "request-1",\n  data: {\n    "path": "admin.users.list",\n    "durationMillis": 12\n  }"#.to_string(),
        );

        assert_eq!(
            compact_worker_record(&record),
            "trpc endpoint success path=admin.users.list duration=12ms"
        );
    }

    #[test]
    fn compacts_response_context() {
        let record = r#"Response
  executionId: "emulatorRequest:abc",
  sessionId: "session-1",
  userId: "user-1",
  data: {
    "durationMillis": 406,
    "endpoint": "/admin.userRent.listUserRents",
    "statusCode": 200
  }
}"#;

        assert_eq!(
            compact_worker_record(record),
            "Response duration=406ms endpoint=/admin.userRent.listUserRents status=200"
        );
    }

    #[test]
    fn colors_http_methods_and_statuses_for_terminals() {
        assert_eq!(
            decorate_worker_message("GET /api/users 200 - 4ms", Level::INFO, false, true),
            "\u{1b}[1;36mGET\u{1b}[0m /api/users 200 - 4ms"
        );
        assert_eq!(
            decorate_worker_message("POST /api/users 201 - 8ms", Level::INFO, false, true),
            "\u{1b}[1;32mPOST\u{1b}[0m /api/users 201 - 8ms"
        );
        assert_eq!(
            decorate_worker_message(
                "Response endpoint=/api/users status=500",
                Level::INFO,
                false,
                true,
            ),
            "Response endpoint=/api/users status=\u{1b}[31m500\u{1b}[0m"
        );
    }

    #[test]
    fn syntax_highlights_structured_json_logs() {
        let formatted = format_worker_log_line(
            r#"{"severity":"WARN","data":{"ok":false,"attempt":2},"name":"worker"}"#,
            Level::INFO,
        );
        let decorated = decorate_worker_message(
            &formatted.message,
            formatted.level,
            formatted.structured,
            true,
        );

        assert!(decorated.contains("\u{1b}[36m\"data\"\u{1b}[0m"));
        assert!(decorated.contains("\u{1b}[35mfalse\u{1b}[0m"));
        assert!(decorated.contains("\u{1b}[33m2\u{1b}[0m"));
        assert!(decorated.contains("\u{1b}[32m\"worker\"\u{1b}[0m"));
    }

    #[test]
    fn keeps_redirected_logs_free_of_ansi_codes() {
        assert_eq!(
            decorate_worker_message("DELETE /api/users/1 204 - 3ms", Level::INFO, false, false),
            "DELETE /api/users/1 204 - 3ms"
        );
    }

    #[test]
    fn strips_ansi_sequences_from_worker_output() {
        let formatted = format_worker_log_line(
            r#"[11:26:20] \x1b[34mDEBUG\x1b[39m: \x1b[36mUpdating user\x1b[39m"#,
            Level::INFO,
        );

        assert_eq!(
            formatted,
            FormattedWorkerLog {
                level: Level::DEBUG,
                message: "Updating user".to_string(),
                structured: false,
            }
        );
    }

    fn write_file(path: &Path, contents: &str) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    async fn node_can_start_loopback_server() -> bool {
        let script = r#"
const http = require("node:http");
const server = http.createServer((req, res) => res.end("ok"));
server.on("error", () => process.exit(1));
server.listen(0, "127.0.0.1", () => server.close(() => process.exit(0)));
"#;

        Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}
