use crate::server::AppState;
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/__/control/attachments",
            get(list_attachments).post(attach_worker),
        )
        .route(
            "/emulator/v1/attachments",
            get(list_attachments).post(attach_worker),
        )
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionAttachment {
    pub id: String,
    pub project_id: String,
    pub workdir: String,
    pub functions_host: String,
    pub functions_port: u16,
    pub filters: Vec<String>,
    pub attached_at_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRequest {
    pub project_id: String,
    pub workdir: String,
    pub functions_host: String,
    pub functions_port: u16,
    #[serde(default)]
    pub filters: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentsResponse {
    pub attachments: Vec<FunctionAttachment>,
}

async fn attach_worker(
    State(state): State<SharedState>,
    Json(payload): Json<AttachRequest>,
) -> Result<Json<FunctionAttachment>, StatusCode> {
    if payload.project_id.trim().is_empty()
        || payload.workdir.trim().is_empty()
        || payload.functions_host.trim().is_empty()
        || payload.functions_port == 0
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let attachment = FunctionAttachment {
        id: attachment_id(
            &payload.project_id,
            &payload.functions_host,
            payload.functions_port,
        ),
        project_id: payload.project_id,
        workdir: payload.workdir,
        functions_host: payload.functions_host,
        functions_port: payload.functions_port,
        filters: payload.filters,
        attached_at_ms: now_ms(),
    };

    state
        .attachments
        .write()
        .expect("attachments state poisoned")
        .insert(attachment.id.clone(), attachment.clone());

    Ok(Json(attachment))
}

async fn list_attachments(State(state): State<SharedState>) -> Json<AttachmentsResponse> {
    let attachments = state
        .attachments
        .read()
        .expect("attachments state poisoned")
        .values()
        .cloned()
        .collect();

    Json(AttachmentsResponse { attachments })
}

fn attachment_id(project_id: &str, functions_host: &str, functions_port: u16) -> String {
    format!("{project_id}@{functions_host}:{functions_port}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}
