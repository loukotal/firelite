use crate::{
    control::find_attachment_for_function, functions::is_hop_by_hop_header, server::AppState,
};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::info;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/projects/:project_id/locations/:location_id/queues/:queue_id/tasks",
            post(create_task).get(list_tasks),
        )
        .route(
            "/projects/:project_id/locations/:location_id/queues/:queue_id/tasks/:task_id",
            get(get_task).delete(delete_task),
        )
}

#[derive(Debug, Clone, Default)]
pub struct TasksState {
    tasks: Arc<RwLock<BTreeMap<String, TaskRecord>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskRecord {
    task: Task,
    queue_name: String,
    status: TaskStatus,
    created_at_ms: u64,
    dispatched_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
enum TaskStatus {
    Created,
    Succeeded,
    Failed,
    Deleted,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTaskRequest {
    task: Task,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    name: Option<String>,
    #[serde(default)]
    http_request: HttpRequest,
    schedule_time: Option<serde_json::Value>,
    dispatch_deadline: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpRequest {
    http_method: Option<String>,
    url: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Option<String>,
    oidc_token: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListTasksResponse {
    tasks: Vec<Task>,
}

#[derive(Debug, Serialize)]
struct EmptyResponse {}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: u16,
    message: String,
    status: String,
}

#[derive(Debug)]
struct TasksError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for TasksError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.status.as_u16(),
                message: self.message,
                status: self
                    .status
                    .canonical_reason()
                    .unwrap_or("ERROR")
                    .to_string(),
            },
        };
        (self.status, Json(body)).into_response()
    }
}

type TasksResult<T> = Result<Json<T>, TasksError>;

async fn create_task(
    State(state): State<SharedState>,
    Path((project_id, location_id, queue_id)): Path<(String, String, String)>,
    Json(payload): Json<CreateTaskRequest>,
) -> TasksResult<Task> {
    let queue_name = queue_name(&project_id, &location_id, &queue_id);
    let mut task = payload.task;
    let name = task
        .name
        .clone()
        .unwrap_or_else(|| format!("{queue_name}/tasks/{}", uuid::Uuid::new_v4()));
    task.name = Some(name.clone());

    {
        let mut tasks = state.tasks.tasks.write().expect("tasks state poisoned");
        if tasks.contains_key(&name) {
            return Err(error(StatusCode::CONFLICT, "ALREADY_EXISTS"));
        }
        tasks.insert(
            name.clone(),
            TaskRecord {
                task: task.clone(),
                queue_name: queue_name.clone(),
                status: TaskStatus::Created,
                created_at_ms: now_ms(),
                dispatched_at_ms: None,
            },
        );
    }

    match dispatch_task(&state, &project_id, &location_id, &queue_id, &task).await {
        Ok(()) => update_task_status(&state, &name, TaskStatus::Succeeded),
        Err(error) => {
            update_task_status(&state, &name, TaskStatus::Failed);
            return Err(error);
        }
    }

    Ok(Json(task))
}

async fn list_tasks(
    State(state): State<SharedState>,
    Path((project_id, location_id, queue_id)): Path<(String, String, String)>,
) -> Json<ListTasksResponse> {
    let queue_name = queue_name(&project_id, &location_id, &queue_id);
    let tasks = state
        .tasks
        .tasks
        .read()
        .expect("tasks state poisoned")
        .values()
        .filter(|record| {
            record.queue_name == queue_name && !matches!(record.status, TaskStatus::Deleted)
        })
        .map(|record| record.task.clone())
        .collect();

    Json(ListTasksResponse { tasks })
}

async fn get_task(
    State(state): State<SharedState>,
    Path((project_id, location_id, queue_id, task_id)): Path<(String, String, String, String)>,
) -> TasksResult<Task> {
    let name = format!(
        "{}/tasks/{task_id}",
        queue_name(&project_id, &location_id, &queue_id)
    );
    let task = state
        .tasks
        .tasks
        .read()
        .expect("tasks state poisoned")
        .get(&name)
        .filter(|record| !matches!(record.status, TaskStatus::Deleted))
        .map(|record| record.task.clone())
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "NOT_FOUND"))?;

    Ok(Json(task))
}

async fn delete_task(
    State(state): State<SharedState>,
    Path((project_id, location_id, queue_id, task_id)): Path<(String, String, String, String)>,
) -> TasksResult<EmptyResponse> {
    let name = format!(
        "{}/tasks/{task_id}",
        queue_name(&project_id, &location_id, &queue_id)
    );
    let mut tasks = state.tasks.tasks.write().expect("tasks state poisoned");
    let record = tasks
        .get_mut(&name)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "NOT_FOUND"))?;
    record.status = TaskStatus::Deleted;

    Ok(Json(EmptyResponse {}))
}

async fn dispatch_task(
    state: &SharedState,
    project_id: &str,
    location_id: &str,
    queue_id: &str,
    task: &Task,
) -> Result<(), TasksError> {
    let target = match task
        .http_request
        .url
        .as_deref()
        .filter(|url| !url.is_empty())
    {
        Some(url) => url.to_string(),
        None => {
            let attachment = find_attachment_for_function(state, project_id, queue_id)
                .ok_or_else(|| error(StatusCode::NOT_FOUND, "NO_ATTACHED_FUNCTIONS_WORKER"))?;
            format!(
                "http://{}:{}/{}/{}/{}",
                attachment.functions_host,
                attachment.functions_port,
                project_id,
                location_id,
                queue_id
            )
        }
    };
    let method = task
        .http_request
        .http_method
        .as_deref()
        .unwrap_or("POST")
        .parse::<reqwest::Method>()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_HTTP_METHOD"))?;
    let body = decode_body(task.http_request.body.as_deref())?;
    let task_name = task.name.as_deref().unwrap_or("");
    let queue_name = queue_name(project_id, location_id, queue_id);

    let mut request = state.http_client.request(method, target.clone()).body(body);
    for (name, value) in &task.http_request.headers {
        if is_hop_by_hop_header(name) {
            continue;
        }
        request = request.header(name, value);
    }
    request = request
        .header("X-CloudTasks-QueueName", queue_name)
        .header("X-CloudTasks-TaskName", task_name)
        .header("X-CloudTasks-TaskRetryCount", "0")
        .header("X-CloudTasks-TaskExecutionCount", "0");

    let response = request
        .send()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "TASK_DISPATCH_FAILED"))?;
    let status = response.status();
    if !status.is_success() {
        let response_body = response.text().await.unwrap_or_default();
        let detail = response_body.trim();
        let message = if detail.is_empty() {
            format!("TASK_DISPATCH_HTTP_{}", status.as_u16())
        } else {
            format!(
                "TASK_DISPATCH_HTTP_{}: {}",
                status.as_u16(),
                truncate_error_detail(detail)
            )
        };
        return Err(error(StatusCode::BAD_GATEWAY, message));
    }

    info!(task = %task_name, target = %target, "dispatched cloud task");
    Ok(())
}

fn decode_body(body: Option<&str>) -> Result<Bytes, TasksError> {
    let Some(body) = body else {
        return Ok(Bytes::new());
    };
    BASE64
        .decode(body)
        .map(Bytes::from)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_TASK_BODY"))
}

fn update_task_status(state: &SharedState, name: &str, status: TaskStatus) {
    if let Some(record) = state
        .tasks
        .tasks
        .write()
        .expect("tasks state poisoned")
        .get_mut(name)
    {
        record.status = status;
        record.dispatched_at_ms = Some(now_ms());
    }
}

fn queue_name(project_id: &str, location_id: &str, queue_id: &str) -> String {
    format!("projects/{project_id}/locations/{location_id}/queues/{queue_id}")
}

fn error(status: StatusCode, message: impl Into<String>) -> TasksError {
    TasksError {
        status,
        message: message.into(),
    }
}

fn truncate_error_detail(detail: &str) -> String {
    const MAX_LEN: usize = 800;
    if detail.len() <= MAX_LEN {
        return detail.to_string();
    }

    let mut end = MAX_LEN;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &detail[..end])
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}
