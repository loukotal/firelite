use crate::server::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const DEFAULT_PROJECT: &str = "demo-firelite";

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/identitytoolkit.googleapis.com/v1/*action",
            post(identity_action),
        )
        .route(
            "/emulator/v1/projects/:project_id/accounts",
            get(list_accounts).delete(reset_project_accounts),
        )
        .route(
            "/emulator/v1/projects/:project_id/accounts/:local_id",
            delete(delete_emulator_account),
        )
        .route(
            "/emulator/v1/projects/:project_id/oobCodes",
            get(list_oob_codes),
        )
}

#[derive(Debug, Clone, Default)]
pub struct AuthState {
    projects: Arc<RwLock<HashMap<String, ProjectAuthState>>>,
}

#[derive(Debug, Clone, Default)]
struct ProjectAuthState {
    users_by_id: HashMap<String, UserRecord>,
    user_ids_by_email: HashMap<String, String>,
    user_ids_by_provider: HashMap<(String, String), String>,
    oob_codes: HashMap<String, OobCodeRecord>,
}

#[derive(Debug, Clone)]
struct UserRecord {
    local_id: String,
    email: String,
    password_hash: Option<String>,
    providers: Vec<ProviderRecord>,
    created_at_ms: u64,
    last_login_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct ProviderRecord {
    provider_id: String,
    raw_id: String,
    email: String,
}

#[derive(Debug, Clone)]
struct OobCodeRecord {
    email: String,
    request_type: String,
    created_at_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignUpRequest {
    email: String,
    password: String,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignInRequest {
    email: String,
    password: String,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LookupRequest {
    id_token: Option<String>,
    local_id: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRequest {
    id_token: Option<String>,
    local_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomTokenRequest {
    token: String,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdpRequest {
    post_body: String,
    request_uri: Option<String>,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendOobCodeRequest {
    request_type: String,
    email: String,
    continue_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmailLinkRequest {
    email: String,
    oob_code: String,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    kind: &'static str,
    local_id: String,
    email: String,
    id_token: String,
    refresh_token: String,
    expires_in: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OobCodeResponse {
    email: String,
    oob_code: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListOobCodesResponse {
    oob_codes: Vec<EmulatorOobCode>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorOobCode {
    email: String,
    oob_code: String,
    request_type: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct EmptyResponse {}

#[derive(Debug, Serialize)]
struct LookupResponse {
    users: Vec<EmulatorUser>,
}

#[derive(Debug, Serialize)]
struct ListAccountsResponse {
    users: Vec<EmulatorUser>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorUser {
    local_id: String,
    email: String,
    password_hash: String,
    valid_since: String,
    created_at: String,
    last_login_at: String,
    provider_user_info: Vec<ProviderInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInfo {
    provider_id: String,
    raw_id: String,
    email: String,
}

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
struct AuthError {
    status: StatusCode,
    message: &'static str,
}

impl IntoResponse for AuthError {
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

type AuthResult<T> = Result<Json<T>, AuthError>;

async fn identity_action(
    State(state): State<SharedState>,
    Path(action): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> AuthResult<serde_json::Value> {
    let body = match action.as_str() {
        "accounts:signUp" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_up(&state, payload)?)
        }
        "accounts:signInWithPassword" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_in_with_password(&state, payload)?)
        }
        "accounts:lookup" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(lookup(&state, payload)?)
        }
        "accounts:delete" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(delete_identity_account(&state, payload)?)
        }
        "accounts:signInWithCustomToken" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_in_with_custom_token(&state, payload)?)
        }
        "accounts:signInWithIdp" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_in_with_idp(&state, payload)?)
        }
        "accounts:sendOobCode" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(send_oob_code(&state, payload)?)
        }
        "accounts:signInWithEmailLink" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_in_with_email_link(&state, payload)?)
        }
        _ => return Err(error(StatusCode::NOT_FOUND, "NOT_FOUND")),
    }
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?;

    Ok(Json(body))
}

fn sign_up(state: &SharedState, payload: SignUpRequest) -> Result<AuthResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let email_key = normalize_email(&payload.email);

    if project.user_ids_by_email.contains_key(&email_key) {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_EXISTS"));
    }

    if payload.password.len() < 6 {
        return Err(error(StatusCode::BAD_REQUEST, "WEAK_PASSWORD"));
    }

    let now = now_ms();
    let local_id = Uuid::new_v4().to_string();
    let record = UserRecord {
        local_id: local_id.clone(),
        email: payload.email,
        password_hash: Some(hash_password(&payload.password)),
        providers: vec![ProviderRecord {
            provider_id: "password".to_string(),
            raw_id: local_id,
            email: email_key.clone(),
        }],
        created_at_ms: now,
        last_login_at_ms: None,
    };

    project
        .user_ids_by_email
        .insert(email_key, record.local_id.clone());
    project
        .users_by_id
        .insert(record.local_id.clone(), record.clone());

    Ok(auth_response(
        project_id,
        &record,
        payload.return_secure_token,
    ))
}

fn sign_in_with_password(
    state: &SharedState,
    payload: SignInRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let email_key = normalize_email(&payload.email);
    let Some(local_id) = project.user_ids_by_email.get(&email_key).cloned() else {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_NOT_FOUND"));
    };

    let Some(record) = project.users_by_id.get_mut(&local_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };

    if record.password_hash.as_deref() != Some(&hash_password(&payload.password)) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_PASSWORD"));
    }

    record.last_login_at_ms = Some(now_ms());
    Ok(auth_response(
        project_id,
        record,
        payload.return_secure_token,
    ))
}

fn lookup(state: &SharedState, payload: LookupRequest) -> Result<LookupResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let Some(project) = projects.get(project_id) else {
        return Ok(LookupResponse { users: vec![] });
    };

    let mut ids = Vec::new();
    if let Some(id_token) = payload.id_token {
        ids.push(parse_token_local_id(&id_token)?);
    }
    if let Some(local_ids) = payload.local_id {
        ids.extend(local_ids);
    }

    let users = ids
        .into_iter()
        .filter_map(|id| project.users_by_id.get(&id))
        .map(EmulatorUser::from)
        .collect();

    Ok(LookupResponse { users })
}

fn delete_identity_account(
    state: &SharedState,
    payload: DeleteRequest,
) -> Result<EmptyResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let local_id = match (payload.local_id, payload.id_token) {
        (Some(local_id), _) => local_id,
        (None, Some(id_token)) => parse_token_local_id(&id_token)?,
        (None, None) => return Err(error(StatusCode::BAD_REQUEST, "MISSING_LOCAL_ID")),
    };

    delete_account(&state.auth, project_id, &local_id)?;
    Ok(EmptyResponse {})
}

fn sign_in_with_custom_token(
    state: &SharedState,
    payload: CustomTokenRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let local_id = parse_custom_token_subject(&payload.token)?;
    let email = format!("{local_id}@custom-token.local");
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let record = get_or_create_user(project, &local_id, &email, "custom", &local_id);

    record.last_login_at_ms = Some(now_ms());
    Ok(auth_response(
        project_id,
        record,
        payload.return_secure_token,
    ))
}

fn sign_in_with_idp(state: &SharedState, payload: IdpRequest) -> Result<AuthResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let _request_uri = payload.request_uri.as_deref().unwrap_or("http://localhost");
    let post_body = parse_post_body(&payload.post_body);
    let provider_id = post_body
        .get("providerId")
        .or_else(|| post_body.get("provider_id"))
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_PROVIDER_ID"))?;
    let raw_id = post_body
        .get("rawId")
        .or_else(|| post_body.get("raw_id"))
        .or_else(|| post_body.get("id_token"))
        .or_else(|| post_body.get("access_token"))
        .or_else(|| post_body.get("oauth_token"))
        .cloned()
        .unwrap_or_else(|| format!("{provider_id}:{}", Uuid::new_v4()));
    let email = post_body
        .get("email")
        .cloned()
        .unwrap_or_else(|| format!("{}@{}.local", sanitize_email_part(&raw_id), provider_id));

    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let local_id = project
        .user_ids_by_provider
        .get(&(provider_id.clone(), raw_id.clone()))
        .cloned();
    let record = match local_id.and_then(|id| project.users_by_id.get_mut(&id)) {
        Some(record) => record,
        None => {
            let local_id = Uuid::new_v4().to_string();
            let record = UserRecord {
                local_id: local_id.clone(),
                email: email.clone(),
                password_hash: None,
                providers: vec![ProviderRecord {
                    provider_id: provider_id.clone(),
                    raw_id: raw_id.clone(),
                    email: email.clone(),
                }],
                created_at_ms: now_ms(),
                last_login_at_ms: None,
            };
            project
                .user_ids_by_provider
                .insert((provider_id, raw_id), local_id.clone());
            project
                .user_ids_by_email
                .entry(normalize_email(&email))
                .or_insert_with(|| local_id.clone());
            project.users_by_id.insert(local_id.clone(), record);
            project
                .users_by_id
                .get_mut(&local_id)
                .expect("inserted user")
        }
    };

    record.last_login_at_ms = Some(now_ms());
    Ok(auth_response(
        project_id,
        record,
        payload.return_secure_token,
    ))
}

fn send_oob_code(
    state: &SharedState,
    payload: SendOobCodeRequest,
) -> Result<OobCodeResponse, AuthError> {
    if payload.request_type != "EMAIL_SIGNIN" {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OOB_REQUEST_TYPE",
        ));
    }

    let _continue_url = payload.continue_url.as_deref().unwrap_or("");
    let project_id = DEFAULT_PROJECT;
    let code = format!("firelite-oob-{}", Uuid::new_v4());
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    project.oob_codes.insert(
        code.clone(),
        OobCodeRecord {
            email: payload.email.clone(),
            request_type: payload.request_type,
            created_at_ms: now_ms(),
        },
    );

    Ok(OobCodeResponse {
        email: payload.email,
        oob_code: code,
    })
}

fn sign_in_with_email_link(
    state: &SharedState,
    payload: EmailLinkRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = DEFAULT_PROJECT;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let Some(code) = project.oob_codes.remove(&payload.oob_code) else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"));
    };
    if code.request_type != "EMAIL_SIGNIN"
        || normalize_email(&code.email) != normalize_email(&payload.email)
    {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_EMAIL"));
    }

    let local_id = project
        .user_ids_by_email
        .get(&normalize_email(&payload.email))
        .cloned()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let record = get_or_create_user(
        project,
        &local_id,
        &payload.email,
        "emailLink",
        &payload.email,
    );
    record.last_login_at_ms = Some(now_ms());

    Ok(auth_response(
        project_id,
        record,
        payload.return_secure_token,
    ))
}

async fn list_accounts(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> AuthResult<ListAccountsResponse> {
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let users = projects
        .get(&project_id)
        .map(|project| {
            project
                .users_by_id
                .values()
                .map(EmulatorUser::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(ListAccountsResponse { users }))
}

async fn reset_project_accounts(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> AuthResult<EmptyResponse> {
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    projects.remove(&project_id);
    Ok(Json(EmptyResponse {}))
}

async fn delete_emulator_account(
    State(state): State<SharedState>,
    Path((project_id, local_id)): Path<(String, String)>,
) -> AuthResult<EmptyResponse> {
    delete_account(&state.auth, &project_id, &local_id)?;
    Ok(Json(EmptyResponse {}))
}

async fn list_oob_codes(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> AuthResult<ListOobCodesResponse> {
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let oob_codes = projects
        .get(&project_id)
        .map(|project| {
            project
                .oob_codes
                .iter()
                .map(|(oob_code, record)| EmulatorOobCode {
                    email: record.email.clone(),
                    oob_code: oob_code.clone(),
                    request_type: record.request_type.clone(),
                    created_at: record.created_at_ms.to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(ListOobCodesResponse { oob_codes }))
}

fn delete_account(auth: &AuthState, project_id: &str, local_id: &str) -> Result<(), AuthError> {
    let mut projects = auth.projects.write().expect("auth state poisoned");
    let Some(project) = projects.get_mut(project_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };
    let Some(record) = project.users_by_id.remove(local_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };
    project
        .user_ids_by_email
        .remove(&normalize_email(&record.email));
    for provider in record.providers {
        project
            .user_ids_by_provider
            .remove(&(provider.provider_id, provider.raw_id));
    }
    Ok(())
}

fn parse_payload<T>(payload: serde_json::Value) -> Result<T, AuthError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(payload).map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_JSON"))
}

fn get_or_create_user<'a>(
    project: &'a mut ProjectAuthState,
    local_id: &str,
    email: &str,
    provider_id: &str,
    raw_id: &str,
) -> &'a mut UserRecord {
    if project.users_by_id.contains_key(local_id) {
        return project.users_by_id.get_mut(local_id).expect("user exists");
    }

    let record = UserRecord {
        local_id: local_id.to_string(),
        email: email.to_string(),
        password_hash: None,
        providers: vec![ProviderRecord {
            provider_id: provider_id.to_string(),
            raw_id: raw_id.to_string(),
            email: email.to_string(),
        }],
        created_at_ms: now_ms(),
        last_login_at_ms: None,
    };
    project
        .user_ids_by_email
        .entry(normalize_email(email))
        .or_insert_with(|| local_id.to_string());
    project.user_ids_by_provider.insert(
        (provider_id.to_string(), raw_id.to_string()),
        local_id.to_string(),
    );
    project.users_by_id.insert(local_id.to_string(), record);
    project
        .users_by_id
        .get_mut(local_id)
        .expect("inserted user")
}

fn parse_custom_token_subject(token: &str) -> Result<String, AuthError> {
    if !token.contains('.') {
        return Ok(token.to_string());
    }

    let Some(payload) = token.split('.').nth(1) else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))?;
    value
        .get("uid")
        .or_else(|| value.get("sub"))
        .or_else(|| value.get("user_id"))
        .and_then(|sub| sub.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))
}

fn parse_post_body(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(input: &str) -> String {
    let input = input.replace('+', " ");
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn sanitize_email_part(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn auth_response(
    project_id: &str,
    record: &UserRecord,
    return_secure_token: Option<bool>,
) -> AuthResponse {
    let include_token = return_secure_token.unwrap_or(true);
    AuthResponse {
        kind: "identitytoolkit#SignupNewUserResponse",
        local_id: record.local_id.clone(),
        email: record.email.clone(),
        id_token: include_token
            .then(|| make_token(project_id, &record.local_id))
            .unwrap_or_default(),
        refresh_token: include_token
            .then(|| format!("firelite-refresh-{}", Uuid::new_v4()))
            .unwrap_or_default(),
        expires_in: "3600".to_string(),
    }
}

fn make_token(project_id: &str, local_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"aud":"{project_id}","iss":"https://securetoken.google.com/{project_id}","sub":"{local_id}","user_id":"{local_id}","iat":{},"exp":{}}}"#,
        now_secs(),
        now_secs() + 3600
    ));
    format!("{header}.{payload}.")
}

fn parse_token_local_id(token: &str) -> Result<String, AuthError> {
    let Some(payload) = token.split('.').nth(1) else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))?;
    value
        .get("sub")
        .and_then(|sub| sub.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

fn now_secs() -> u64 {
    now_ms() / 1000
}

fn error(status: StatusCode, message: &'static str) -> AuthError {
    AuthError { status, message }
}

impl From<&UserRecord> for EmulatorUser {
    fn from(record: &UserRecord) -> Self {
        Self {
            local_id: record.local_id.clone(),
            email: record.email.clone(),
            password_hash: record.password_hash.clone().unwrap_or_default(),
            valid_since: (record.created_at_ms / 1000).to_string(),
            created_at: record.created_at_ms.to_string(),
            last_login_at: record
                .last_login_at_ms
                .unwrap_or(record.created_at_ms)
                .to_string(),
            provider_user_info: record
                .providers
                .iter()
                .map(|provider| ProviderInfo {
                    provider_id: provider.provider_id.clone(),
                    raw_id: provider.raw_id.clone(),
                    email: provider.email.clone(),
                })
                .collect(),
        }
    }
}
