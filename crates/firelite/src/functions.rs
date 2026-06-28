use anyhow::{anyhow, Context};
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::{Child, Command},
    sync::{mpsc, RwLock},
    time::{sleep, Duration},
};
use tracing::{error, event, info, warn, Level};

#[derive(Debug, Clone)]
pub struct FunctionsConfig {
    pub project_id: String,
    pub source_dir: PathBuf,
    pub addr: SocketAddr,
    pub build_command: Option<String>,
    pub filters: Vec<String>,
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
    let listener = TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("failed to bind functions emulator {}", config.addr))?;
    let bound_addr = listener.local_addr().context("missing listener addr")?;
    let state = Arc::new(FunctionsState {
        project_id: config.project_id.clone(),
        active: Arc::new(RwLock::new(None)),
        client: reqwest::Client::new(),
    });
    let (reload_tx, reload_rx) = mpsc::channel(8);

    tokio::spawn(supervise_workers(
        config.project_id.clone(),
        config.source_dir.clone(),
        config.build_command.clone(),
        config.filters.clone(),
        state.active.clone(),
        reload_rx,
    ));
    tokio::spawn(watch_source(config.source_dir.clone(), reload_tx.clone()));
    reload_tx
        .send(())
        .await
        .context("failed to queue initial functions load")?;

    info!(
        addr = %bound_addr,
        project = %config.project_id,
        source = %config.source_dir.display(),
        "firelite functions emulator listening"
    );

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("functions emulator stopped unexpectedly")
}

fn app(state: Arc<FunctionsState>) -> Router {
    Router::new()
        .route("/*path", any(proxy_request))
        .with_state(state)
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
    let mut request = state
        .client
        .request(reqwest_method, target)
        .body(body.to_vec());
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
    build_command: Option<String>,
    filters: Vec<String>,
    active: Arc<RwLock<Option<ActiveWorker>>>,
    mut reload_rx: mpsc::Receiver<()>,
) {
    let mut generation = 0;
    let mut current: Option<Child> = None;

    while reload_rx.recv().await.is_some() {
        while reload_rx.try_recv().is_ok() {}
        generation += 1;

        if let Err(error) =
            run_build_command(build_command.as_deref(), &source_dir, generation).await
        {
            error!(%error, generation, "failed to build functions source");
            continue;
        }

        match start_worker(&project_id, &source_dir, &filters, generation).await {
            Ok(started) => {
                let names = started
                    .active
                    .http_functions
                    .keys()
                    .map(|key| format!("{}/{}", key.region, key.name))
                    .collect::<Vec<_>>();
                info!(
                    generation,
                    registered_functions = started.active.functions.len(),
                    functions = ?names,
                    "loaded functions worker"
                );
                let old = current.replace(started.child);
                *active.write().await = Some(started.active);
                if let Some(mut child) = old {
                    if let Err(error) = child.kill().await {
                        warn!(%error, "failed to stop previous functions worker");
                    }
                }
            }
            Err(error) => {
                error!(%error, generation, "failed to load functions worker");
            }
        }
    }
}

async fn run_build_command(
    build_command: Option<&str>,
    source_dir: &Path,
    generation: u64,
) -> anyhow::Result<()> {
    let Some(build_command) = build_command else {
        return Ok(());
    };

    info!(generation, command = %build_command, "building functions source");
    let output = Command::new("sh")
        .arg("-lc")
        .arg(build_command)
        .current_dir(source_dir)
        .output()
        .await
        .with_context(|| format!("failed to run build command `{build_command}`"))?;

    if !output.stdout.is_empty() {
        info!(
            generation,
            output = %String::from_utf8_lossy(&output.stdout),
            "functions build stdout"
        );
    }
    if !output.stderr.is_empty() {
        warn!(
            generation,
            output = %String::from_utf8_lossy(&output.stderr),
            "functions build stderr"
        );
    }

    if !output.status.success() {
        return Err(anyhow!(
            "build command `{build_command}` exited with {}",
            output.status
        ));
    }

    Ok(())
}

async fn start_worker(
    project_id: &str,
    source_dir: &Path,
    filters: &[String],
    generation: u64,
) -> anyhow::Result<StartedWorker> {
    let worker_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/functions_worker.cjs");
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
        tokio::spawn(log_worker_stderr(stderr, generation));
    }

    let mut lines = BufReader::new(stdout).lines();
    let line = lines
        .next_line()
        .await
        .context("failed to read worker ready message")?
        .ok_or_else(|| anyhow!("worker exited before ready message"))?;
    tokio::spawn(log_worker_stdout(lines, generation));

    let message: WorkerMessage =
        serde_json::from_str(&line).with_context(|| format!("invalid worker message: {line}"))?;
    let (port, descriptors) = match message {
        WorkerMessage::Ready { port, functions } => (port, functions),
        WorkerMessage::Error { message } => return Err(anyhow!(message)),
    };
    let descriptors = filter_descriptors(descriptors, filters);
    let http_functions = descriptors
        .iter()
        .cloned()
        .into_iter()
        .filter(|descriptor| matches!(descriptor.trigger, TriggerKind::Https { .. }))
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

async fn log_worker_stdout(
    mut lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    generation: u64,
) {
    while let Ok(Some(line)) = lines.next_line().await {
        log_worker_line("stdout", generation, &line, Level::INFO);
    }
}

async fn log_worker_stderr(stderr: tokio::process::ChildStderr, generation: u64) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        log_worker_line("stderr", generation, &line, Level::WARN);
    }
}

fn log_worker_line(source: &'static str, generation: u64, line: &str, fallback_level: Level) {
    let formatted = format_worker_log_line(line, fallback_level);
    let message = format!(
        "[functions worker:{source} generation={generation}] {}",
        formatted.message
    );

    match formatted.level {
        Level::ERROR => event!(Level::ERROR, "{}", message),
        Level::WARN => event!(Level::WARN, "{}", message),
        Level::INFO => event!(Level::INFO, "{}", message),
        Level::DEBUG => event!(Level::DEBUG, "{}", message),
        Level::TRACE => event!(Level::TRACE, "{}", message),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormattedWorkerLog {
    level: Level,
    message: String,
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
            message: line,
        };
    };

    let level = object
        .get("severity")
        .and_then(|value| value.as_str())
        .and_then(level_from_severity)
        .unwrap_or(fallback_level);

    let mut message = object
        .get("message")
        .or_else(|| object.get("textPayload"))
        .map(render_log_value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            serde_json::to_string(&serde_json::Value::Object(object.clone()))
                .unwrap_or_else(|_| line.clone())
        });

    let fields = object
        .iter()
        .filter(|(key, _)| !is_structured_log_metadata(key))
        .map(|(key, value)| format!("{key}={}", render_log_value(value)))
        .collect::<Vec<_>>();

    if !fields.is_empty() {
        message.push_str(" | ");
        message.push_str(&fields.join(" "));
    }

    FormattedWorkerLog { level, message }
}

fn format_text_worker_log_line(line: &str, fallback_level: Level) -> Option<FormattedWorkerLog> {
    let rest = line.strip_prefix('[')?;
    let (_, rest) = rest.split_once("] ")?;
    let (severity, message) = rest.split_once(": ")?;
    let level = level_from_severity(severity).unwrap_or(fallback_level);

    Some(FormattedWorkerLog {
        level,
        message: message.to_string(),
    })
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
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => "null".to_string(),
        value => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

async fn watch_source(source_dir: PathBuf, reload_tx: mpsc::Sender<()>) {
    let mut previous = scan_source(&source_dir);

    loop {
        sleep(Duration::from_millis(500)).await;
        let current = scan_source(&source_dir);
        if current != previous {
            previous = current;
            sleep(Duration::from_millis(150)).await;
            if reload_tx.send(()).await.is_err() {
                break;
            }
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

    #[test]
    fn watches_typescript_inputs() {
        assert!(should_watch_file(Path::new("index.ts")));
        assert!(should_watch_file(Path::new("src/index.tsx")));
        assert!(should_watch_file(Path::new("lib/index.js")));
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
            }
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
            }
        );
    }

    #[tokio::test]
    async fn build_command_can_generate_loadable_javascript() {
        if !node_can_start_loopback_server().await {
            return;
        }

        let dir = std::env::temp_dir().join(format!("firelite-functions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("index.ts"), "// pretend TypeScript source\n");

        run_build_command(
            Some(
                r#"printf '%s\n' 'exports.api = (req, res) => res.end("built");' 'exports.api.__trigger = { name: "api", regions: ["us-central1"], httpsTrigger: {} };' > index.js"#,
            ),
            &dir,
            1,
        )
        .await
        .unwrap();

        let mut worker = start_worker("demo-firelite", &dir, &[], 1).await.unwrap();
        assert!(worker.active.http_functions.contains_key(&FunctionKey {
            region: "us-central1".to_string(),
            name: "api".to_string(),
        }));
        worker.child.kill().await.unwrap();
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
