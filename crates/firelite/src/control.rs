use crate::{
    functions::{function_id_matches_filter, is_hop_by_hop_header, parse_function_route},
    server::AppState,
};
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
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
        .route("/*path", any(proxy_attached_function))
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
    Ok(Json(register_attachment(&state, payload)?))
}

pub fn register_attachment(
    state: &SharedState,
    payload: AttachRequest,
) -> Result<FunctionAttachment, StatusCode> {
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

    Ok(attachment)
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

async fn proxy_attached_function(
    State(state): State<SharedState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match proxy_attached_function_inner(state, uri.path(), uri.query(), method, headers, body).await
    {
        Ok(response) => response,
        Err(response) => response,
    }
}

async fn proxy_attached_function_inner(
    state: SharedState,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Response> {
    let route = parse_function_route(path).ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    let attachment = find_attachment_for_function(&state, &route.project_id, &route.name)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "no attached functions worker for route",
            )
                .into_response()
        })?;

    let mut target = format!(
        "http://{}:{}{}",
        attachment.functions_host, attachment.functions_port, path
    );
    if let Some(query) = query {
        target.push('?');
        target.push_str(query);
    }

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid method").into_response())?;
    let mut request = state
        .http_client
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

    let proxied = request.send().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            "attached functions worker request failed",
        )
            .into_response()
    })?;
    let status = proxied.status();
    let response_headers = proxied.headers().clone();
    let response_body = proxied.bytes().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            "attached functions worker response failed",
        )
            .into_response()
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

pub(crate) fn find_attachment_for_function(
    state: &SharedState,
    project_id: &str,
    function_name: &str,
) -> Option<FunctionAttachment> {
    state
        .attachments
        .read()
        .expect("attachments state poisoned")
        .values()
        .find(|attachment| {
            attachment.project_id == project_id
                && (attachment.filters.is_empty()
                    || attachment
                        .filters
                        .iter()
                        .any(|filter| function_id_matches_filter(function_name, filter)))
        })
        .cloned()
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
