use crate::server::AppState;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_PROJECT: &str = "demo-firelite";
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/upload/storage/v1/b/:bucket/o", post(upload_gcs_object))
        .route("/storage/v1/b/:bucket/o", get(list_gcs_objects))
        .route(
            "/storage/v1/b/:bucket/o/*object",
            get(get_gcs_object).delete(delete_gcs_object),
        )
        .route(
            "/v0/b/:bucket/o",
            get(list_firebase_objects).post(upload_firebase_object),
        )
        .route(
            "/v0/b/:bucket/o/*object",
            get(get_firebase_object).delete(delete_firebase_object),
        )
        .route(
            "/emulator/v1/projects/:project_id/storage/buckets/:bucket/objects",
            get(list_emulator_objects).delete(reset_emulator_bucket),
        )
}

#[derive(Debug, Clone, Default)]
pub struct StorageState {
    projects: Arc<RwLock<HashMap<String, ProjectStorageState>>>,
}

#[derive(Debug, Clone, Default)]
struct ProjectStorageState {
    buckets: HashMap<String, BucketState>,
}

#[derive(Debug, Clone, Default)]
struct BucketState {
    objects: HashMap<String, ObjectRecord>,
}

#[derive(Debug, Clone)]
struct ObjectRecord {
    name: String,
    content_type: String,
    data: Bytes,
    generation: u64,
    created_at_ms: u64,
    updated_at_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    name: Option<String>,
    upload_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadMetadata {
    name: Option<String>,
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectQuery {
    alt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    prefix: Option<String>,
    max_results: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectMetadata {
    kind: &'static str,
    bucket: String,
    name: String,
    id: String,
    generation: String,
    metageneration: String,
    size: String,
    content_type: String,
    time_created: String,
    updated: String,
}

#[derive(Debug, Serialize)]
struct ListObjectsResponse {
    kind: &'static str,
    items: Vec<ObjectMetadata>,
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
    message: &'static str,
    status: &'static str,
}

#[derive(Debug)]
struct StorageError {
    status: StatusCode,
    message: &'static str,
}

impl IntoResponse for StorageError {
    fn into_response(self) -> Response {
        let status = self.status;
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: status.as_u16(),
                message: self.message,
                status: status.canonical_reason().unwrap_or("ERROR"),
            },
        };
        (status, Json(body)).into_response()
    }
}

type StorageResult<T> = Result<Json<T>, StorageError>;

async fn upload_gcs_object(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> StorageResult<ObjectMetadata> {
    if matches!(query.upload_type.as_deref(), Some(value) if value != "media" && value != "multipart")
    {
        return Err(error(StatusCode::BAD_REQUEST, "UNSUPPORTED_UPLOAD_TYPE"));
    }
    let upload = parse_upload(query.name, headers, body)?;
    upload_object(&state, &infer_project_id(&bucket), &bucket, upload)
}

async fn upload_firebase_object(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> StorageResult<ObjectMetadata> {
    let upload = parse_upload(query.name, headers, body)?;
    upload_object(&state, &infer_project_id(&bucket), &bucket, upload)
}

async fn list_gcs_objects(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<ListQuery>,
) -> StorageResult<ListObjectsResponse> {
    list_objects(&state, &infer_project_id(&bucket), &bucket, query)
}

async fn list_firebase_objects(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<ListQuery>,
) -> StorageResult<ListObjectsResponse> {
    list_objects(&state, &infer_project_id(&bucket), &bucket, query)
}

async fn list_emulator_objects(
    State(state): State<SharedState>,
    Path((project_id, bucket)): Path<(String, String)>,
    Query(query): Query<ListQuery>,
) -> StorageResult<ListObjectsResponse> {
    list_objects(&state, &project_id, &bucket, query)
}

async fn get_gcs_object(
    State(state): State<SharedState>,
    Path((bucket, object)): Path<(String, String)>,
    Query(query): Query<ObjectQuery>,
) -> Result<Response, StorageError> {
    get_object(&state, &infer_project_id(&bucket), &bucket, &object, query)
}

async fn get_firebase_object(
    State(state): State<SharedState>,
    Path((bucket, object)): Path<(String, String)>,
    Query(query): Query<ObjectQuery>,
) -> Result<Response, StorageError> {
    get_object(&state, &infer_project_id(&bucket), &bucket, &object, query)
}

async fn delete_gcs_object(
    State(state): State<SharedState>,
    Path((bucket, object)): Path<(String, String)>,
) -> StorageResult<EmptyResponse> {
    delete_object(&state, &infer_project_id(&bucket), &bucket, &object)
}

async fn delete_firebase_object(
    State(state): State<SharedState>,
    Path((bucket, object)): Path<(String, String)>,
) -> StorageResult<EmptyResponse> {
    delete_object(&state, &infer_project_id(&bucket), &bucket, &object)
}

async fn reset_emulator_bucket(
    State(state): State<SharedState>,
    Path((project_id, bucket)): Path<(String, String)>,
) -> StorageResult<EmptyResponse> {
    let mut projects = state
        .storage
        .projects
        .write()
        .expect("storage state poisoned");
    if let Some(project) = projects.get_mut(&project_id) {
        project.buckets.remove(&bucket);
    }
    Ok(Json(EmptyResponse {}))
}

fn upload_object(
    state: &SharedState,
    project_id: &str,
    bucket: &str,
    upload: ParsedUpload,
) -> StorageResult<ObjectMetadata> {
    if upload.name.is_empty() {
        return Err(error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"));
    }

    let now = now_ms();
    let mut projects = state
        .storage
        .projects
        .write()
        .expect("storage state poisoned");
    let bucket_state = projects
        .entry(project_id.to_string())
        .or_default()
        .buckets
        .entry(bucket.to_string())
        .or_default();
    let generation = bucket_state
        .objects
        .get(&upload.name)
        .map(|object| object.generation + 1)
        .unwrap_or(1);
    let created_at_ms = bucket_state
        .objects
        .get(&upload.name)
        .map(|object| object.created_at_ms)
        .unwrap_or(now);
    let record = ObjectRecord {
        name: upload.name.clone(),
        content_type: upload.content_type,
        data: upload.data,
        generation,
        created_at_ms,
        updated_at_ms: now,
    };
    let metadata = metadata(bucket, &record);
    bucket_state.objects.insert(upload.name, record);

    Ok(Json(metadata))
}

struct ParsedUpload {
    name: String,
    content_type: String,
    data: Bytes,
}

fn parse_upload(
    query_name: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<ParsedUpload, StorageError> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or(DEFAULT_CONTENT_TYPE)
        .to_string();

    if !content_type.starts_with("multipart/") {
        return Ok(ParsedUpload {
            name: query_name
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"))?,
            content_type,
            data: body,
        });
    }

    let boundary = content_type
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("boundary="))
        .map(|value| value.trim_matches('"'))
        .next()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MULTIPART_UPLOAD"))?;
    let parts = parse_multipart_body(&body, boundary)?;
    if parts.len() < 2 {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_MULTIPART_UPLOAD"));
    }

    let metadata: UploadMetadata = serde_json::from_slice(&parts[0].body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_MULTIPART_UPLOAD"))?;
    let media = &parts[1];
    let name = query_name
        .or(metadata.name)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"))?;
    let media_content_type = metadata
        .content_type
        .or_else(|| media.content_type.clone())
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());

    Ok(ParsedUpload {
        name,
        content_type: media_content_type,
        data: Bytes::from(media.body.clone()),
    })
}

#[derive(Debug)]
struct MultipartPart {
    content_type: Option<String>,
    body: Vec<u8>,
}

fn parse_multipart_body(body: &[u8], boundary: &str) -> Result<Vec<MultipartPart>, StorageError> {
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();

    let body_text = String::from_utf8_lossy(body);
    for section in body_text.split(&delimiter).skip(1) {
        let section = section.trim_start_matches("\r\n");
        if section.is_empty() || section.starts_with("--") {
            continue;
        }
        let section = section.trim_end_matches("\r\n");
        let Some((raw_headers, raw_body)) = section.split_once("\r\n\r\n") else {
            return Err(error(StatusCode::BAD_REQUEST, "INVALID_MULTIPART_UPLOAD"));
        };
        let content_type = raw_headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-type")
                .then(|| value.trim().to_string())
        });
        parts.push(MultipartPart {
            content_type,
            body: raw_body.as_bytes().to_vec(),
        });
    }

    Ok(parts)
}

fn list_objects(
    state: &SharedState,
    project_id: &str,
    bucket: &str,
    query: ListQuery,
) -> StorageResult<ListObjectsResponse> {
    let projects = state
        .storage
        .projects
        .read()
        .expect("storage state poisoned");
    let max_results = query.max_results.unwrap_or(1000);
    let mut items = projects
        .get(project_id)
        .and_then(|project| project.buckets.get(bucket))
        .map(|bucket_state| {
            let mut objects = bucket_state
                .objects
                .values()
                .filter(|object| {
                    query
                        .prefix
                        .as_deref()
                        .map(|prefix| object.name.starts_with(prefix))
                        .unwrap_or(true)
                })
                .map(|object| metadata(bucket, object))
                .collect::<Vec<_>>();
            objects.sort_by(|left, right| left.name.cmp(&right.name));
            objects.truncate(max_results);
            objects
        })
        .unwrap_or_default();
    items.shrink_to_fit();

    Ok(Json(ListObjectsResponse {
        kind: "storage#objects",
        items,
    }))
}

fn get_object(
    state: &SharedState,
    project_id: &str,
    bucket: &str,
    object: &str,
    query: ObjectQuery,
) -> Result<Response, StorageError> {
    let projects = state
        .storage
        .projects
        .read()
        .expect("storage state poisoned");
    let record = projects
        .get(project_id)
        .and_then(|project| project.buckets.get(bucket))
        .and_then(|bucket_state| bucket_state.objects.get(object))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "OBJECT_NOT_FOUND"))?
        .clone();

    if query.alt.as_deref() == Some("media") {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&record.content_type)
                .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_CONTENT_TYPE)),
        );
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&record.data.len().to_string())
                .expect("content length is a valid header value"),
        );
        Ok((headers, record.data).into_response())
    } else {
        Ok(Json(metadata(bucket, &record)).into_response())
    }
}

fn delete_object(
    state: &SharedState,
    project_id: &str,
    bucket: &str,
    object: &str,
) -> StorageResult<EmptyResponse> {
    let mut projects = state
        .storage
        .projects
        .write()
        .expect("storage state poisoned");
    let removed = projects
        .get_mut(project_id)
        .and_then(|project| project.buckets.get_mut(bucket))
        .and_then(|bucket_state| bucket_state.objects.remove(object));

    if removed.is_none() {
        return Err(error(StatusCode::NOT_FOUND, "OBJECT_NOT_FOUND"));
    }

    Ok(Json(EmptyResponse {}))
}

fn metadata(bucket: &str, object: &ObjectRecord) -> ObjectMetadata {
    ObjectMetadata {
        kind: "storage#object",
        bucket: bucket.to_string(),
        name: object.name.clone(),
        id: format!("{bucket}/{}/{}", object.name, object.generation),
        generation: object.generation.to_string(),
        metageneration: "1".to_string(),
        size: object.data.len().to_string(),
        content_type: object.content_type.clone(),
        time_created: object.created_at_ms.to_string(),
        updated: object.updated_at_ms.to_string(),
    }
}

fn infer_project_id(bucket: &str) -> String {
    bucket
        .strip_suffix(".appspot.com")
        .or_else(|| bucket.strip_suffix(".firebasestorage.app"))
        .unwrap_or(DEFAULT_PROJECT)
        .to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as u64
}

fn error(status: StatusCode, message: &'static str) -> StorageError {
    StorageError { status, message }
}
