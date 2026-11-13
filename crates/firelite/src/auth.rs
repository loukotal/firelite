use crate::server::AppState;
use anyhow::Context;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::IsTerminal,
    path::{Path as FilePath, PathBuf},
    sync::{Arc, Mutex, RwLock},
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
            "/identitytoolkit.googleapis.com/v1/recaptchaParams",
            get(recaptcha_params).options(preflight),
        )
        .route(
            "/identitytoolkit.googleapis.com/v2/recaptchaConfig",
            get(recaptcha_config).options(preflight),
        )
        .route(
            "/identitytoolkit.googleapis.com/v1/*action",
            post(identity_action)
                .get(identity_get_action)
                .options(preflight),
        )
        .route(
            "/identitytoolkit.googleapis.com/v2/*action",
            post(identity_v2_action).options(preflight),
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
        .route(
            "/emulator/v1/projects/:project_id/verificationCodes",
            get(list_verification_codes),
        )
        .route("/emulator/action", get(oob_action))
        .route("/emulator/auth/handler", get(idp_widget))
        .route("/emulator/auth/iframe", get(idp_iframe))
}

#[derive(Debug, Clone)]
pub struct AuthState {
    default_project_id: String,
    projects: Arc<RwLock<HashMap<String, ProjectAuthState>>>,
    persistence_path: Option<Arc<PathBuf>>,
    persistence_lock: Arc<Mutex<()>>,
}

impl AuthState {
    pub fn new(default_project_id: impl Into<String>) -> Self {
        Self {
            default_project_id: default_project_id.into(),
            projects: Arc::default(),
            persistence_path: None,
            persistence_lock: Arc::default(),
        }
    }

    pub fn persistent(
        default_project_id: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> anyhow::Result<Self> {
        let path = path.into();
        let projects = load_projects(&path)?;
        Ok(Self {
            default_project_id: default_project_id.into(),
            projects: Arc::new(RwLock::new(projects)),
            persistence_path: Some(Arc::new(path)),
            persistence_lock: Arc::default(),
        })
    }

    pub fn persist(&self) -> anyhow::Result<()> {
        let Some(path) = &self.persistence_path else {
            return Ok(());
        };
        let _persistence_guard = self
            .persistence_lock
            .lock()
            .expect("Auth persistence lock poisoned");
        let snapshots = {
            let projects = self.projects.read().expect("auth state poisoned");
            projects
                .iter()
                .map(|(project_id, project)| {
                    let users = project.users_by_id.values().cloned().collect::<Vec<_>>();
                    serde_json::to_string(&users)
                        .map(|users| (project_id.clone(), users))
                        .map_err(anyhow::Error::from)
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };

        let mut connection = open_database(path)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM auth_projects", [])?;
        for (project_id, users) in snapshots {
            transaction.execute(
                "INSERT INTO auth_projects (project_id, users_json) VALUES (?1, ?2)",
                params![project_id, users],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn is_persistent(&self) -> bool {
        self.persistence_path.is_some()
    }

    pub fn reset_persisted_project(
        path: impl AsRef<FilePath>,
        project_id: &str,
    ) -> anyhow::Result<()> {
        let connection = open_database(path.as_ref())?;
        connection.execute(
            "DELETE FROM auth_projects WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(())
    }

    fn default_project_id(&self) -> &str {
        &self.default_project_id
    }
}

impl Default for AuthState {
    fn default() -> Self {
        Self::new(DEFAULT_PROJECT)
    }
}

fn open_database(path: &FilePath) -> anyhow::Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create persistence directory {}",
                parent.display()
            )
        })?;
    }
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open persistence database {}", path.display()))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS auth_projects (
            project_id TEXT PRIMARY KEY NOT NULL,
            users_json TEXT NOT NULL
        );",
    )?;
    Ok(connection)
}

fn load_projects(path: &FilePath) -> anyhow::Result<HashMap<String, ProjectAuthState>> {
    let connection = open_database(path)?;
    let mut statement = connection.prepare("SELECT project_id, users_json FROM auth_projects")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut projects = HashMap::new();
    for row in rows {
        let (project_id, users_json) = row?;
        let users: Vec<UserRecord> = serde_json::from_str(&users_json)
            .with_context(|| format!("invalid persisted Auth state for project {project_id}"))?;
        projects.insert(project_id, ProjectAuthState::from_users(users));
    }
    Ok(projects)
}

#[derive(Debug, Clone, Default)]
struct ProjectAuthState {
    users_by_id: HashMap<String, UserRecord>,
    user_ids_by_email: HashMap<String, String>,
    user_ids_by_phone: HashMap<String, String>,
    user_ids_by_provider: HashMap<(String, String), String>,
    oob_codes: HashMap<String, OobCodeRecord>,
    verification_codes: HashMap<String, VerificationCodeRecord>,
    next_verification_code_sequence: u64,
    mfa_pending_credentials: HashMap<String, MfaPendingCredential>,
}

impl ProjectAuthState {
    fn from_users(users: Vec<UserRecord>) -> Self {
        let mut project = Self::default();
        for user in users {
            let local_id = user.local_id.clone();
            if !user.email.is_empty() {
                project
                    .user_ids_by_email
                    .insert(normalize_email(&user.email), local_id.clone());
            }
            if let Some(phone_number) = &user.phone_number {
                project
                    .user_ids_by_phone
                    .insert(phone_number.clone(), local_id.clone());
            }
            for provider in &user.providers {
                project.user_ids_by_provider.insert(
                    (provider.provider_id.clone(), provider.raw_id.clone()),
                    local_id.clone(),
                );
            }
            project.users_by_id.insert(local_id, user);
        }
        project
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProviderRecord {
    provider_id: String,
    raw_id: String,
    email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    photo_url: Option<String>,
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

#[derive(Debug, Clone)]
struct VerificationCodeRecord {
    phone_number: String,
    code: String,
    sequence: u64,
    purpose: VerificationPurpose,
}

#[derive(Debug, Clone)]
enum VerificationPurpose {
    FirstFactorSignIn,
    Enrollment {
        local_id: String,
    },
    SignIn {
        local_id: String,
        mfa_pending_credential: String,
        mfa_enrollment_id: String,
    },
}

#[derive(Debug, Clone)]
struct MfaPendingCredential {
    local_id: String,
    issued_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignUpRequest {
    email: Option<String>,
    password: Option<String>,
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
struct SendVerificationCodeRequest {
    phone_number: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneSignInRequest {
    session_info: String,
    code: String,
    id_token: Option<String>,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneUpdateRequest {
    id_token: String,
    phone_verification_info: PhoneVerificationInfo,
    return_secure_token: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaEnrollmentStartRequest {
    id_token: String,
    phone_enrollment_info: PhoneEnrollmentInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneEnrollmentInfo {
    phone_number: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaEnrollmentFinalizeRequest {
    id_token: String,
    display_name: Option<String>,
    phone_verification_info: PhoneVerificationInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneVerificationInfo {
    session_info: String,
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaSignInStartRequest {
    mfa_pending_credential: String,
    mfa_enrollment_id: String,
    phone_sign_in_info: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaSignInFinalizeRequest {
    mfa_pending_credential: String,
    phone_verification_info: PhoneVerificationInfo,
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
    post_body: Option<String>,
    request_uri: Option<String>,
    id_token: Option<String>,
    return_secure_token: Option<bool>,
    return_idp_credential: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendOobCodeRequest {
    request_type: String,
    email: Option<String>,
    id_token: Option<String>,
    continue_url: Option<String>,
    return_oob_link: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyActionCodeRequest {
    oob_code: String,
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
    password: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
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
struct ListVerificationCodesResponse {
    verification_codes: Vec<EmulatorVerificationCode>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorVerificationCode {
    phone_number: String,
    session_info: String,
    code: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    password_hash: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    federated_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    photo_url: Option<String>,
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

async fn recaptcha_params() -> Json<serde_json::Value> {
    Json(json!({
        "kind": "identitytoolkit#GetRecaptchaParamResponse",
        "recaptchaStoken": "This-is-a-fake-token__Dont-send-this-to-the-Recaptcha-service__The-Auth-Emulator-does-not-support-Recaptcha",
        "recaptchaSiteKey": "Fake-key__Do-not-send-this-to-Recaptcha_",
    }))
}

async fn recaptcha_config() -> Json<serde_json::Value> {
    Json(json!({
        "kind": "identitytoolkit#GetRecaptchaConfigResponse",
        "recaptchaKey": "Fake-key__Do-not-send-this-to-Recaptcha_",
        "recaptchaEnforcementState": [
            {
                "provider": "PHONE_PROVIDER",
                "enforcementState": "OFF"
            },
            {
                "provider": "EMAIL_PASSWORD_PROVIDER",
                "enforcementState": "OFF"
            }
        ]
    }))
}

async fn refresh_secure_token(
    State(state): State<SharedState>,
    Form(payload): Form<RefreshTokenRequest>,
) -> AuthResult<SecureTokenResponse> {
    if payload.grant_type != "refresh_token" {
        return Err(error(StatusCode::BAD_REQUEST, "UNSUPPORTED_GRANT_TYPE"));
    }
    let refresh = parse_refresh_token(&payload.refresh_token)?;
    if refresh.project_id != state.auth.default_project_id() {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_REFRESH_TOKEN"));
    }
    let project_id = refresh.project_id;
    let local_id = refresh.local_id;
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let record = projects
        .get(&project_id)
        .and_then(|project| project.users_by_id.get(&local_id))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if record.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if refresh.issued_at < record.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    let second_factor = refresh
        .second_factor
        .as_deref()
        .zip(refresh.second_factor_identifier.as_deref());
    let id_token = make_token_with_second_factor(&project_id, record, second_factor);
    Ok(Json(SecureTokenResponse {
        access_token: id_token.clone(),
        expires_in: "3600".to_string(),
        token_type: "Bearer",
        refresh_token: make_refresh_token_with_second_factor(&project_id, &local_id, second_factor),
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
            Ok(sign_in_with_password(&state, payload)?)
        }
        "accounts:sendVerificationCode" => {
            let payload = parse_payload(payload)?;
            Ok(send_verification_code(&state, payload)?)
        }
        "accounts:signInWithPhoneNumber" => {
            let payload = parse_payload(payload)?;
            Ok(sign_in_with_phone_number(&state, payload)?)
        }
        "accounts:update" => {
            if payload.get("oobCode").is_some() {
                let payload = parse_payload(payload)?;
                serde_json::to_value(apply_action_code(&state, payload)?)
            } else {
                let payload = parse_payload(payload)?;
                Ok(update_phone_number(&state, payload)?)
            }
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
            Ok(sign_in_with_idp(&state, payload)?)
        }
        "accounts:sendOobCode" => {
            let payload = parse_payload(payload)?;
            serde_json::to_value(send_oob_code(
                &state,
                state.auth.default_project_id(),
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

async fn identity_v2_action(
    State(state): State<SharedState>,
    Path(action): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> AuthResult<serde_json::Value> {
    let body = match action.as_str() {
        "accounts/mfaEnrollment:start" => {
            let payload = parse_payload(payload)?;
            start_mfa_enrollment(&state, payload)?
        }
        "accounts/mfaEnrollment:finalize" => {
            let payload = parse_payload(payload)?;
            finalize_mfa_enrollment(&state, payload)?
        }
        "accounts/mfaSignIn:start" => {
            let payload = parse_payload(payload)?;
            start_mfa_sign_in(&state, payload)?
        }
        "accounts/mfaSignIn:finalize" => {
            let payload = parse_payload(payload)?;
            finalize_mfa_sign_in(&state, payload)?
        }
        _ => return Err(error(StatusCode::NOT_FOUND, "NOT_FOUND")),
    };
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
    let mode = query.mode.unwrap_or_else(|| "action".to_string());
    let project_id = query
        .project_id
        .unwrap_or_else(|| state.auth.default_project_id().to_string());
    let oob_code = query
        .oob_code
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_OOB_CODE"))?;
    let record = {
        let projects = state.auth.projects.read().expect("auth state poisoned");
        projects
            .get(&project_id)
            .and_then(|project| project.oob_codes.get(&oob_code))
            .cloned()
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"))?
    };
    let email_verified = if mode == "verifyEmail" && record.request_type == "VERIFY_EMAIL" {
        apply_action_code(
            &state,
            ApplyActionCodeRequest {
                oob_code: oob_code.clone(),
            },
        )?;
        true
    } else {
        false
    };
    serde_json::to_value(json!({
        "mode": mode,
        "oobCode": oob_code,
        "projectId": project_id,
        "email": record.email,
        "requestType": record.request_type,
        "emailVerified": email_verified,
    }))
    .map(Json)
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))
}

fn sign_up(state: &SharedState, payload: SignUpRequest) -> Result<AuthResponse, AuthError> {
    let project_id = state.auth.default_project_id().to_string();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.to_string()).or_default();
    let (email, password) = match (payload.email, payload.password) {
        (Some(email), Some(password)) => (normalize_email(&email), Some(password)),
        (None, None) => (String::new(), None),
        _ => return Err(error(StatusCode::BAD_REQUEST, "MISSING_EMAIL_OR_PASSWORD")),
    };
    let email_key = email.clone();

    if !email.is_empty() && project.user_ids_by_email.contains_key(&email_key) {
        return Err(error(StatusCode::BAD_REQUEST, "EMAIL_EXISTS"));
    }

    if password.as_ref().is_some_and(|password| password.len() < 6) {
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
        password_hash: password.as_deref().map(hash_password),
        valid_since_secs: now / 1000,
        mfa_info: Vec::new(),
        providers: password
            .map(|_| {
                vec![ProviderRecord {
                    provider_id: "password".to_string(),
                    raw_id: local_id,
                    email,
                    ..Default::default()
                }]
            })
            .unwrap_or_default(),
        created_at_ms: now,
        last_login_at_ms: None,
    };

    if !email_key.is_empty() {
        project
            .user_ids_by_email
            .insert(email_key, record.local_id.clone());
    }
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
            ..Default::default()
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
    if let Some(password) = payload.password {
        record.password_hash = Some(hash_password(&password));
        record.valid_since_secs = now_secs();
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
) -> Result<serde_json::Value, AuthError> {
    let project_id = state.auth.default_project_id().to_string();
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

    if !record.mfa_info.is_empty() {
        let local_id = record.local_id.clone();
        let email = record.email.clone();
        let mfa_info = record.mfa_info.clone();
        let pending_credential = format!("firelite-mfa-pending-{}", Uuid::new_v4());
        project.mfa_pending_credentials.insert(
            pending_credential.clone(),
            MfaPendingCredential {
                local_id: local_id.clone(),
                issued_at: now_secs(),
            },
        );
        return Ok(json!({
            "kind": "identitytoolkit#VerifyPasswordResponse",
            "localId": local_id,
            "email": email,
            "registered": true,
            "mfaPendingCredential": pending_credential,
            "mfaInfo": mfa_info,
        }));
    }

    record.last_login_at_ms = Some(now_ms());
    serde_json::to_value(auth_response(
        &project_id,
        record,
        payload.return_secure_token,
    ))
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))
}

fn send_verification_code(
    state: &SharedState,
    payload: SendVerificationCodeRequest,
) -> Result<serde_json::Value, AuthError> {
    if !is_valid_phone_number(&payload.phone_number) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_PHONE_NUMBER"));
    }
    let project_id = state.auth.default_project_id().to_string();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id).or_default();
    let session_info = create_verification_code(
        project,
        payload.phone_number,
        VerificationPurpose::FirstFactorSignIn,
    );
    Ok(json!({ "sessionInfo": session_info }))
}

fn sign_in_with_phone_number(
    state: &SharedState,
    payload: PhoneSignInRequest,
) -> Result<serde_json::Value, AuthError> {
    let project_id = state.auth.default_project_id().to_string();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"))?;
    let verification = project
        .verification_codes
        .get(&payload.session_info)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"))?;
    if !matches!(verification.purpose, VerificationPurpose::FirstFactorSignIn) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"));
    }
    if verification.code != payload.code {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CODE"));
    }

    if let Some(id_token) = payload.id_token.as_deref() {
        let claims = parse_client_token_claims(&state.auth, id_token)?;
        if claims.project_id != project_id {
            return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
        }
        validate_phone_number_available(
            project,
            &verification.phone_number,
            Some(&claims.local_id),
        )?;
        let phone_number = verification.phone_number;
        let (local_id, email, old_phone_number, id_token) = {
            let user = project
                .users_by_id
                .get_mut(&claims.local_id)
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
            if user.disabled {
                return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
            }
            if claims.issued_at < user.valid_since_secs {
                return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
            }
            let old_phone_number = user.phone_number.replace(phone_number.clone());
            upsert_phone_provider(user, &phone_number);
            user.last_login_at_ms = Some(now_ms());
            (
                user.local_id.clone(),
                user.email.clone(),
                old_phone_number,
                make_token(&project_id, user),
            )
        };
        if let Some(old_phone_number) = old_phone_number {
            project.user_ids_by_phone.remove(&old_phone_number);
            project
                .user_ids_by_provider
                .remove(&("phone".to_string(), old_phone_number));
        }
        project
            .user_ids_by_phone
            .insert(phone_number.clone(), local_id.clone());
        project.user_ids_by_provider.insert(
            ("phone".to_string(), phone_number.clone()),
            local_id.clone(),
        );
        project.verification_codes.remove(&payload.session_info);

        let include_token = payload.return_secure_token.unwrap_or(true);
        return Ok(json!({
            "idToken": if include_token { id_token } else { String::default() },
            "refreshToken": if include_token {
                make_refresh_token(&project_id, &local_id)
            } else {
                String::default()
            },
            "expiresIn": "3600",
            "localId": local_id,
            "email": email,
            "phoneNumber": phone_number,
            "isNewUser": false,
        }));
    }

    let existing_local_id = project
        .user_ids_by_phone
        .get(&verification.phone_number)
        .cloned();
    let is_new_user = existing_local_id.is_none();
    let local_id = existing_local_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    if is_new_user {
        let now = now_ms();
        let record = UserRecord {
            local_id: local_id.clone(),
            email: String::new(),
            display_name: None,
            photo_url: None,
            phone_number: Some(verification.phone_number.clone()),
            custom_attributes: None,
            disabled: false,
            email_verified: false,
            password_hash: None,
            valid_since_secs: now / 1000,
            mfa_info: Vec::new(),
            providers: vec![ProviderRecord {
                provider_id: "phone".to_string(),
                raw_id: verification.phone_number.clone(),
                email: String::new(),
                ..Default::default()
            }],
            created_at_ms: now,
            last_login_at_ms: None,
        };
        project
            .user_ids_by_phone
            .insert(verification.phone_number.clone(), local_id.clone());
        project.user_ids_by_provider.insert(
            ("phone".to_string(), verification.phone_number.clone()),
            local_id.clone(),
        );
        project.users_by_id.insert(local_id.clone(), record);
    }

    let record = project
        .users_by_id
        .get_mut(&local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if record.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    record.last_login_at_ms = Some(now_ms());
    let include_token = payload.return_secure_token.unwrap_or(true);
    let response = json!({
        "idToken": if include_token {
            make_token(&project_id, record)
        } else {
            String::default()
        },
        "refreshToken": if include_token {
            make_refresh_token(&project_id, &local_id)
        } else {
            String::default()
        },
        "expiresIn": "3600",
        "localId": local_id,
        "isNewUser": is_new_user,
        "phoneNumber": verification.phone_number,
    });
    project.verification_codes.remove(&payload.session_info);
    Ok(response)
}

fn update_phone_number(
    state: &SharedState,
    payload: PhoneUpdateRequest,
) -> Result<serde_json::Value, AuthError> {
    let claims = parse_client_token_claims(&state.auth, &payload.id_token)?;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&claims.project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    let verification = project
        .verification_codes
        .get(&payload.phone_verification_info.session_info)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"))?;
    if !matches!(verification.purpose, VerificationPurpose::FirstFactorSignIn) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"));
    }
    if verification.code != payload.phone_verification_info.code {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CODE"));
    }

    let user = project
        .users_by_id
        .get(&claims.local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if claims.issued_at < user.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    validate_phone_number_available(project, &verification.phone_number, Some(&claims.local_id))?;

    let phone_number = verification.phone_number;
    let (local_id, email, old_phone_number, id_token) = {
        let user = project
            .users_by_id
            .get_mut(&claims.local_id)
            .expect("user was checked above");
        let old_phone_number = user.phone_number.replace(phone_number.clone());
        upsert_phone_provider(user, &phone_number);
        user.last_login_at_ms = Some(now_ms());
        (
            user.local_id.clone(),
            user.email.clone(),
            old_phone_number,
            make_token(&claims.project_id, user),
        )
    };
    if let Some(old_phone_number) = old_phone_number {
        project.user_ids_by_phone.remove(&old_phone_number);
        project
            .user_ids_by_provider
            .remove(&("phone".to_string(), old_phone_number));
    }
    project
        .user_ids_by_phone
        .insert(phone_number.clone(), local_id.clone());
    project.user_ids_by_provider.insert(
        ("phone".to_string(), phone_number.clone()),
        local_id.clone(),
    );
    project
        .verification_codes
        .remove(&payload.phone_verification_info.session_info);

    let include_token = payload.return_secure_token.unwrap_or(true);
    Ok(json!({
        "kind": "identitytoolkit#SetAccountInfoResponse",
        "localId": local_id,
        "email": email,
        "phoneNumber": phone_number,
        "idToken": if include_token { id_token } else { String::new() },
        "refreshToken": if include_token {
            make_refresh_token(&claims.project_id, &claims.local_id)
        } else {
            String::new()
        },
        "expiresIn": "3600",
    }))
}

fn start_mfa_enrollment(
    state: &SharedState,
    payload: MfaEnrollmentStartRequest,
) -> Result<serde_json::Value, AuthError> {
    let claims = parse_client_token_claims(&state.auth, &payload.id_token)?;
    if !is_valid_phone_number(&payload.phone_enrollment_info.phone_number) {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_PHONE_NUMBER"));
    }
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&claims.project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    let user = project
        .users_by_id
        .get(&claims.local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if claims.issued_at < user.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    if user.mfa_info.iter().any(|factor| {
        factor.phone_info.as_deref() == Some(&payload.phone_enrollment_info.phone_number)
    }) {
        return Err(error(StatusCode::BAD_REQUEST, "SECOND_FACTOR_EXISTS"));
    }

    let session_info = create_verification_code(
        project,
        payload.phone_enrollment_info.phone_number,
        VerificationPurpose::Enrollment {
            local_id: claims.local_id,
        },
    );
    Ok(json!({
        "phoneSessionInfo": {
            "sessionInfo": session_info,
        }
    }))
}

fn finalize_mfa_enrollment(
    state: &SharedState,
    payload: MfaEnrollmentFinalizeRequest,
) -> Result<serde_json::Value, AuthError> {
    let claims = parse_client_token_claims(&state.auth, &payload.id_token)?;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&claims.project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    let verification = project
        .verification_codes
        .get(&payload.phone_verification_info.session_info)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"))?;
    let VerificationPurpose::Enrollment { local_id } = &verification.purpose else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"));
    };
    if local_id != &claims.local_id {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"));
    }
    if verification.code != payload.phone_verification_info.code {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CODE"));
    }

    let user = project
        .users_by_id
        .get_mut(&claims.local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if claims.issued_at < user.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    if user
        .mfa_info
        .iter()
        .any(|factor| factor.phone_info.as_deref() == Some(verification.phone_number.as_str()))
    {
        return Err(error(StatusCode::BAD_REQUEST, "SECOND_FACTOR_EXISTS"));
    }
    let enrollment = MfaEnrollment {
        mfa_enrollment_id: Some(Uuid::new_v4().to_string()),
        display_name: payload.display_name,
        phone_info: Some(verification.phone_number),
        unobfuscated_phone_info: None,
        enrolled_at: Some(now_rfc3339()),
    };
    user.mfa_info.push(enrollment);
    project
        .verification_codes
        .remove(&payload.phone_verification_info.session_info);

    Ok(json!({
        "idToken": make_token(&claims.project_id, user),
        "refreshToken": make_refresh_token(&claims.project_id, &claims.local_id),
        "localId": user.local_id,
        "email": user.email,
        "mfaInfo": user.mfa_info,
    }))
}

fn start_mfa_sign_in(
    state: &SharedState,
    payload: MfaSignInStartRequest,
) -> Result<serde_json::Value, AuthError> {
    let _phone_sign_in_info = payload.phone_sign_in_info;
    let project_id = project_id_for_mfa_pending(&state.auth, &payload.mfa_pending_credential)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    let pending = project
        .mfa_pending_credentials
        .get(&payload.mfa_pending_credential)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    let user = project
        .users_by_id
        .get(&pending.local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if pending.issued_at < user.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    let factor = user
        .mfa_info
        .iter()
        .find(|factor| factor.mfa_enrollment_id.as_deref() == Some(&payload.mfa_enrollment_id))
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MFA_ENROLLMENT_NOT_FOUND"))?;
    let phone_number = factor
        .phone_info
        .clone()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MFA_ENROLLMENT_NOT_FOUND"))?;
    let session_info = create_verification_code(
        project,
        phone_number,
        VerificationPurpose::SignIn {
            local_id: pending.local_id,
            mfa_pending_credential: payload.mfa_pending_credential,
            mfa_enrollment_id: payload.mfa_enrollment_id,
        },
    );
    Ok(json!({
        "phoneResponseInfo": {
            "sessionInfo": session_info,
        }
    }))
}

fn finalize_mfa_sign_in(
    state: &SharedState,
    payload: MfaSignInFinalizeRequest,
) -> Result<serde_json::Value, AuthError> {
    let project_id = project_id_for_mfa_pending(&state.auth, &payload.mfa_pending_credential)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    let verification = project
        .verification_codes
        .get(&payload.phone_verification_info.session_info)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"))?;
    let VerificationPurpose::SignIn {
        local_id,
        mfa_pending_credential,
        mfa_enrollment_id,
    } = &verification.purpose
    else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_SESSION_INFO"));
    };
    if mfa_pending_credential != &payload.mfa_pending_credential {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "INVALID_MFA_PENDING_CREDENTIAL",
        ));
    }
    if verification.code != payload.phone_verification_info.code {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CODE"));
    }
    let pending = project
        .mfa_pending_credentials
        .get(&payload.mfa_pending_credential)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_MFA_PENDING_CREDENTIAL"))?;
    if &pending.local_id != local_id {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "INVALID_MFA_PENDING_CREDENTIAL",
        ));
    }
    let user = project
        .users_by_id
        .get_mut(local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if pending.issued_at < user.valid_since_secs {
        return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
    }
    if !user
        .mfa_info
        .iter()
        .any(|factor| factor.mfa_enrollment_id.as_deref() == Some(mfa_enrollment_id))
    {
        return Err(error(StatusCode::BAD_REQUEST, "MFA_ENROLLMENT_NOT_FOUND"));
    }
    user.last_login_at_ms = Some(now_ms());
    let second_factor = Some(("phone", mfa_enrollment_id.as_str()));
    let id_token = make_token_with_second_factor(&project_id, user, second_factor);
    let refresh_token = make_refresh_token_with_second_factor(&project_id, local_id, second_factor);
    project
        .verification_codes
        .remove(&payload.phone_verification_info.session_info);
    project
        .mfa_pending_credentials
        .remove(&payload.mfa_pending_credential);

    Ok(json!({
        "idToken": id_token,
        "refreshToken": refresh_token,
        "localId": local_id,
        "email": user.email,
        "mfaInfo": user.mfa_info,
    }))
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
        let claims = parse_client_token_claims(&state.auth, &id_token)?;
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
        let project_id = state.auth.default_project_id().to_string();
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
        (Some(local_id), _) => (state.auth.default_project_id().to_string(), local_id),
        (None, Some(id_token)) => {
            let claims = parse_client_token_claims(&state.auth, &id_token)?;
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
    let project_id = state.auth.default_project_id().to_string();
    let custom_token = parse_custom_token(&payload.token)?;
    let local_id = custom_token.local_id;
    let email = format!("{local_id}@custom-token.local");
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.clone()).or_default();
    let record = get_or_create_user(project, &local_id, &email, "custom", &local_id);

    record.last_login_at_ms = Some(now_ms());
    let mut response = auth_response(&project_id, record, payload.return_secure_token);
    if payload.return_secure_token.unwrap_or(true) {
        response.id_token =
            make_token_with_custom_token_claims(&project_id, record, custom_token.claims.as_ref());
    }
    Ok(response)
}

/// Profile extracted from a fake IdP credential (unsigned JWT or JSON claims).
#[derive(Debug)]
struct IdpProfile {
    provider_id: String,
    raw_id: String,
    email: Option<String>,
    email_verified: bool,
    display_name: Option<String>,
    photo_url: Option<String>,
}

/// How a `signInWithIdp` request maps onto local accounts.
enum IdpTarget {
    /// Link the IdP identity to the user that owns the request's `idToken`.
    Link(String),
    /// A user already has this provider identity.
    Provider(String),
    /// A user with the same email exists and the IdP vouches for the email.
    VerifiedEmail(String),
    /// A user with the same email exists but the IdP email is unverified.
    NeedConfirmation(String),
    New,
}

fn is_federated_provider(provider_id: &str) -> bool {
    !matches!(provider_id, "password" | "phone" | "custom" | "anonymous")
}

fn sign_in_with_idp(
    state: &SharedState,
    payload: IdpRequest,
) -> Result<serde_json::Value, AuthError> {
    let project_id = state.auth.default_project_id().to_string();
    let params = idp_params(payload.request_uri.as_deref(), payload.post_body.as_deref());
    let provider_id = params
        .get("providerId")
        .or_else(|| params.get("provider_id"))
        .map(|provider_id| provider_id.trim().to_ascii_lowercase())
        .filter(|provider_id| !provider_id.is_empty())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_CREDENTIAL_OR_PROVIDER_ID"))?;
    let oauth_id_token = params.get("id_token").filter(|token| !token.is_empty());
    let oauth_access_token = params.get("access_token").filter(|token| !token.is_empty());
    let claims = idp_claims(
        &params,
        oauth_id_token.map(String::as_str),
        oauth_access_token.map(String::as_str),
    )?;
    let (profile, mut response) = idp_profile(&provider_id, &claims);
    response["oauthAccessToken"] = json!(oauth_access_token
        .cloned()
        .unwrap_or_else(|| format!("FirebaseAuthEmulatorFakeAccessToken_{provider_id}")));
    if let Some(oauth_id_token) = oauth_id_token {
        response["oauthIdToken"] = json!(oauth_id_token);
    }
    let linking_claims = payload
        .id_token
        .as_deref()
        .map(|id_token| parse_client_token_claims(&state.auth, id_token))
        .transpose()?;

    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects.entry(project_id.clone()).or_default();
    let target = match resolve_idp_target(project, &profile, linking_claims)? {
        Ok(target) => target,
        Err(message) if payload.return_idp_credential.unwrap_or(false) => {
            response["errorMessage"] = json!(message);
            return Ok(response);
        }
        Err(message) => return Err(error(StatusCode::BAD_REQUEST, message)),
    };

    let mut is_new_user = false;
    let local_id = match target {
        IdpTarget::NeedConfirmation(local_id) => {
            let verified_providers = project
                .users_by_id
                .get(&local_id)
                .map(|record| {
                    record
                        .providers
                        .iter()
                        .filter(|provider| is_federated_provider(&provider.provider_id))
                        .map(|provider| provider.provider_id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            response["needConfirmation"] = json!(true);
            response["localId"] = json!(local_id);
            response["verifiedProvider"] = json!(verified_providers);
            return Ok(response);
        }
        IdpTarget::New => {
            is_new_user = true;
            create_idp_user(project, &profile)
        }
        IdpTarget::Provider(local_id) => local_id,
        IdpTarget::Link(local_id) => {
            link_idp_email(project, &local_id, &profile);
            local_id
        }
        IdpTarget::VerifiedEmail(local_id) => {
            if adopt_verified_idp_email(project, &local_id, &profile) {
                response["emailRecycled"] = json!(true);
            }
            local_id
        }
    };
    upsert_idp_provider(project, &local_id, &profile);

    let record = project
        .users_by_id
        .get_mut(&local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if record.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    if profile.email.as_deref() == Some(record.email.as_str()) {
        response["emailVerified"] = json!(record.email_verified);
    }
    response["localId"] = json!(local_id);
    response["isNewUser"] = json!(is_new_user);

    if !record.mfa_info.is_empty() {
        let mfa_info = record.mfa_info.clone();
        let pending_credential = format!("firelite-mfa-pending-{}", Uuid::new_v4());
        project.mfa_pending_credentials.insert(
            pending_credential.clone(),
            MfaPendingCredential {
                local_id,
                issued_at: now_secs(),
            },
        );
        response["mfaPendingCredential"] = json!(pending_credential);
        response["mfaInfo"] = json!(mfa_info);
        return Ok(response);
    }

    record.last_login_at_ms = Some(now_ms());
    if payload.return_secure_token.unwrap_or(true) {
        response["idToken"] = json!(make_token_with_second_factor_and_claims(
            &project_id,
            record,
            None,
            None,
            Some(&provider_id),
        ));
        response["refreshToken"] = json!(make_refresh_token(&project_id, &local_id));
        response["expiresIn"] = json!("3600");
    }
    Ok(response)
}

/// Merges IdP parameters from the request URI query, `postBody`, and URI
/// fragment, in that order of precedence (later wins), like the official
/// emulator.
fn idp_params(request_uri: Option<&str>, post_body: Option<&str>) -> HashMap<String, String> {
    let request_uri = request_uri.unwrap_or_default();
    let (without_fragment, fragment) = request_uri
        .split_once('#')
        .map_or((request_uri, None), |(uri, fragment)| (uri, Some(fragment)));
    let mut params = HashMap::new();
    if let Some((_, query)) = without_fragment.split_once('?') {
        params.extend(parse_post_body(query));
    }
    if let Some(post_body) = post_body {
        params.extend(parse_post_body(post_body));
    }
    if let Some(fragment) = fragment {
        params.extend(parse_post_body(fragment));
    }
    params
}

fn idp_claims(
    params: &HashMap<String, String>,
    id_token: Option<&str>,
    access_token: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, AuthError> {
    for token in [id_token, access_token].into_iter().flatten() {
        if let Some(claims) = parse_idp_token_claims(token)? {
            return Ok(claims);
        }
    }
    if id_token.is_some() {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_IDP_RESPONSE"));
    }

    // Tolerant legacy form: an explicit raw ID (or opaque OAuth token) and an
    // optional email in the post body.
    let raw_id = params
        .get("rawId")
        .or_else(|| params.get("raw_id"))
        .map(String::as_str)
        .or(access_token)
        .or_else(|| params.get("oauth_token").map(String::as_str))
        .filter(|raw_id| !raw_id.is_empty())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_IDP_RESPONSE"))?;
    let mut claims = serde_json::Map::new();
    claims.insert("sub".to_string(), json!(raw_id));
    if let Some(email) = params.get("email") {
        claims.insert("email".to_string(), json!(email));
    }
    Ok(claims)
}

/// Parses a fake IdP token: strict JSON claims or an (unverified) JWT.
/// Returns `None` for opaque tokens that are neither.
fn parse_idp_token_claims(
    token: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, AuthError> {
    let invalid = || error(StatusCode::BAD_REQUEST, "INVALID_IDP_RESPONSE");
    let token = token.trim();
    let value = if token.starts_with('{') {
        serde_json::from_str::<serde_json::Value>(token).map_err(|_| invalid())?
    } else {
        let decoded = token
            .split('.')
            .nth(1)
            .and_then(|payload| URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok())
            .and_then(|payload| serde_json::from_slice::<serde_json::Value>(&payload).ok());
        match decoded {
            Some(value) => value,
            None => return Ok(None),
        }
    };
    let serde_json::Value::Object(claims) = value else {
        return Err(invalid());
    };
    match claims.get("sub") {
        Some(serde_json::Value::String(sub)) if !sub.is_empty() => Ok(Some(claims)),
        _ => Err(invalid()),
    }
}

fn idp_profile(
    provider_id: &str,
    claims: &serde_json::Map<String, serde_json::Value>,
) -> (IdpProfile, serde_json::Value) {
    let string_claim = |name: &str| {
        claims
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    let raw_id = string_claim("sub").expect("IdP claims are validated to contain sub");
    let email = string_claim("email").map(|email| normalize_email(&email));
    let email_verified = match claims.get("email_verified") {
        Some(serde_json::Value::Bool(verified)) => *verified,
        Some(serde_json::Value::String(verified)) => verified == "true",
        _ => false,
    };
    let display_name = string_claim("name");
    let photo_url = string_claim("picture");
    let screen_name = string_claim("screen_name");

    let mut response = json!({
        "kind": "identitytoolkit#VerifyAssertionResponse",
        "context": "",
        "providerId": provider_id,
        "emailVerified": email_verified,
    });
    let (federated_id, raw_user_info) = if provider_id == "google.com" {
        let given_name = string_claim("given_name");
        let family_name = string_claim("family_name");
        let mut granted_scopes =
            "openid https://www.googleapis.com/auth/userinfo.profile".to_string();
        if email.is_some() {
            granted_scopes.push_str(" https://www.googleapis.com/auth/userinfo.email");
        }
        let raw_user_info = json!({
            "granted_scopes": granted_scopes,
            "id": raw_id,
            "name": display_name,
            "given_name": given_name,
            "family_name": family_name,
            "verified_email": email_verified,
            "locale": "en",
            "email": email,
            "picture": photo_url,
        });
        if let Some(given_name) = given_name {
            response["firstName"] = json!(given_name);
        }
        if let Some(family_name) = family_name {
            response["lastName"] = json!(family_name);
        }
        (
            format!("https://accounts.google.com/{raw_id}"),
            raw_user_info,
        )
    } else {
        (raw_id.clone(), serde_json::Value::Object(claims.clone()))
    };
    response["federatedId"] = json!(federated_id);
    response["rawUserInfo"] = json!(raw_user_info.to_string());
    for (field, value) in [
        ("displayName", &display_name),
        ("fullName", &display_name),
        ("screenName", &screen_name),
        ("email", &email),
        ("photoUrl", &photo_url),
    ] {
        if let Some(value) = value {
            response[field] = json!(value);
        }
    }

    let profile = IdpProfile {
        provider_id: provider_id.to_string(),
        raw_id,
        email,
        email_verified,
        display_name,
        photo_url,
    };
    (profile, response)
}

/// Picks the account an IdP sign-in applies to. The outer error is a hard
/// failure; the inner error is a conflict that `returnIdpCredential` callers
/// receive as `errorMessage` instead.
fn resolve_idp_target(
    project: &ProjectAuthState,
    profile: &IdpProfile,
    linking_claims: Option<TokenClaims>,
) -> Result<Result<IdpTarget, &'static str>, AuthError> {
    let existing = |local_id: Option<&String>| {
        local_id
            .filter(|local_id| project.users_by_id.contains_key(*local_id))
            .cloned()
    };
    let user_matching_provider = existing(
        project
            .user_ids_by_provider
            .get(&(profile.provider_id.clone(), profile.raw_id.clone())),
    );
    let user_matching_email = existing(
        profile
            .email
            .as_ref()
            .and_then(|email| project.user_ids_by_email.get(email)),
    );

    if let Some(claims) = linking_claims {
        let record = project
            .users_by_id
            .get(&claims.local_id)
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
        if claims.issued_at < record.valid_since_secs {
            return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
        }
        if user_matching_provider.is_some() {
            return Ok(Err("FEDERATED_USER_ID_ALREADY_LINKED"));
        }
        if user_matching_email.is_some_and(|local_id| local_id != claims.local_id) {
            return Ok(Err("EMAIL_EXISTS"));
        }
        return Ok(Ok(IdpTarget::Link(claims.local_id)));
    }
    Ok(Ok(match (user_matching_provider, user_matching_email) {
        (Some(local_id), _) => IdpTarget::Provider(local_id),
        (None, Some(local_id)) if profile.email_verified => IdpTarget::VerifiedEmail(local_id),
        (None, Some(local_id)) => IdpTarget::NeedConfirmation(local_id),
        (None, None) => IdpTarget::New,
    }))
}

fn create_idp_user(project: &mut ProjectAuthState, profile: &IdpProfile) -> String {
    let local_id = Uuid::new_v4().to_string();
    let now = now_ms();
    let email = profile.email.clone().unwrap_or_default();
    if !email.is_empty() {
        project
            .user_ids_by_email
            .insert(email.clone(), local_id.clone());
    }
    project.users_by_id.insert(
        local_id.clone(),
        UserRecord {
            local_id: local_id.clone(),
            email_verified: !email.is_empty() && profile.email_verified,
            email,
            display_name: profile.display_name.clone(),
            photo_url: profile.photo_url.clone(),
            phone_number: None,
            custom_attributes: None,
            disabled: false,
            password_hash: None,
            valid_since_secs: now / 1000,
            mfa_info: Vec::new(),
            providers: Vec::new(),
            created_at_ms: now,
            last_login_at_ms: None,
        },
    );
    local_id
}

/// Adopts the IdP email for a user linking a provider.
fn link_idp_email(project: &mut ProjectAuthState, local_id: &str, profile: &IdpProfile) {
    let Some(record) = project.users_by_id.get_mut(local_id) else {
        return;
    };
    let Some(email) = &profile.email else {
        return;
    };
    if record.email.is_empty() {
        record.email = email.clone();
        record.email_verified = profile.email_verified;
        project
            .user_ids_by_email
            .insert(email.clone(), local_id.to_string());
    } else if &record.email == email && profile.email_verified {
        record.email_verified = true;
    }
}

/// Signs a trusted-IdP user into the existing account with the same email.
/// An account whose email was never verified is taken over: its password,
/// phone number, other providers, and sessions are dropped, as in production.
/// Returns whether the email was recycled from another identity of the same
/// provider.
fn adopt_verified_idp_email(
    project: &mut ProjectAuthState,
    local_id: &str,
    profile: &IdpProfile,
) -> bool {
    let Some(record) = project.users_by_id.get_mut(local_id) else {
        return false;
    };
    let email_recycled = record.providers.iter().any(|provider| {
        provider.provider_id == profile.provider_id && provider.raw_id != profile.raw_id
    });
    if !record.email_verified {
        record.password_hash = None;
        if let Some(phone_number) = record.phone_number.take() {
            project.user_ids_by_phone.remove(&phone_number);
        }
        record.valid_since_secs = now_secs();
        for provider in record.providers.drain(..) {
            project
                .user_ids_by_provider
                .remove(&(provider.provider_id, provider.raw_id));
        }
    }
    if profile.display_name.is_some() {
        record.display_name = profile.display_name.clone();
    }
    if profile.photo_url.is_some() {
        record.photo_url = profile.photo_url.clone();
    }
    record.email_verified = true;
    email_recycled
}

fn upsert_idp_provider(project: &mut ProjectAuthState, local_id: &str, profile: &IdpProfile) {
    let Some(record) = project.users_by_id.get_mut(local_id) else {
        return;
    };
    let mut provider = ProviderRecord {
        provider_id: profile.provider_id.clone(),
        raw_id: profile.raw_id.clone(),
        email: profile.email.clone().unwrap_or_default(),
        display_name: profile.display_name.clone(),
        photo_url: profile.photo_url.clone(),
    };
    match record
        .providers
        .iter_mut()
        .find(|existing| existing.provider_id == profile.provider_id)
    {
        Some(existing) => {
            if existing.raw_id == profile.raw_id {
                // Credentials that omit profile claims keep what we already know.
                provider.display_name = provider.display_name.or(existing.display_name.take());
                provider.photo_url = provider.photo_url.or(existing.photo_url.take());
                if provider.email.is_empty() {
                    provider.email = std::mem::take(&mut existing.email);
                }
            } else {
                project
                    .user_ids_by_provider
                    .remove(&(existing.provider_id.clone(), existing.raw_id.clone()));
            }
            *existing = provider;
        }
        None => record.providers.push(provider),
    }
    project.user_ids_by_provider.insert(
        (profile.provider_id.clone(), profile.raw_id.clone()),
        local_id.to_string(),
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdpWidgetQuery {
    api_key: Option<String>,
    provider_id: Option<String>,
}

/// Fake IdP sign-in page the Firebase JS SDK opens for `signInWithPopup` and
/// `signInWithRedirect` when connected to the Auth emulator.
async fn idp_widget(
    State(state): State<SharedState>,
    Query(query): Query<IdpWidgetQuery>,
) -> Response {
    let (Some(_), Some(provider_id)) = (query.api_key, query.provider_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "authEmulator": {
                    "error": "missing apiKey or providerId query parameters",
                },
            })),
        )
            .into_response();
    };
    let mut accounts = {
        let projects = state.auth.projects.read().expect("auth state poisoned");
        projects
            .get(state.auth.default_project_id())
            .map(|project| {
                project
                    .users_by_id
                    .values()
                    .filter_map(|user| {
                        let provider = user
                            .providers
                            .iter()
                            .find(|provider| provider.provider_id == provider_id)?;
                        let email = Some(provider.email.as_str())
                            .filter(|email| !email.is_empty())
                            .or((!user.email.is_empty()).then_some(user.email.as_str()))
                            .map(ToOwned::to_owned);
                        Some(IdpWidgetAccount {
                            raw_id: provider.raw_id.clone(),
                            email,
                            display_name: provider
                                .display_name
                                .clone()
                                .or_else(|| user.display_name.clone()),
                            photo_url: provider
                                .photo_url
                                .clone()
                                .or_else(|| user.photo_url.clone()),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    accounts.sort_by(|left, right| {
        (&left.email, &left.display_name, &left.raw_id).cmp(&(
            &right.email,
            &right.display_name,
            &right.raw_id,
        ))
    });
    Html(render_idp_widget(&provider_id, &accounts)).into_response()
}

async fn idp_iframe() -> Html<&'static str> {
    Html(IDP_IFRAME_HTML)
}

struct IdpWidgetAccount {
    raw_id: String,
    email: Option<String>,
    display_name: Option<String>,
    photo_url: Option<String>,
}

fn render_idp_widget(provider_id: &str, accounts: &[IdpWidgetAccount]) -> String {
    let accounts_html = if accounts.is_empty() {
        format!(
            r#"<p class="empty">No {} accounts exist in the Firelite Auth emulator yet.</p>"#,
            escape_html(provider_id)
        )
    } else {
        let items = accounts
            .iter()
            .map(|account| {
                let claims = json!({
                    "sub": account.raw_id,
                    "iss": "",
                    "aud": "",
                    "exp": 0,
                    "iat": 0,
                    "name": account.display_name,
                    "email": account.email,
                    "email_verified": true,
                    "picture": account.photo_url,
                });
                let label = account
                    .display_name
                    .as_deref()
                    .unwrap_or("(No display name)");
                let initial = label
                    .chars()
                    .find(|character| character.is_alphanumeric())
                    .map(|character| character.to_uppercase().to_string())
                    .unwrap_or_else(|| "?".to_string());
                let avatar = match &account.photo_url {
                    Some(photo_url) => format!(
                        r#"<img class="avatar" alt="" src="{}">"#,
                        escape_html(photo_url)
                    ),
                    None => format!(
                        r#"<span class="avatar" aria-hidden="true">{}</span>"#,
                        escape_html(&initial)
                    ),
                };
                format!(
                    r#"<li><button type="button" class="account js-reuse-account" data-id-token="{token}" data-email="{email}">{avatar}<span class="account-text"><span class="account-name">{label}</span><span class="account-email">{email}</span></span></button></li>"#,
                    token = escape_html(&percent_encode_query(&claims.to_string())),
                    email = escape_html(account.email.as_deref().unwrap_or_default()),
                    label = escape_html(label),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(r#"<ul class="accounts">{items}</ul>"#)
    };
    IDP_WIDGET_HTML
        .replace("{{PROVIDER_ID}}", &escape_html(provider_id))
        .replace("{{ACCOUNTS}}", &accounts_html)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

const IDP_WIDGET_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in with {{PROVIDER_ID}} · Firelite Auth emulator</title>
<style>
  :root { color-scheme: light dark; --bg: #f6f7f9; --card: #fff; --text: #1d2025; --muted: #646b75; --line: #dde1e6; --accent: #1a73e8; --hover: #eef3fd; }
  @media (prefers-color-scheme: dark) { :root { --bg: #15171a; --card: #1e2125; --text: #e8eaed; --muted: #9aa0a6; --line: #34383d; --accent: #8ab4f8; --hover: #263042; } }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; background: var(--bg); color: var(--text); font: 15px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; display: flex; justify-content: center; padding: 32px 16px; }
  main { width: 100%; max-width: 440px; background: var(--card); border: 1px solid var(--line); border-radius: 12px; padding: 24px; align-self: flex-start; }
  h1 { font-size: 20px; margin: 0 0 4px; }
  .subtitle { color: var(--muted); margin: 0 0 16px; }
  .accounts { list-style: none; margin: 0 0 12px; padding: 0; border-top: 1px solid var(--line); }
  .accounts li { border-bottom: 1px solid var(--line); }
  .account { display: flex; gap: 12px; align-items: center; width: 100%; padding: 10px 8px; border: 0; background: none; color: inherit; font: inherit; text-align: left; cursor: pointer; }
  .account:hover, .account:focus-visible { background: var(--hover); outline: none; }
  .avatar { flex: none; width: 36px; height: 36px; border-radius: 50%; object-fit: cover; display: inline-flex; align-items: center; justify-content: center; background: var(--accent); color: var(--card); font-weight: 600; }
  .account-text { display: flex; flex-direction: column; min-width: 0; }
  .account-name, .account-email { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .account-email { color: var(--muted); font-size: 13px; }
  .empty { color: var(--muted); }
  button.primary, button.secondary { font: inherit; border-radius: 6px; padding: 8px 14px; cursor: pointer; }
  button.primary { background: var(--accent); color: var(--card); border: 1px solid var(--accent); font-weight: 600; }
  button.secondary { background: none; color: var(--accent); border: 1px solid var(--line); }
  form { display: none; flex-direction: column; gap: 12px; }
  form.open { display: flex; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  input { font: inherit; color: var(--text); background: var(--bg); border: 1px solid var(--line); border-radius: 6px; padding: 8px 10px; }
  input[aria-invalid="true"] { border-color: #d93025; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; justify-content: space-between; }
  .actions > div { display: flex; gap: 8px; }
</style>
</head>
<body>
<main>
  <h1>Sign in with {{PROVIDER_ID}}</h1>
  <p class="subtitle">Firelite Auth emulator: fake accounts only, no real {{PROVIDER_ID}} sign-in happens here.</p>
  <section class="js-accounts">
    {{ACCOUNTS}}
    <button type="button" class="secondary js-new-account">+ Add new account</button>
  </section>
  <form class="js-form" novalidate>
    <label>Email
      <input type="email" name="email" autocomplete="off" required>
    </label>
    <label>Display name (optional)
      <input type="text" name="displayName" autocomplete="off">
    </label>
    <label>Profile photo URL (optional)
      <input type="url" name="photoUrl" autocomplete="off">
    </label>
    <div class="actions">
      <button type="button" class="secondary js-autogen">Auto-generate</button>
      <div>
        <button type="button" class="secondary js-back">Back</button>
        <button type="submit" class="primary">Sign in</button>
      </div>
    </div>
  </form>
</main>
<script>
  function fail(message) {
    document.body.textContent = 'Firelite Auth emulator error: ' + message;
    document.body.style.cssText = 'color:#d93025;white-space:pre-wrap;padding:16px;font-family:monospace';
    throw new Error(message);
  }

  var query = new URLSearchParams(location.search);
  if (query.get('error')) fail(query.get('error'));
  var apiKey = query.get('apiKey');
  var appName = query.get('appName');
  var tenantId = query.get('tid');
  var authType = query.get('authType');
  var providerId = query.get('providerId');
  var redirectUrl = query.get('redirectUrl');
  var eventId = query.get('eventId');
  var storageKey = apiKey + ':' + appName;
  if (!appName) fail('Missing the appName query parameter. Firelite supports the web SDK popup and redirect flows only.');

  function saveAuthEvent(authEvent) {
    if (/popup/i.test(authType)) {
      sendAuthEventViaIframeRelay(authEvent);
    } else if (redirectUrl) {
      sessionStorage['firebase:redirectEvent:' + storageKey] = JSON.stringify(authEvent);
      window.location = redirectUrl;
    } else {
      fail('Missing redirectUrl. Use signInWithCredential for this platform instead.');
    }
  }

  function sendAuthEventViaIframeRelay(authEvent) {
    var sent = false;
    if (window.opener) {
      for (var i = 0; i < window.opener.frames.length; i++) {
        try {
          var frame = window.opener.frames[i];
          var frameQuery = new URLSearchParams(frame.location.search);
          if (frameQuery.get('apiKey') === apiKey && frameQuery.get('appName') === appName) {
            frame.postMessage({
              data: { authEvent: authEvent, storageKey: storageKey },
              eventId: Math.floor(Math.random() * Math.pow(10, 20)).toString(),
              eventType: 'sendAuthEvent'
            }, '*');
            sent = true;
          }
        } catch (_) {
          // Frames from other origins throw; skip them.
        }
      }
    }
    if (!sent) fail('Could not find the Auth emulator helper iframe in the opener window.');
  }

  function finishWithUser(urlEncodedIdToken) {
    var url = window.location.href.split('?')[0] +
      '?providerId=' + encodeURIComponent(providerId) +
      '&id_token=' + urlEncodedIdToken;
    saveAuthEvent({
      type: authType,
      eventId: eventId,
      urlResponse: url,
      sessionId: 'ValueNotUsedByAuthEmulator',
      postBody: '',
      tenantId: tenantId,
      error: null
    });
  }

  document.querySelectorAll('.js-reuse-account').forEach(function (button) {
    button.addEventListener('click', function () {
      finishWithUser(button.dataset.idToken);
    });
  });

  var form = document.querySelector('.js-form');
  var accounts = document.querySelector('.js-accounts');
  function toggleForm(open) {
    form.classList.toggle('open', open);
    accounts.hidden = open;
    if (open) form.elements.email.focus();
  }
  document.querySelector('.js-new-account').addEventListener('click', function () { toggleForm(true); });
  document.querySelector('.js-back').addEventListener('click', function () { toggleForm(false); });

  function randomDigits(length) {
    var digits = '';
    for (var i = 0; i < length; i++) digits += Math.floor(Math.random() * 10).toString();
    return digits;
  }

  var adjectives = ['amber', 'brisk', 'calm', 'dusky', 'eager', 'fuzzy', 'gentle', 'hazel', 'jolly', 'lucky'];
  var animals = ['otter', 'falcon', 'lynx', 'heron', 'badger', 'gecko', 'panda', 'koala', 'raven', 'tapir'];
  document.querySelector('.js-autogen').addEventListener('click', function () {
    var adjective = adjectives[Math.floor(Math.random() * adjectives.length)];
    var animal = animals[Math.floor(Math.random() * animals.length)];
    form.elements.displayName.value = adjective[0].toUpperCase() + adjective.slice(1) + ' ' + animal[0].toUpperCase() + animal.slice(1);
    form.elements.email.value = adjective + '.' + animal + '.' + randomDigits(3) + '@example.com';
    form.elements.email.removeAttribute('aria-invalid');
  });

  form.addEventListener('submit', function (event) {
    event.preventDefault();
    var email = form.elements.email.value.trim();
    if (!/^[^@\s]+@[^@\s]+$/.test(email)) {
      form.elements.email.setAttribute('aria-invalid', 'true');
      form.elements.email.focus();
      return;
    }
    var claims = {
      sub: randomDigits(21),
      iss: '',
      aud: '',
      exp: 0,
      iat: 0,
      email: email,
      email_verified: true
    };
    if (form.elements.displayName.value.trim()) claims.name = form.elements.displayName.value.trim();
    if (form.elements.photoUrl.value.trim()) claims.picture = form.elements.photoUrl.value.trim();
    finishWithUser(encodeURIComponent(JSON.stringify(claims)));
  });
</script>
</body>
</html>
"#;

/// Helper iframe the Firebase JS SDK embeds to receive popup and redirect
/// results from the emulator widget. It speaks the `gapi.iframes` protocol the
/// SDK expects, so it loads `gapi` from apis.google.com like the official
/// emulator does.
const IDP_IFRAME_HTML: &str = r#"<!DOCTYPE html>
<meta charset="utf-8">
<title>Firelite Auth emulator helper iframe</title>
<script>
  var query = new URLSearchParams(location.search);
  var apiKey = query.get('apiKey');
  var appName = query.get('appName');
  if (!apiKey || !appName) {
    console.error('Firelite Auth emulator: missing apiKey or appName for the helper iframe.');
  }
  var storageKey = apiKey + ':' + appName;
  var redirectEventKey = 'firebase:redirectEvent:' + storageKey;
  var parentContainer = null;

  window.addEventListener('message', function (event) {
    if (typeof event.data !== 'object' || !event.data || event.data.eventType !== 'sendAuthEvent') {
      return;
    }
    if (event.data.data.storageKey !== storageKey) {
      return console.error('Firelite Auth emulator: auth event storage key mismatch.');
    }
    var authEvent = event.data.data.authEvent;
    if (parentContainer) {
      sendAuthEvent(authEvent);
    } else {
      // initFrameMessaging() picks this up once gapi has loaded.
      sessionStorage[redirectEventKey] = JSON.stringify(authEvent);
    }
  });

  function initFrameMessaging() {
    parentContainer = gapi.iframes.getContext().getParentIframe();
    parentContainer.register('webStorageSupport', function () {
      // The SDK waits for this reply before continuing the popup flow.
      return { status: 'ACK', webStorageSupport: true };
    }, gapi.iframes.CROSS_ORIGIN_IFRAMES_FILTER);

    var authEvent = null;
    var storedEvent = sessionStorage[redirectEventKey];
    if (storedEvent) {
      try {
        authEvent = JSON.parse(storedEvent);
      } catch (_) {
        console.error('Firelite Auth emulator: invalid stored auth event.');
      }
    }
    sendAuthEvent(authEvent);
    delete sessionStorage[redirectEventKey];
  }

  function sendAuthEvent(authEvent) {
    parentContainer.send('authEvent', {
      type: 'authEvent',
      authEvent: authEvent || { type: 'unknown', error: { code: 'auth/no-auth-event' } }
    }, function (responses) {
      if (!responses || !responses.length || responses[responses.length - 1].status !== 'ACK') {
        console.error('Firelite Auth emulator: sending the auth event failed.');
      }
    }, gapi.iframes.CROSS_ORIGIN_IFRAMES_FILTER);
  }

  window.gapi_onload = function () {
    gapi.load('gapi.iframes', {
      callback: initFrameMessaging,
      timeout: 10000,
      ontimeout: function () {
        console.error('Firelite Auth emulator: timed out loading gapi.iframes. Check your internet connection.');
      }
    });
  };
</script>
<script src="https://apis.google.com/js/api.js"></script>
"#;

fn send_oob_code(
    state: &SharedState,
    project_id: &str,
    auth_base_url: &str,
    payload: SendOobCodeRequest,
) -> Result<OobCodeResponse, AuthError> {
    if payload.request_type != "EMAIL_SIGNIN"
        && payload.request_type != "PASSWORD_RESET"
        && payload.request_type != "VERIFY_EMAIL"
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OOB_REQUEST_TYPE",
        ));
    }

    let email = if payload.request_type == "VERIFY_EMAIL" {
        let id_token = payload
            .id_token
            .as_deref()
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_ID_TOKEN"))?;
        let claims = parse_client_token_claims(&state.auth, id_token)?;
        let projects = state.auth.projects.read().expect("auth state poisoned");
        let user = projects
            .get(&claims.project_id)
            .and_then(|project| project.users_by_id.get(&claims.local_id))
            .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
        if user.disabled {
            return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
        }
        if claims.issued_at < user.valid_since_secs {
            return Err(error(StatusCode::BAD_REQUEST, "TOKEN_EXPIRED"));
        }
        user.email.clone()
    } else {
        normalize_email(
            payload
                .email
                .as_deref()
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "MISSING_EMAIL"))?,
        )
    };
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
    let link = make_oob_link(
        auth_base_url,
        project_id,
        &payload.request_type,
        &code,
        payload.continue_url.as_deref(),
    );
    log_email_action(&payload.request_type, &email, &code, &link);
    let oob_link = payload.return_oob_link.unwrap_or(false).then_some(link);

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
    let project_id = state.auth.default_project_id().to_string();
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
    record.email_verified = true;

    Ok(auth_response(
        &project_id,
        record,
        payload.return_secure_token,
    ))
}

fn apply_action_code(
    state: &SharedState,
    payload: ApplyActionCodeRequest,
) -> Result<EmptyResponse, AuthError> {
    let project_id = state.auth.default_project_id().to_string();
    let mut projects = state.auth.projects.write().expect("auth state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"))?;
    let code = project
        .oob_codes
        .get(&payload.oob_code)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"))?;
    if code.request_type != "VERIFY_EMAIL" {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_OOB_CODE"));
    }
    let local_id = project
        .user_ids_by_email
        .get(&normalize_email(&code.email))
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    let user = project
        .users_by_id
        .get_mut(&local_id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"))?;
    if user.disabled {
        return Err(error(StatusCode::BAD_REQUEST, "USER_DISABLED"));
    }
    user.email_verified = true;
    project.oob_codes.remove(&payload.oob_code);
    Ok(EmptyResponse {})
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

async fn list_verification_codes(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> AuthResult<ListVerificationCodesResponse> {
    let projects = state.auth.projects.read().expect("auth state poisoned");
    let verification_codes = projects
        .get(&project_id)
        .map(|project| {
            let mut codes = project
                .verification_codes
                .iter()
                .map(|(session_info, record)| {
                    (
                        record.sequence,
                        EmulatorVerificationCode {
                            phone_number: record.phone_number.clone(),
                            session_info: session_info.clone(),
                            code: record.code.clone(),
                        },
                    )
                })
                .collect::<Vec<_>>();
            codes.sort_by_key(|(sequence, _)| *sequence);
            codes.into_iter().map(|(_, code)| code).collect()
        })
        .unwrap_or_default();
    Ok(Json(ListVerificationCodesResponse { verification_codes }))
}

fn delete_account(auth: &AuthState, project_id: &str, local_id: &str) -> Result<(), AuthError> {
    let mut projects = auth.projects.write().expect("auth state poisoned");
    let Some(project) = projects.get_mut(project_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };
    let Some(record) = project.users_by_id.remove(local_id) else {
        return Err(error(StatusCode::BAD_REQUEST, "USER_NOT_FOUND"));
    };
    if !record.email.is_empty() {
        project
            .user_ids_by_email
            .remove(&normalize_email(&record.email));
    }
    if let Some(phone_number) = record.phone_number {
        project.user_ids_by_phone.remove(&phone_number);
    }
    for provider in record.providers {
        project
            .user_ids_by_provider
            .remove(&(provider.provider_id, provider.raw_id));
    }
    project
        .mfa_pending_credentials
        .retain(|_, pending| pending.local_id != local_id);
    project
        .verification_codes
        .retain(|_, verification| match &verification.purpose {
            VerificationPurpose::FirstFactorSignIn => true,
            VerificationPurpose::Enrollment {
                local_id: verification_user,
            }
            | VerificationPurpose::SignIn {
                local_id: verification_user,
                ..
            } => verification_user != local_id,
        });
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
            ..Default::default()
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
    continue_url: Option<&str>,
) -> String {
    let mode = match request_type {
        "PASSWORD_RESET" => "resetPassword",
        "EMAIL_SIGNIN" => "signIn",
        "VERIFY_EMAIL" => "verifyEmail",
        _ => "action",
    };
    if request_type == "EMAIL_SIGNIN" {
        if let Some(continue_url) = continue_url {
            let (base, fragment) = continue_url
                .split_once('#')
                .map_or((continue_url, None), |(base, fragment)| {
                    (base, Some(fragment))
                });
            let separator = if base.contains('?') { '&' } else { '?' };
            let fragment = fragment.map_or(String::new(), |value| format!("#{value}"));
            return format!(
                "{base}{separator}mode={mode}&oobCode={}&apiKey=fake{fragment}",
                percent_encode_query(oob_code)
            );
        }
    }
    format!(
        "{auth_base_url}/emulator/action?mode={mode}&oobCode={}&projectId={}",
        percent_encode_query(oob_code),
        percent_encode_query(project_id)
    )
}

fn log_email_action(request_type: &str, email: &str, code: &str, link: &str) {
    let label = match request_type {
        "EMAIL_SIGNIN" => "magic sign-in link",
        "VERIFY_EMAIL" => "email verification link",
        "PASSWORD_RESET" => "password reset link",
        _ => "email action link",
    };
    let message = if auth_log_colors_enabled() {
        format!(
            "\u{1b}[1;36mAUTH {label}\u{1b}[0m \u{1b}[2mfor\u{1b}[0m {email}\n  \u{1b}[1;35m{link}\u{1b}[0m\n  \u{1b}[2;33mcode: {code}\u{1b}[0m"
        )
    } else {
        format!("AUTH {label} for {email}\n  {link}\n  code: {code}")
    };
    eprintln!("{message}");
}

fn log_sms_code(phone_number: &str, code: &str) {
    let message = if auth_log_colors_enabled() {
        format!(
            "\u{1b}[1;36mAUTH SMS verification code\u{1b}[0m \u{1b}[2mfor\u{1b}[0m {phone_number}: \u{1b}[1;33m{code}\u{1b}[0m"
        )
    } else {
        format!("AUTH SMS verification code for {phone_number}: {code}")
    };
    eprintln!("{message}");
}

fn auth_log_colors_enabled() -> bool {
    let forced = std::env::var("FORCE_COLOR")
        .ok()
        .is_some_and(|value| value != "0");
    forced
        || (std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").ok().as_deref() != Some("dumb")
            && std::io::stderr().is_terminal())
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

struct CustomToken {
    local_id: String,
    claims: Option<serde_json::Map<String, serde_json::Value>>,
}

fn parse_custom_token(token: &str) -> Result<CustomToken, AuthError> {
    if !token.contains('.') {
        return Ok(CustomToken {
            local_id: token.to_string(),
            claims: None,
        });
    }

    let Some(payload) = token.split('.').nth(1) else {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))?;
    let local_id = value
        .get("uid")
        .or_else(|| value.get("sub"))
        .or_else(|| value.get("user_id"))
        .and_then(|sub| sub.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "INVALID_CUSTOM_TOKEN"))?;
    let claims = value
        .get("claims")
        .and_then(|claims| claims.as_object())
        .cloned();

    Ok(CustomToken { local_id, claims })
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

fn auth_response(
    project_id: &str,
    record: &UserRecord,
    return_secure_token: Option<bool>,
) -> AuthResponse {
    let include_token = return_secure_token.unwrap_or(true);
    AuthResponse {
        kind: "identitytoolkit#SignupNewUserResponse",
        local_id: record.local_id.clone(),
        email: (!record.email.is_empty()).then(|| record.email.clone()),
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
    make_refresh_token_with_second_factor(project_id, local_id, None)
}

fn make_refresh_token_with_second_factor(
    project_id: &str,
    local_id: &str,
    second_factor: Option<(&str, &str)>,
) -> String {
    let mut value = json!({
        "project_id": project_id,
        "local_id": local_id,
        "iat": now_secs(),
        "nonce": Uuid::new_v4(),
    });
    if let Some((factor, identifier)) = second_factor {
        value["second_factor"] = json!(factor);
        value["second_factor_identifier"] = json!(identifier);
    }
    let payload = URL_SAFE_NO_PAD.encode(value.to_string());
    format!("firelite-refresh.{payload}")
}

struct RefreshTokenClaims {
    project_id: String,
    local_id: String,
    issued_at: u64,
    second_factor: Option<String>,
    second_factor_identifier: Option<String>,
}

fn parse_refresh_token(token: &str) -> Result<RefreshTokenClaims, AuthError> {
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
    Ok(RefreshTokenClaims {
        project_id: project_id.to_string(),
        local_id: local_id.to_string(),
        issued_at,
        second_factor: value
            .get("second_factor")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        second_factor_identifier: value
            .get("second_factor_identifier")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
    })
}

fn make_token(project_id: &str, record: &UserRecord) -> String {
    make_token_with_second_factor_and_claims(project_id, record, None, None, None)
}

fn make_token_with_custom_token_claims(
    project_id: &str,
    record: &UserRecord,
    custom_token_claims: Option<&serde_json::Map<String, serde_json::Value>>,
) -> String {
    make_token_with_second_factor_and_claims(
        project_id,
        record,
        None,
        custom_token_claims,
        Some("custom"),
    )
}

fn make_token_with_second_factor(
    project_id: &str,
    record: &UserRecord,
    second_factor: Option<(&str, &str)>,
) -> String {
    make_token_with_second_factor_and_claims(project_id, record, second_factor, None, None)
}

fn make_token_with_second_factor_and_claims(
    project_id: &str,
    record: &UserRecord,
    second_factor: Option<(&str, &str)>,
    custom_token_claims: Option<&serde_json::Map<String, serde_json::Value>>,
    sign_in_provider: Option<&str>,
) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let issued_at = now_secs();
    let sign_in_provider = sign_in_provider.unwrap_or_else(|| {
        record
            .providers
            .first()
            .map(|provider| provider.provider_id.as_str())
            .unwrap_or("anonymous")
    });
    let mut payload = serde_json::json!({
        "aud": project_id,
        "iss": format!("https://securetoken.google.com/{project_id}"),
        "sub": record.local_id,
        "user_id": record.local_id,
        "iat": issued_at,
        "auth_time": issued_at,
        "exp": issued_at + 3600,
        "firebase": {
            "sign_in_provider": sign_in_provider,
            "identities": {}
        },
    });
    if !record.email.is_empty() {
        let object = payload.as_object_mut().expect("token payload is an object");
        object.insert("email".to_string(), serde_json::json!(record.email));
        object.insert(
            "email_verified".to_string(),
            serde_json::json!(record.email_verified),
        );
        payload["firebase"]["identities"]["email"] = serde_json::json!([record.email]);
    }
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
    for provider in &record.providers {
        if is_federated_provider(&provider.provider_id) {
            payload["firebase"]["identities"][provider.provider_id.as_str()] =
                json!([provider.raw_id]);
        }
    }
    if let Some(display_name) = &record.display_name {
        payload["name"] = json!(display_name);
    }
    if let Some(photo_url) = &record.photo_url {
        payload["picture"] = json!(photo_url);
    }
    if let Some((factor, identifier)) = second_factor {
        payload["firebase"]["sign_in_second_factor"] = json!(factor);
        payload["firebase"]["second_factor_identifier"] = json!(identifier);
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
    if let Some(claims) = custom_token_claims {
        let object = payload.as_object_mut().expect("token payload is an object");
        for (key, value) in claims {
            object.insert(key.clone(), value.clone());
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

fn parse_token_claims(token: &str, fallback_project_id: &str) -> Result<TokenClaims, AuthError> {
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
        .unwrap_or_else(|| fallback_project_id.to_string());
    let issued_at = value.get("iat").and_then(|iat| iat.as_u64()).unwrap_or(0);
    Ok(TokenClaims {
        project_id,
        local_id,
        issued_at,
    })
}

fn parse_client_token_claims(auth: &AuthState, token: &str) -> Result<TokenClaims, AuthError> {
    let claims = parse_token_claims(token, auth.default_project_id())?;
    if claims.project_id != auth.default_project_id() {
        return Err(error(StatusCode::BAD_REQUEST, "INVALID_ID_TOKEN"));
    }
    Ok(claims)
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
        ..Default::default()
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
                enrollment.enrolled_at = Some(now_rfc3339());
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

fn now_rfc3339() -> String {
    format_timestamp_ms(now_ms())
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

fn create_verification_code(
    project: &mut ProjectAuthState,
    phone_number: String,
    purpose: VerificationPurpose,
) -> String {
    let session_info = format!("firelite-sms-session-{}", Uuid::new_v4());
    let mut hasher = Sha256::new();
    hasher.update(session_info.as_bytes());
    let digest = hasher.finalize();
    let number = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    project.next_verification_code_sequence += 1;
    let code = format!("{number:06}");
    log_sms_code(&phone_number, &code);
    project.verification_codes.insert(
        session_info.clone(),
        VerificationCodeRecord {
            phone_number,
            code,
            sequence: project.next_verification_code_sequence,
            purpose,
        },
    );
    session_info
}

fn error(status: StatusCode, message: &'static str) -> AuthError {
    AuthError { status, message }
}

fn project_id_for_mfa_pending(auth: &AuthState, credential: &str) -> Option<String> {
    let projects = auth.projects.read().expect("auth state poisoned");
    projects.iter().find_map(|(project_id, project)| {
        project
            .mfa_pending_credentials
            .contains_key(credential)
            .then(|| project_id.clone())
    })
}

impl From<&UserRecord> for EmulatorUser {
    fn from(record: &UserRecord) -> Self {
        Self {
            local_id: record.local_id.clone(),
            email: (!record.email.is_empty()).then(|| record.email.clone()),
            display_name: record.display_name.clone(),
            photo_url: record.photo_url.clone(),
            phone_number: record.phone_number.clone(),
            custom_attributes: record.custom_attributes.clone(),
            disabled: record.disabled,
            email_verified: record.email_verified,
            password_hash: record.password_hash.clone(),
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
                    federated_id: is_federated_provider(&provider.provider_id)
                        .then(|| provider.raw_id.clone()),
                    display_name: provider.display_name.clone(),
                    photo_url: provider.photo_url.clone(),
                })
                .collect(),
            mfa_info: record.mfa_info.clone(),
        }
    }
}
