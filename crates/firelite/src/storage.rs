use crate::server::AppState;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const DEFAULT_PROJECT: &str = "demo-firelite";
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/upload/storage/v1/b/:bucket/o",
            post(upload_gcs_object).put(upload_gcs_resumable_chunk),
        )
        .route("/b/:bucket/o", get(list_gcs_objects))
        .route(
            "/b/:bucket/o/*object",
            get(get_gcs_object).delete(delete_gcs_object),
        )
        .route("/storage/v1/b/:bucket/o", get(list_gcs_objects))
        .route(
            "/storage/v1/b/:bucket/o/*object",
            get(get_gcs_object).delete(delete_gcs_object),
        )
        .route(
            "/v0/b/:bucket/o",
            get(list_firebase_objects)
                .post(upload_firebase_object)
                .options(storage_preflight),
        )
        .route(
            "/v0/b/:bucket/o/*object",
            get(get_firebase_object)
                .delete(delete_firebase_object)
                .options(storage_preflight),
        )
        .route(
            "/emulator/v1/projects/:project_id/storage/buckets/:bucket/objects",
            get(list_emulator_objects).delete(reset_emulator_bucket),
        )
}

#[derive(Debug, Clone, Default)]
pub struct StorageState {
    projects: Arc<RwLock<HashMap<String, ProjectStorageState>>>,
    resumable_uploads: Arc<RwLock<HashMap<String, ResumableUploadSession>>>,
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
    metadata: HashMap<String, String>,
    data: Bytes,
    generation: u64,
    created_at_ms: u64,
    updated_at_ms: u64,
}

#[derive(Debug, Clone)]
struct ResumableUploadSession {
    project_id: String,
    bucket: String,
    name: String,
    content_type: String,
    metadata: HashMap<String, String>,
    expected_length: Option<usize>,
    data: Vec<u8>,
    received: usize,
    finalized: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    name: Option<String>,
    upload_type: Option<String>,
    #[serde(alias = "upload_id")]
    upload_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadMetadata {
    name: Option<String>,
    content_type: Option<String>,
    #[serde(default)]
    metadata: HashMap<String, String>,
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

#[derive(Debug, Clone, Serialize)]
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
    metadata: HashMap<String, String>,
    crc32c: String,
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

async fn storage_preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn upload_gcs_object(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    if query.upload_type.as_deref() == Some("resumable") {
        return start_resumable_upload(&state, &bucket, query.name, headers, body);
    }
    if matches!(query.upload_type.as_deref(), Some(value) if value != "media" && value != "multipart")
    {
        return Err(error(StatusCode::BAD_REQUEST, "UNSUPPORTED_UPLOAD_TYPE"));
    }
    let upload = parse_upload(query.name, headers, body)?;
    Ok(upload_object(&state, &infer_project_id(&bucket), &bucket, upload)?.into_response())
}

async fn upload_gcs_resumable_chunk(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    if query.upload_type.as_deref() != Some("resumable") {
        return Err(error(StatusCode::BAD_REQUEST, "UNSUPPORTED_UPLOAD_TYPE"));
    }
    let upload_id = query
        .upload_id
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_UPLOAD_ID"))?;
    let session = state
        .storage
        .resumable_uploads
        .write()
        .expect("storage state poisoned")
        .remove(&upload_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "UPLOAD_SESSION_NOT_FOUND"))?;
    if session.bucket != bucket {
        return Err(error(StatusCode::NOT_FOUND, "UPLOAD_SESSION_NOT_FOUND"));
    }
    if is_resumable_status_query(&headers, &body) {
        state
            .storage
            .resumable_uploads
            .write()
            .expect("storage state poisoned")
            .insert(upload_id, session);
        return Ok(StatusCode::PERMANENT_REDIRECT.into_response());
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or(&session.content_type)
        .to_string();
    let upload = ParsedUpload {
        name: session.name,
        content_type,
        metadata: session.metadata,
        data: body,
    };
    Ok(upload_object(&state, &session.project_id, &session.bucket, upload)?.into_response())
}

async fn upload_firebase_object(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    if headers
        .get("x-goog-upload-protocol")
        .and_then(|value| value.to_str().ok())
        == Some("resumable")
    {
        return start_firebase_resumable_upload(&state, &bucket, query.name, headers, body);
    }
    if let Some(upload_id) = query.upload_id {
        return handle_firebase_resumable_upload(&state, &bucket, &upload_id, headers, body);
    }
    let upload = parse_upload(query.name, headers, body)?;
    Ok(upload_object(&state, &infer_project_id(&bucket), &bucket, upload)?.into_response())
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

fn start_resumable_upload(
    state: &SharedState,
    bucket: &str,
    query_name: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    let metadata: Option<UploadMetadata> = if body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice(&body)
                .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_RESUMABLE_METADATA"))?,
        )
    };
    let name = query_name
        .or_else(|| metadata.as_ref().and_then(|value| value.name.clone()))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"))?;
    let content_type = headers
        .get("x-upload-content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
        .or_else(|| {
            metadata
                .as_ref()
                .and_then(|value| value.content_type.clone())
        })
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
    let custom_metadata = metadata.map(|value| value.metadata).unwrap_or_default();
    let upload_id = Uuid::new_v4().to_string();
    let project_id = infer_project_id(bucket);

    state
        .storage
        .resumable_uploads
        .write()
        .expect("storage state poisoned")
        .insert(
            upload_id.clone(),
            ResumableUploadSession {
                project_id,
                bucket: bucket.to_string(),
                name,
                content_type,
                metadata: custom_metadata,
                expected_length: None,
                data: Vec::new(),
                received: 0,
                finalized: false,
            },
        );

    let location = format!(
        "{}/upload/storage/v1/b/{}/o?uploadType=resumable&upload_id={}",
        request_base_url(&headers),
        percent_encode_path_segment(bucket),
        percent_encode_query(&upload_id)
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        LOCATION,
        HeaderValue::from_str(&location)
            .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INVALID_UPLOAD_LOCATION"))?,
    );
    Ok((StatusCode::OK, response_headers, Json(EmptyResponse {})).into_response())
}

fn start_firebase_resumable_upload(
    state: &SharedState,
    bucket: &str,
    query_name: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    let metadata: Option<UploadMetadata> = if body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice(&body)
                .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_RESUMABLE_METADATA"))?,
        )
    };
    let name = query_name
        .or_else(|| metadata.as_ref().and_then(|value| value.name.clone()))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"))?;
    let content_type = headers
        .get("x-goog-upload-header-content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
        .or_else(|| {
            metadata
                .as_ref()
                .and_then(|value| value.content_type.clone())
        })
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
    let custom_metadata = metadata.map(|value| value.metadata).unwrap_or_default();
    let expected_length = headers
        .get("x-goog-upload-header-content-length")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_LENGTH"))
        })
        .transpose()?;
    let upload_id = Uuid::new_v4().to_string();
    state
        .storage
        .resumable_uploads
        .write()
        .expect("storage state poisoned")
        .insert(
            upload_id.clone(),
            ResumableUploadSession {
                project_id: infer_project_id(bucket),
                bucket: bucket.to_string(),
                name,
                content_type,
                metadata: custom_metadata,
                expected_length,
                data: Vec::new(),
                received: 0,
                finalized: false,
            },
        );

    let upload_url = format!(
        "{}/v0/b/{}/o?upload_id={}",
        request_base_url(&headers),
        percent_encode_path_segment(bucket),
        percent_encode_query(&upload_id)
    );
    let mut response_headers = HeaderMap::new();
    insert_header(&mut response_headers, "x-goog-upload-status", "active")?;
    insert_header(&mut response_headers, "x-goog-upload-url", &upload_url)?;
    Ok((StatusCode::OK, response_headers).into_response())
}

fn handle_firebase_resumable_upload(
    state: &SharedState,
    bucket: &str,
    upload_id: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StorageError> {
    let command = headers
        .get("x-goog-upload-command")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_UPLOAD_COMMAND"))?;
    let commands = command.split(',').map(str::trim).collect::<Vec<_>>();

    if commands.contains(&"query") {
        let sessions = state
            .storage
            .resumable_uploads
            .read()
            .expect("storage state poisoned");
        let session = sessions
            .get(upload_id)
            .filter(|session| session.bucket == bucket)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "UPLOAD_SESSION_NOT_FOUND"))?;
        let mut response_headers = HeaderMap::new();
        insert_header(
            &mut response_headers,
            "x-goog-upload-status",
            if session.finalized { "final" } else { "active" },
        )?;
        insert_header(
            &mut response_headers,
            "x-goog-upload-size-received",
            &session.received.to_string(),
        )?;
        return Ok((StatusCode::OK, response_headers).into_response());
    }

    let should_upload = commands.contains(&"upload");
    let should_finalize = commands.contains(&"finalize");
    if !should_upload && !should_finalize {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_COMMAND"));
    }

    let mut sessions = state
        .storage
        .resumable_uploads
        .write()
        .expect("storage state poisoned");
    let session = sessions
        .get_mut(upload_id)
        .filter(|session| session.bucket == bucket)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "UPLOAD_SESSION_NOT_FOUND"))?;
    if session.finalized {
        return Err(error(StatusCode::BAD_REQUEST, "UPLOAD_ALREADY_FINALIZED"));
    }
    let offset = headers
        .get("x-goog-upload-offset")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_UPLOAD_OFFSET"))?
        .parse::<usize>()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_OFFSET"))?;
    if offset != session.received {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_OFFSET"));
    }
    if should_upload {
        let resulting_length = session
            .received
            .checked_add(body.len())
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_LENGTH"))?;
        if session
            .expected_length
            .is_some_and(|expected| resulting_length > expected)
        {
            return Err(error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_LENGTH"));
        }
        session.data.extend_from_slice(&body);
        session.received = resulting_length;
    }

    if !should_finalize {
        let mut response_headers = HeaderMap::new();
        insert_header(&mut response_headers, "x-goog-upload-status", "active")?;
        return Ok((StatusCode::OK, response_headers).into_response());
    }
    if session
        .expected_length
        .is_some_and(|expected| session.received != expected)
    {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_UPLOAD_LENGTH"));
    }

    session.finalized = true;
    let project_id = session.project_id.clone();
    let session_bucket = session.bucket.clone();
    let name = session.name.clone();
    let content_type = session.content_type.clone();
    let custom_metadata = session.metadata.clone();
    let data = std::mem::take(&mut session.data);
    drop(sessions);
    let metadata = upload_object(
        state,
        &project_id,
        &session_bucket,
        ParsedUpload {
            name,
            content_type,
            metadata: custom_metadata,
            data: Bytes::from(data),
        },
    )?;
    let mut response_headers = HeaderMap::new();
    insert_header(&mut response_headers, "x-goog-upload-status", "final")?;
    Ok((StatusCode::OK, response_headers, metadata).into_response())
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), StorageError> {
    headers.insert(
        name,
        HeaderValue::from_str(value)
            .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INVALID_UPLOAD_HEADER"))?,
    );
    Ok(())
}

fn is_resumable_status_query(headers: &HeaderMap, body: &Bytes) -> bool {
    body.is_empty()
        && headers
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("bytes */"))
            .unwrap_or(false)
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
        metadata: upload.metadata,
        data: upload.data,
        generation,
        created_at_ms,
        updated_at_ms: now,
    };
    let metadata = metadata(bucket, &record);
    bucket_state.objects.insert(upload.name, record);
    drop(projects);
    dispatch_object_finalized(state, project_id, &metadata);

    Ok(Json(metadata))
}

struct ParsedUpload {
    name: String,
    content_type: String,
    metadata: HashMap<String, String>,
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
            metadata: HashMap::new(),
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
        .or_else(|| metadata.name.clone())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OBJECT_NAME"))?;
    let media_content_type = metadata
        .content_type
        .or_else(|| media.content_type.clone())
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());

    Ok(ParsedUpload {
        name,
        content_type: media_content_type,
        metadata: metadata.metadata,
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
        metadata: object.metadata.clone(),
        crc32c: crc32c_base64(&object.data),
        time_created: object.created_at_ms.to_string(),
        updated: object.updated_at_ms.to_string(),
    }
}

fn dispatch_object_finalized(state: &SharedState, project_id: &str, metadata: &ObjectMetadata) {
    let Some(functions) = state.functions.clone() else {
        return;
    };
    let mut attributes = HashMap::new();
    attributes.insert("bucket".to_string(), metadata.bucket.clone());
    let event_type = "google.cloud.storage.object.v1.finalized";
    let event_time = format_timestamp_ms(metadata.updated.parse().unwrap_or(0));
    let event = serde_json::json!({
        "specversion": "1.0",
        "id": Uuid::new_v4().to_string(),
        "source": format!("//storage.googleapis.com/projects/_/buckets/{}", metadata.bucket),
        "type": event_type,
        "subject": format!("objects/{}", metadata.name),
        "time": event_time,
        "data": {
            "bucket": metadata.bucket,
            "name": metadata.name,
            "generation": metadata.generation,
            "metageneration": metadata.metageneration,
            "contentType": metadata.content_type,
            "size": metadata.size,
            "crc32c": metadata.crc32c,
            "timeCreated": format_timestamp_ms(metadata.time_created.parse().unwrap_or(0)),
            "updated": format_timestamp_ms(metadata.updated.parse().unwrap_or(0)),
            "metadata": metadata.metadata,
        }
    });
    let project_id = project_id.to_string();
    tokio::spawn(async move {
        let delivered = functions
            .dispatch_event(event_type, &attributes, &event)
            .await;
        tracing::debug!(project = %project_id, delivered, "storage finalize event dispatch complete");
    });
}

fn format_timestamp_ms(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let milliseconds = timestamp_ms % 1000;
    let days = (seconds / 86_400) as i64;
    let seconds_in_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_in_day / 3_600;
    let minute = (seconds_in_day % 3_600) / 60;
    let second = seconds_in_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milliseconds:03}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u64, u64) {
    let days = days_since_epoch + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u64, day as u64)
}

fn crc32c_base64(data: &[u8]) -> String {
    STANDARD.encode(crc32c(data).to_be_bytes())
}

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
        }
    }
    !crc
}

fn infer_project_id(bucket: &str) -> String {
    bucket
        .strip_suffix(".appspot.com")
        .or_else(|| bucket.strip_suffix(".firebasestorage.app"))
        .unwrap_or(DEFAULT_PROJECT)
        .to_string()
}

fn request_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost:9199");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

fn percent_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

fn percent_encode_query(value: &str) -> String {
    percent_encode_path_segment(value)
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
