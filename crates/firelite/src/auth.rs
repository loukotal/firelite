use crate::server::AppState;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
            "/securetoken.googleapis.com/v1/token",
            post(refresh_secure_token).options(preflight),
        )
        .route(
            "/identitytoolkit.googleapis.com/v1/*action",
            post(identity_action)
                .get(identity_get_action)
                .options(preflight),
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
        .route("/emulator/action", get(oob_action))
}

#[derive(Debug, Clone, Default)]
pub struct AuthState {
    projects: Arc<RwLock<HashMap<String, ProjectAuthState>>>,
}

#[derive(Debug, Clone, Default)]
struct ProjectAuthState {
    users_by_id: HashMap<String, UserRecord>,
    user_ids_by_email: HashMap<String, String>,
    user_ids_by_phone: HashMap<String, String>,
    user_ids_by_provider: HashMap<(String, String), String>,
    oob_codes: HashMap<String, OobCodeRecord>,
}

#[derive(Debug, Clone)]
struct UserRecord {
    local_id: String,
    email: String,
    display_name: Option<String>,
    photo_url: Option<String>,
    phone_number: Option<String>,
    custom_attributes: Option<String>,
    disabled: bool,
    email_verified: bool,
    password_hash: Option<String>,
    valid_since_secs: u64,
    mfa_info: Vec<MfaEnrollment>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaEnrollment {
    #[serde(skip_serializing_if = "Option::is_none")]
    mfa_enrollment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unobfuscated_phone_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enrolled_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaUpdate {
    enrollments: Option<Vec<MfaEnrollment>>,
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
struct RefreshTokenRequest {
    grant_type: String,
    refresh_token: String,
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
    return_oob_link: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmailLinkRequest {
    email: String,
    oob_code: String,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OobActionQuery {
    mode: Option<String>,
    oob_code: Option<String>,
    project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminCreateRequest {
    local_id: Option<String>,
    email: Option<String>,
    password: Option<String>,
    display_name: Option<String>,
    photo_url: Option<String>,
    phone_number: Option<String>,
    custom_attributes: Option<String>,
    disabled: Option<bool>,
    email_verified: Option<bool>,
    mfa_info: Option<Vec<MfaEnrollment>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminUpdateRequest {
    local_id: String,
    display_name: Option<String>,
    photo_url: Option<String>,
    phone_number: Option<String>,
    custom_attributes: Option<String>,
    disabled: Option<bool>,
    disable_user: Option<bool>,
    email_verified: Option<bool>,
    valid_since: Option<u64>,
    delete_attribute: Option<Vec<String>>,
    delete_provider: Option<Vec<String>>,
    mfa: Option<MfaUpdate>,
    mfa_info: Option<Vec<MfaEnrollment>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminLookupRequest {
    local_id: Option<Vec<String>>,
    email: Option<Vec<String>>,
    phone_number: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminDeleteRequest {
    local_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminBatchDeleteRequest {
    local_ids: Vec<String>,
    force: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminBatchGetQuery {
    max_results: Option<usize>,
    next_page_token: Option<String>,
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
struct SecureTokenResponse {
    access_token: String,
    expires_in: String,
    token_type: &'static str,
    refresh_token: String,
    id_token: String,
    user_id: String,
    project_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OobCodeResponse {
    email: String,
    oob_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oob_link: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    photo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_attributes: Option<String>,
    disabled: bool,
    email_verified: bool,
    password_hash: String,
    valid_since: String,
    created_at: String,
    last_login_at: String,
    provider_user_info: Vec<ProviderInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mfa_info: Vec<MfaEnrollment>,
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

async fn preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn refresh_secure_token(
    State(state): State<SharedState>,
    Form(payload): Form<RefreshTokenRequest>,
) -> AuthResult<SecureTokenResponse> {
    if payload.grant_type != "refresh_token" {
        return Err(error(StatusCode::BAD_REQUEST, "UNSUPPORTED_GRANT_TYPE"));
    }
    let (project_id, local_id, issued_at) = parse_refresh_token(&payload.refresh_token)?;
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let record = projects
        .get(&project_id)
        .and_then(|project| project.users_by_id.get(&local_id))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if record.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if issued_at < record.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    let id_token = make_token(&project_id, record);
    Ok(Json(SecureTokenResponse {
        access_token: id_token.clone(),
        expires_in: "3600".to_string(),
        token_type: "Bearer",
        refresh_token: make_refresh_token(&project_id, &local_id),
        id_token,
        user_id: local_id,
        project_id,
    }))
}

async fn identity_action(
    State(state): State<SharedState>,
    Path(action): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> AuthResult<serde_json::Value> {
    let auth_base_url = request_base_url(&headers);
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
            serde_json::to_value(send_oob_code(
                &state,
                &default_project_id(),
                &auth_base_url,
                payload,
            )?)
        }
        "accounts:signInWithEmailLink" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(sign_in_with_email_link(&state, payload)?)
        }
        _ => {
            if let Some((project_id, admin_action)) = parse_project_action(&action) {
                match admin_action {
                    "accounts" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(admin_create_user(&state, &project_id, payload)?)
                    }
                    "accounts:lookup" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(admin_lookup(&state, &project_id, payload)?)
                    }
                    "accounts:update" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(admin_update_user(&state, &project_id, payload)?)
                    }
                    "accounts:delete" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(admin_delete_user(&state, &project_id, payload)?)
                    }
                    "accounts:batchDelete" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(admin_batch_delete_users(
                            &state,
                            &project_id,
                            payload,
                        )?)
                    }
                    "accounts:sendOobCode" => {
                        let payload = parse_payload(payload)?;
                        serde_json::to_value(send_oob_code(
                            &state,
                            &project_id,
                            &auth_base_url,
                            payload,
                        )?)
                    }
                    _ => return Err(error(StatusCode::NOT_FOUND, "NOT_FOUND")),
                }
            } else {
                return Err(error(StatusCode::NOT_FOUND, "NOT_FOUND"));
            }
        }
    }
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?;

    Ok(Json(body))
}

async fn identity_get_action(
    State(state): State<SharedState>,
    Path(action): Path<String>,
    Query(query): Query<AdminBatchGetQuery>,
) -> AuthResult<serde_json::Value> {
    let Some((project_id, "accounts:batchGet")) = parse_project_action(&action) else {
        return Err(error(StatusCode::NOT_FOUND, "NOT_FOUND"));
    };
    let body = serde_json::to_value(admin_batch_get(&state, &project_id, query)?)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?;
    Ok(Json(body))
}

async fn oob_action(
    State(state): State<SharedState>,
    Query(query): Query<OobActionQuery>,
) -> AuthResult<serde_json::Value> {
    let project_id = query.project_id.unwrap_or_else(default_project_id);
    let oob_code = query
        .oob_code
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OOB_CODE"))?;
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let record = projects
        .get(&project_id)
        .and_then(|project| project.oob_codes.get(&oob_code))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"))?;
    serde_json::to_value(json!({
        "mode": query.mode.unwrap_or_else(|| "action".to_string()),
        "oobCode": oob_code,
        "projectId": project_id,
        "email": record.email,
        "requestType": record.request_type,
    }))
    .map(Json)
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))
}

fn sign_up(state: &SharedState, payload: SignUpRequest) -> Result<AuthResponse, AuthError> {
    let project_id = default_project_id();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let email = normalize_email(&payload.email);
    let email_key = email.clone();

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
        email: email.clone(),
        display_name: None,
        photo_url: None,
        phone_number: None,
        custom_attributes: None,
        disabled: false,
        email_verified: false,
        password_hash: Some(hash_password(&payload.password)),
        valid_since_secs: now / 1000,
        mfa_info: Vec::new(),
        providers: vec![ProviderRecord {
            provider_id: "password".to_string(),
            raw_id: local_id,
            email,
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
        &project_id,
        &record,
        payload.return_secure_token,
    ))
}

fn admin_create_user(
    state: &SharedState,
    project_id: &str,
    payload: AdminCreateRequest,
) -> Result<EmulatorUser, AuthError> {
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let local_id = payload
        .local_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let email = payload
        .email
        .unwrap_or_else(|| format!("{local_id}@admin.local"));
    let email = normalize_email(&email);
    let email_key = email.clone();

    if project.users_by_id.contains_key(&local_id) {
        return Err(error(StatusCode::BAD_REQUEST, "UID_EXISTS"));
    }
    if project.user_ids_by_email.contains_key(&email_key) {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_EXISTS"));
    }
    let phone_number = payload.phone_number;
    if let Some(phone_number) = &phone_number {
        validate_phone_number_available(project, phone_number, None)?;
    }

    let now = now_ms();
    let record = UserRecord {
        local_id: local_id.clone(),
        email: email.clone(),
        display_name: payload.display_name,
        photo_url: payload.photo_url,
        phone_number: phone_number.clone(),
        custom_attributes: payload.custom_attributes,
        disabled: payload.disabled.unwrap_or(false),
        email_verified: payload.email_verified.unwrap_or(false),
        password_hash: payload.password.as_deref().map(hash_password),
        valid_since_secs: now / 1000,
        mfa_info: normalize_mfa_enrollments(payload.mfa_info.unwrap_or_default()),
        providers: vec![ProviderRecord {
            provider_id: "password".to_string(),
            raw_id: local_id.clone(),
            email: email.clone(),
        }],
        created_at_ms: now_ms(),
        last_login_at_ms: None,
    };
    project
        .user_ids_by_email
        .insert(email_key, local_id.clone());
    if let Some(phone_number) = phone_number {
        project
            .user_ids_by_phone
            .insert(phone_number, local_id.clone());
    }
    project.users_by_id.insert(local_id.clone(), record);
    Ok(EmulatorUser::from(
        project.users_by_id.get(&local_id).expect("inserted user"),
    ))
}

fn admin_update_user(
    state: &SharedState,
    project_id: &str,
    payload: AdminUpdateRequest,
) -> Result<EmulatorUser, AuthError> {
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let Some(project) = projects.get_mut(project_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };
    let local_id = payload.local_id;
    if let Some(phone_number) = &payload.phone_number {
        validate_phone_number_available(project, phone_number, Some(&local_id))?;
    }

    let Some(record) = project.users_by_id.get_mut(&local_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };

    if let Some(delete_attributes) = payload.delete_attribute {
        for attribute in delete_attributes {
            match attribute.as_str() {
                "DISPLAY_NAME" => record.display_name = None,
                "PHOTO_URL" => record.photo_url = None,
                _ => {}
            }
        }
    }
    if let Some(delete_providers) = payload.delete_provider {
        if delete_providers.iter().any(|provider| provider == "phone") {
            if let Some(phone_number) = record.phone_number.take() {
                project.user_ids_by_phone.remove(&phone_number);
            }
            record
                .providers
                .retain(|provider| provider.provider_id != "phone");
        }
    }
    if let Some(display_name) = payload.display_name {
        record.display_name = Some(display_name);
    }
    if let Some(photo_url) = payload.photo_url {
        record.photo_url = Some(photo_url);
    }
    if let Some(phone_number) = payload.phone_number {
        if let Some(old_phone_number) = record.phone_number.replace(phone_number.clone()) {
            project.user_ids_by_phone.remove(&old_phone_number);
        }
        project
            .user_ids_by_phone
            .insert(phone_number.clone(), local_id.clone());
        upsert_phone_provider(record, &phone_number);
    }
    if let Some(custom_attributes) = payload.custom_attributes {
        record.custom_attributes = Some(custom_attributes);
    }
    if let Some(disabled) = payload.disabled.or(payload.disable_user) {
        record.disabled = disabled;
    }
    if let Some(email_verified) = payload.email_verified {
        record.email_verified = email_verified;
    }
    if let Some(valid_since) = payload.valid_since {
        record.valid_since_secs = valid_since;
    }
    if let Some(mfa) = payload.mfa {
        record.mfa_info = normalize_mfa_enrollments(mfa.enrollments.unwrap_or_default());
    }
    if let Some(mfa_info) = payload.mfa_info {
        record.mfa_info = normalize_mfa_enrollments(mfa_info);
    }

    Ok(EmulatorUser::from(&*record))
}

fn sign_in_with_password(
    state: &SharedState,
    payload: SignInRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = project_id_for_email(&state.auth, &payload.email);
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let email_key = normalize_email(&payload.email);
    let Some(local_id) = project.user_ids_by_email.get(&email_key).cloned() else {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_NOT_FOUND"));
    };

    let Some(record) = project.users_by_id.get_mut(&local_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };

    if record.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if record.password_hash.as_deref() != Some(&hash_password(&payload.password)) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_PASSWORD"));
    }

    record.last_login_at_ms = Some(now_ms());
    Ok(auth_response(
        &project_id,
        record,
        payload.return_secure_token,
    ))
}

fn admin_lookup(
    state: &SharedState,
    project_id: &str,
    payload: AdminLookupRequest,
) -> Result<LookupResponse, AuthError> {
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let Some(project) = projects.get(project_id) else {
        return Ok(LookupResponse { users: vec![] });
    };

    let mut users = Vec::new();
    if let Some(local_ids) = payload.local_id {
        users.extend(
            local_ids
                .into_iter()
                .filter_map(|id| project.users_by_id.get(&id))
                .map(EmulatorUser::from),
        );
    }
    if let Some(emails) = payload.email {
        users.extend(
            emails
                .into_iter()
                .filter_map(|email| project.user_ids_by_email.get(&normalize_email(&email)))
                .filter_map(|id| project.users_by_id.get(id))
                .map(EmulatorUser::from),
        );
    }
    if let Some(phone_numbers) = payload.phone_number {
        users.extend(
            phone_numbers
                .into_iter()
                .filter_map(|phone_number| project.user_ids_by_phone.get(&phone_number))
                .filter_map(|id| project.users_by_id.get(id))
                .map(EmulatorUser::from),
        );
    }

    Ok(LookupResponse { users })
}

fn admin_batch_get(
    state: &SharedState,
    project_id: &str,
    query: AdminBatchGetQuery,
) -> Result<ListAccountsResponse, AuthError> {
    let _next_page_token = query.next_page_token.as_deref();
    let max_results = query.max_results.unwrap_or(1000);
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let users = projects
        .get(project_id)
        .map(|project| {
            project
                .users_by_id
                .values()
                .take(max_results)
                .map(EmulatorUser::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(ListAccountsResponse { users })
}

fn admin_delete_user(
    state: &SharedState,
    project_id: &str,
    payload: AdminDeleteRequest,
) -> Result<EmptyResponse, AuthError> {
    delete_account(&state.auth, project_id, &payload.local_id)?;
    Ok(EmptyResponse {})
}

fn admin_batch_delete_users(
    state: &SharedState,
    project_id: &str,
    payload: AdminBatchDeleteRequest,
) -> Result<serde_json::Value, AuthError> {
    if payload.force != Some(true) {
        return Err(error(StatusCode::BAD_REQUEST, "MISSING_FORCE"));
    }

    let mut errors = Vec::new();
    for (index, local_id) in payload.local_ids.iter().enumerate() {
        if delete_account(&state.auth, project_id, local_id).is_err() {
            errors.push(serde_json::json!({
                "index": index,
                "localId": local_id,
                "message": "USER_NOT_FOUND"
            }));
        }
    }

    Ok(serde_json::json!({ "errors": errors }))
}

fn lookup(state: &SharedState, payload: LookupRequest) -> Result<LookupResponse, AuthError> {
    let projects = state.auth.projects.read().expect("auth state poisoned");

    let mut users = Vec::new();
    if let Some(id_token) = payload.id_token {
        let claims = parse_token_claims(&id_token)?;
        if let Some(record) = projects
            .get(&claims.project_id)
            .and_then(|project| project.users_by_id.get(&claims.local_id))
        {
            if record.disabled {
                return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
            }
            if claims.issued_at < record.valid_since_secs {
                return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
            }
            users.push(EmulatorUser::from(record));
        }
    }
    if let Some(local_ids) = payload.local_id {
        let project_id = default_project_id();
        if let Some(project) = projects.get(&project_id) {
            users.extend(
                local_ids
                    .into_iter()
                    .filter_map(|id| project.users_by_id.get(&id))
                    .map(EmulatorUser::from),
            );
        }
    }

    Ok(LookupResponse { users })
}

fn delete_identity_account(
    state: &SharedState,
    payload: DeleteRequest,
) -> Result<EmptyResponse, AuthError> {
    let (project_id, local_id) = match (payload.local_id, payload.id_token) {
        (Some(local_id), _) => (default_project_id(), local_id),
        (None, Some(id_token)) => {
            let claims = parse_token_claims(&id_token)?;
            (claims.project_id, claims.local_id)
        }
        (None, None) => return Err(error(StatusCode::BAD_REQUEST, "MISSING_LOCAL_ID")),
    };

    delete_account(&state.auth, &project_id, &local_id)?;
    Ok(EmptyResponse {})
}

fn sign_in_with_custom_token(
    state: &SharedState,
    payload: CustomTokenRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = default_project_id();
    let local_id = parse_custom_token_subject(&payload.token)?;
    let email = format!("{local_id}@custom-token.local");
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.clone()).or_default();
    let record = get_or_create_user(project, &local_id, &email, "custom", &local_id);

    record.last_login_at_ms = Some(now_ms());
    Ok(auth_response(
        &project_id,
        record,
        payload.return_secure_token,
    ))
}

fn sign_in_with_idp(state: &SharedState, payload: IdpRequest) -> Result<AuthResponse, AuthError> {
    let project_id = default_project_id();
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
    let email = normalize_email(&email);

    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.clone()).or_default();
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
                display_name: None,
                photo_url: None,
                phone_number: None,
                custom_attributes: None,
                disabled: false,
                email_verified: false,
                password_hash: None,
                valid_since_secs: now_ms() / 1000,
                mfa_info: Vec::new(),
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
                .entry(email.clone())
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
        &project_id,
        record,
        payload.return_secure_token,
    ))
}

fn send_oob_code(
    state: &SharedState,
    project_id: &str,
    auth_base_url: &str,
    payload: SendOobCodeRequest,
) -> Result<OobCodeResponse, AuthError> {
    if payload.request_type != "EMAIL_SIGNIN" && payload.request_type != "PASSWORD_RESET" {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OOB_REQUEST_TYPE",
        ));
    }

    let _continue_url = payload.continue_url.as_deref().unwrap_or("");
    let email = normalize_email(&payload.email);
    let code = format!("firelite-oob-{}", Uuid::new_v4());
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    if payload.request_type == "PASSWORD_RESET" && !project.user_ids_by_email.contains_key(&email) {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_NOT_FOUND"));
    }
    project.oob_codes.insert(
        code.clone(),
        OobCodeRecord {
            email: email.clone(),
            request_type: payload.request_type.clone(),
            created_at_ms: now_ms(),
        },
    );
    let oob_link = payload
        .return_oob_link
        .unwrap_or(false)
        .then(|| make_oob_link(auth_base_url, project_id, &payload.request_type, &code));

    Ok(OobCodeResponse {
        email,
        oob_code: code,
        oob_link,
    })
}

fn sign_in_with_email_link(
    state: &SharedState,
    payload: EmailLinkRequest,
) -> Result<AuthResponse, AuthError> {
    let project_id = default_project_id();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.clone()).or_default();
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
        &normalize_email(&payload.email),
        "emailLink",
        &normalize_email(&payload.email),
    );
    record.last_login_at_ms = Some(now_ms());

    Ok(auth_response(
        &project_id,
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
    if let Some(phone_number) = record.phone_number {
        project.user_ids_by_phone.remove(&phone_number);
    }
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
    let email = normalize_email(email);
    if project.users_by_id.contains_key(local_id) {
        return project.users_by_id.get_mut(local_id).expect("user exists");
    }

    let record = UserRecord {
        local_id: local_id.to_string(),
        email: email.clone(),
        display_name: None,
        photo_url: None,
        phone_number: None,
        custom_attributes: None,
        disabled: false,
        email_verified: false,
        password_hash: None,
        valid_since_secs: now_ms() / 1000,
        mfa_info: Vec::new(),
        providers: vec![ProviderRecord {
            provider_id: provider_id.to_string(),
            raw_id: raw_id.to_string(),
            email: email.clone(),
        }],
        created_at_ms: now_ms(),
        last_login_at_ms: None,
    };
    project
        .user_ids_by_email
        .entry(email)
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

fn parse_project_action(action: &str) -> Option<(String, &str)> {
    let rest = action.strip_prefix("projects/")?;
    let (project_id, admin_action) = rest.split_once('/')?;
    if project_id.is_empty() || admin_action.is_empty() {
        return None;
    }
    Some((project_id.to_string(), admin_action))
}

fn make_oob_link(
    auth_base_url: &str,
    project_id: &str,
    request_type: &str,
    oob_code: &str,
) -> String {
    let mode = match request_type {
        "PASSWORD_RESET" => "resetPassword",
        "EMAIL_SIGNIN" => "signIn",
        _ => "action",
    };
    format!(
        "{auth_base_url}/emulator/action?mode={mode}&oobCode={}&projectId={}",
        percent_encode_query(oob_code),
        percent_encode_query(project_id)
    )
}

fn request_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost:9099");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

fn percent_encode_query(value: &str) -> String {
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
        id_token: if include_token {
            make_token(project_id, record)
        } else {
            String::default()
        },
        refresh_token: if include_token {
            make_refresh_token(project_id, &record.local_id)
        } else {
            String::default()
        },
        expires_in: "3600".to_string(),
    }
}

fn make_refresh_token(project_id: &str, local_id: &str) -> String {
    let payload = URL_SAFE_NO_PAD.encode(format!(
        r#"{{"project_id":"{project_id}","local_id":"{local_id}","iat":{},"nonce":"{}"}}"#,
        now_secs(),
        Uuid::new_v4()
    ));
    format!("firelite-refresh.{payload}")
}

fn parse_refresh_token(token: &str) -> Result<(String, String, u64), AuthError> {
    let Some(payload) = token.strip_prefix("firelite-refresh.") else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"))?;
    let project_id = value
        .get("project_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"))?;
    let local_id = value
        .get("local_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"))?;
    let issued_at = value
        .get("iat")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    Ok((project_id.to_string(), local_id.to_string(), issued_at))
}

fn make_token(project_id: &str, record: &UserRecord) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let issued_at = now_secs();
    let mut payload = serde_json::json!({
        "aud": project_id,
        "iss": format!("https://securetoken.google.com/{project_id}"),
        "sub": record.local_id,
        "user_id": record.local_id,
        "email": record.email,
        "email_verified": record.email_verified,
        "iat": issued_at,
        "auth_time": issued_at,
        "exp": issued_at + 3600,
        "firebase": {
            "sign_in_provider": "password",
            "identities": {
                "email": [record.email]
            }
        },
    });
    if let Some(phone_number) = &record.phone_number {
        let object = payload.as_object_mut().expect("token payload is an object");
        object.insert("phone_number".to_string(), serde_json::json!(phone_number));
        if let Some(firebase) = object
            .get_mut("firebase")
            .and_then(|value| value.as_object_mut())
        {
            if let Some(identities) = firebase
                .get_mut("identities")
                .and_then(|value| value.as_object_mut())
            {
                identities.insert("phone".to_string(), serde_json::json!([phone_number]));
            }
        }
    }
    if let Some(custom_attributes) = &record.custom_attributes {
        if let Ok(serde_json::Value::Object(claims)) =
            serde_json::from_str::<serde_json::Value>(custom_attributes)
        {
            let object = payload.as_object_mut().expect("token payload is an object");
            for (key, value) in claims {
                object.insert(key, value);
            }
        }
    }
    let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("{header}.{payload}.")
}

#[derive(Debug)]
struct TokenClaims {
    project_id: String,
    local_id: String,
    issued_at: u64,
}

fn parse_token_claims(token: &str) -> Result<TokenClaims, AuthError> {
    let Some(payload) = token.split('.').nth(1) else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))?;
    let local_id = value
        .get("sub")
        .and_then(|sub| sub.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"))?;
    let project_id = value
        .get("aud")
        .and_then(|aud| aud.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(default_project_id);
    let issued_at = value.get("iat").and_then(|iat| iat.as_u64()).unwrap_or(0);
    Ok(TokenClaims {
        project_id,
        local_id,
        issued_at,
    })
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn validate_phone_number_available(
    project: &ProjectAuthState,
    phone_number: &str,
    current_local_id: Option<&str>,
) -> Result<(), AuthError> {
    if let Some(existing_local_id) = project.user_ids_by_phone.get(phone_number) {
        if Some(existing_local_id.as_str()) != current_local_id {
            return Err(error(StatusCode::BAD_REQUEST, "PHONE_NUMBER_EXISTS"));
        }
    }
    if !is_valid_phone_number(phone_number) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_PHONE_NUMBER"));
    }
    Ok(())
}

fn is_valid_phone_number(phone_number: &str) -> bool {
    let Some(rest) = phone_number.strip_prefix('+') else {
        return false;
    };
    (1..=15).contains(&rest.len()) && rest.chars().all(|ch| ch.is_ascii_digit())
}

fn upsert_phone_provider(record: &mut UserRecord, phone_number: &str) {
    if let Some(provider) = record
        .providers
        .iter_mut()
        .find(|provider| provider.provider_id == "phone")
    {
        provider.raw_id = phone_number.to_string();
        provider.email = String::new();
        return;
    }
    record.providers.push(ProviderRecord {
        provider_id: "phone".to_string(),
        raw_id: phone_number.to_string(),
        email: String::new(),
    });
}

fn normalize_mfa_enrollments(enrollments: Vec<MfaEnrollment>) -> Vec<MfaEnrollment> {
    enrollments
        .into_iter()
        .map(|mut enrollment| {
            if enrollment.mfa_enrollment_id.is_none() {
                enrollment.mfa_enrollment_id = Some(Uuid::new_v4().to_string());
            }
            if enrollment.enrolled_at.is_none() {
                enrollment.enrolled_at = Some("1970-01-01T00:00:00.000Z".to_string());
            }
            if enrollment.unobfuscated_phone_info.is_none() {
                enrollment.unobfuscated_phone_info = enrollment.phone_info.clone();
            }
            enrollment
        })
        .collect()
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

fn default_project_id() -> String {
    std::env::var("GCLOUD_PROJECT")
        .or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT"))
        .unwrap_or_else(|_| DEFAULT_PROJECT.to_string())
}

fn project_id_for_email(auth: &AuthState, email: &str) -> String {
    let email_key = normalize_email(email);
    let projects = auth.projects.read().expect("auth state poisoned");
    projects
        .iter()
        .find_map(|(project_id, project)| {
            project
                .user_ids_by_email
                .contains_key(&email_key)
                .then(|| project_id.clone())
        })
        .unwrap_or_else(default_project_id)
}

impl From<&UserRecord> for EmulatorUser {
    fn from(record: &UserRecord) -> Self {
        Self {
            local_id: record.local_id.clone(),
            email: record.email.clone(),
            display_name: record.display_name.clone(),
            photo_url: record.photo_url.clone(),
            phone_number: record.phone_number.clone(),
            custom_attributes: record.custom_attributes.clone(),
            disabled: record.disabled,
            email_verified: record.email_verified,
            password_hash: record.password_hash.clone().unwrap_or_default(),
            valid_since: record.valid_since_secs.to_string(),
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
            mfa_info: record.mfa_info.clone(),
        }
    }
}
