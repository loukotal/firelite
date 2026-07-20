use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use firelite::server;
use reqwest::{header, StatusCode};
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[tokio::test]
async fn auth_create_sign_in_list_delete_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": "alice@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let local_id = created["localId"].as_str().unwrap();
    assert_eq!(created["email"], "alice@example.test");
    assert!(created["idToken"].as_str().unwrap().contains('.'));

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "alice@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(signed_in["localId"], local_id);

    let listed: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/accounts"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["users"].as_array().unwrap().len(), 1);

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:delete?key=fake"
        ))
        .json(&json!({ "localId": local_id }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed_after_delete: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/accounts"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed_after_delete["users"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn auth_anonymous_signup_lookup_refresh_and_delete_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({ "returnSecureToken": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(created.get("email").is_none());
    let claims = decode_jwt_payload(created["idToken"].as_str().unwrap());
    assert_eq!(claims["firebase"]["sign_in_provider"], "anonymous");
    assert_eq!(claims["firebase"]["identities"], json!({}));
    assert!(claims.get("email").is_none());

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": created["idToken"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(lookup["users"][0].get("email").is_none());
    assert!(lookup["users"][0].get("passwordHash").is_none());
    assert_eq!(lookup["users"][0]["providerUserInfo"], json!([]));

    let refreshed: Value = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", created["refreshToken"].as_str().unwrap()),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let refreshed_claims = decode_jwt_payload(refreshed["id_token"].as_str().unwrap());
    assert_eq!(
        refreshed_claims["firebase"]["sign_in_provider"],
        "anonymous"
    );

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:delete?key=fake"
        ))
        .json(&json!({ "idToken": created["idToken"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

#[tokio::test]
async fn auth_phone_mfa_enrollment_and_sign_in_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let email = "phone-mfa@example.test";
    let password = "secret123";

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": email,
            "password": password,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let enrollment_started: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaEnrollment:start?key=fake"
        ))
        .json(&json!({
            "idToken": created["idToken"],
            "phoneEnrollmentInfo": {
                "phoneNumber": "+15555550123",
                "clientType": "CLIENT_TYPE_WEB"
            }
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let enrollment_session = enrollment_started["phoneSessionInfo"]["sessionInfo"]
        .as_str()
        .unwrap();
    let enrollment_codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        enrollment_codes["verificationCodes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        enrollment_codes["verificationCodes"][0]["sessionInfo"],
        enrollment_session
    );
    assert_eq!(
        enrollment_codes["verificationCodes"][0]["phoneNumber"],
        "+15555550123"
    );
    let enrollment_code = enrollment_codes["verificationCodes"][0]["code"]
        .as_str()
        .unwrap();

    let enrolled: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaEnrollment:finalize?key=fake"
        ))
        .json(&json!({
            "idToken": created["idToken"],
            "displayName": "Personal phone",
            "phoneVerificationInfo": {
                "sessionInfo": enrollment_session,
                "code": enrollment_code
            }
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(enrolled["idToken"].as_str().unwrap().contains('.'));
    assert!(enrolled["refreshToken"]
        .as_str()
        .unwrap()
        .starts_with("firelite-refresh."));
    let enrollment_id = enrolled["mfaInfo"][0]["mfaEnrollmentId"].as_str().unwrap();

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": enrolled["idToken"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        lookup["users"][0]["mfaInfo"][0]["mfaEnrollmentId"],
        enrollment_id
    );
    assert_eq!(
        lookup["users"][0]["mfaInfo"][0]["phoneInfo"],
        "+15555550123"
    );

    let first_factor: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": email,
            "password": password,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(first_factor.get("idToken").is_none());
    assert!(first_factor.get("refreshToken").is_none());
    assert_eq!(first_factor["mfaInfo"][0]["mfaEnrollmentId"], enrollment_id);
    let pending = first_factor["mfaPendingCredential"].as_str().unwrap();

    let sign_in_started: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:start?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": pending,
            "mfaEnrollmentId": enrollment_id,
            "phoneSignInInfo": {}
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let sign_in_session = sign_in_started["phoneResponseInfo"]["sessionInfo"]
        .as_str()
        .unwrap();
    let sign_in_codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        sign_in_codes["verificationCodes"].as_array().unwrap().len(),
        1
    );
    let sign_in_code = sign_in_codes["verificationCodes"][0]["code"]
        .as_str()
        .unwrap();

    let wrong_code = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:finalize?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": pending,
            "phoneVerificationInfo": {
                "sessionInfo": sign_in_session,
                "code": "000000"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_code.status(), StatusCode::BAD_REQUEST);

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:finalize?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": pending,
            "phoneVerificationInfo": {
                "sessionInfo": sign_in_session,
                "code": sign_in_code
            }
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let claims = decode_jwt_payload(signed_in["idToken"].as_str().unwrap());
    assert_eq!(claims["firebase"]["sign_in_provider"], "password");
    assert_eq!(claims["firebase"]["sign_in_second_factor"], "phone");
    assert_eq!(
        claims["firebase"]["second_factor_identifier"],
        enrollment_id
    );

    let refreshed: Value = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", signed_in["refreshToken"].as_str().unwrap()),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let refreshed_claims = decode_jwt_payload(refreshed["id_token"].as_str().unwrap());
    assert_eq!(
        refreshed_claims["firebase"]["sign_in_second_factor"],
        "phone"
    );

    let replay = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:start?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": pending,
            "mfaEnrollmentId": enrollment_id,
            "phoneSignInInfo": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    let blocked_first_factor: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": email,
            "password": password,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let blocked_pending = blocked_first_factor["mfaPendingCredential"]
        .as_str()
        .unwrap();
    let blocked_start: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:start?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": blocked_pending,
            "mfaEnrollmentId": enrollment_id,
            "phoneSignInInfo": {}
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let blocked_session = blocked_start["phoneResponseInfo"]["sessionInfo"]
        .as_str()
        .unwrap();
    let blocked_codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let blocked_code = blocked_codes["verificationCodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionInfo"] == blocked_session)
        .unwrap()["code"]
        .as_str()
        .unwrap();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"],
            "disableUser": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let blocked_finalize: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:finalize?key=fake"
        ))
        .json(&json!({
            "mfaPendingCredential": blocked_pending,
            "phoneVerificationInfo": {
                "sessionInfo": blocked_session,
                "code": blocked_code
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(blocked_finalize["error"]["message"], "USER_DISABLED");

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:delete?key=fake"
        ))
        .json(&json!({ "localId": created["localId"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let codes_after_delete: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        codes_after_delete["verificationCodes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn auth_secure_token_refresh_supports_browser_sdk_cors_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": "refresh@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let preflight = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base_url}/securetoken.googleapis.com/v1/token?key=fake"),
        )
        .header(header::ORIGIN, "http://localhost:3010")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "content-type,x-client-version,x-firebase-client,x-firebase-gmpid",
        )
        .send()
        .await
        .unwrap();
    assert!(preflight.status().is_success());
    assert_eq!(
        preflight
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "*"
    );

    let refreshed: Value = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                created["refreshToken"].as_str().expect("refresh token"),
            ),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(refreshed["user_id"], created["localId"]);
    assert_eq!(refreshed["project_id"], "demo-firelite");
    assert_eq!(refreshed["expires_in"], "3600");
    assert!(refreshed["id_token"].as_str().unwrap().contains('.'));
    assert!(refreshed["access_token"].as_str().unwrap().contains('.'));
    assert!(refreshed["refresh_token"]
        .as_str()
        .unwrap()
        .starts_with("firelite-refresh."));
}

#[tokio::test]
async fn auth_admin_update_custom_claims_are_in_new_id_tokens() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "claims@example.test",
            "password": "secret123",
            "emailVerified": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"].as_str().unwrap(),
            "customAttributes": "{\"admin\":true,\"superadmin\":true,\"partner\":\"admin\"}"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "claims@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let token_payload = decode_jwt_payload(signed_in["idToken"].as_str().unwrap());
    assert_eq!(token_payload["email"], "claims@example.test");
    assert_eq!(token_payload["email_verified"], true);
    assert!(token_payload["auth_time"].as_u64().is_some());
    assert_eq!(token_payload["firebase"]["sign_in_provider"], "password");
    assert_eq!(
        token_payload["firebase"]["identities"]["email"][0],
        "claims@example.test"
    );
    assert_eq!(token_payload["admin"], true);
    assert_eq!(token_payload["superadmin"], true);
    assert_eq!(token_payload["partner"], "admin");
}

#[tokio::test]
async fn auth_secure_token_refresh_sees_updated_custom_claims() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "refresh-claims@example.test",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "refresh-claims@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"].as_str().unwrap(),
            "customAttributes": "{\"admin\":true}"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let refreshed: Value = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                signed_in["refreshToken"].as_str().expect("refresh token"),
            ),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let token_payload = decode_jwt_payload(refreshed["id_token"].as_str().unwrap());
    assert_eq!(token_payload["admin"], true);
}

#[tokio::test]
async fn auth_admin_update_persists_phone_disabled_and_mfa() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "mfa@example.test",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let updated: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"].as_str().unwrap(),
            "phoneNumber": "+15555550100",
            "disableUser": true,
            "mfa": {
                "enrollments": [{
                    "mfaEnrollmentId": "factor-1",
                    "displayName": "phone",
                    "phoneInfo": "+15555550101",
                    "enrolledAt": "2026-01-01T00:00:00.000Z"
                }]
            }
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(updated["phoneNumber"], "+15555550100");
    assert_eq!(updated["disabled"], true);
    assert_eq!(updated["mfaInfo"][0]["mfaEnrollmentId"], "factor-1");
    assert_eq!(updated["mfaInfo"][0]["phoneInfo"], "+15555550101");

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:lookup?key=fake"
        ))
        .json(&json!({
            "phoneNumber": ["+15555550100"]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lookup["users"][0]["localId"], created["localId"]);
    assert_eq!(
        lookup["users"][0]["mfaInfo"][0]["mfaEnrollmentId"],
        "factor-1"
    );
}

#[tokio::test]
async fn auth_duplicate_phone_returns_phone_exists_before_invalid_phone() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "phone-one@example.test",
            "password": "secret123",
            "phoneNumber": "+15555550111"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let duplicate = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "phone-two@example.test",
            "password": "secret123",
            "phoneNumber": "+15555550111"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    let body: Value = duplicate.json().await.unwrap();
    assert_eq!(body["error"]["message"], "PHONE_NUMBER_EXISTS");
}

#[tokio::test]
async fn auth_revoke_refresh_tokens_invalidates_old_id_and_refresh_tokens() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "revoked@example.test",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "revoked@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"].as_str().unwrap(),
            "validSince": 4_102_444_800_u64
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let lookup = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({
            "idToken": signed_in["idToken"].as_str().unwrap()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(lookup.status(), StatusCode::BAD_REQUEST);
    let lookup_body: Value = lookup.json().await.unwrap();
    assert_eq!(lookup_body["error"]["message"], "TOKEN_EXPIRED");

    let refresh = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                signed_in["refreshToken"].as_str().expect("refresh token"),
            ),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::BAD_REQUEST);
    let refresh_body: Value = refresh.json().await.unwrap();
    assert_eq!(refresh_body["error"]["message"], "TOKEN_EXPIRED");
}

#[tokio::test]
async fn auth_password_sign_in_uses_configured_project() {
    let base_url = spawn_app_for_project("bf-demo-a24dc").await;
    let client = reqwest::Client::new();

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/bf-demo-a24dc/accounts?key=fake"
        ))
        .json(&json!({
            "email": "admin-created@example.test",
            "password": "password",
            "emailVerified": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "admin-created@example.test",
            "password": "password",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(signed_in["email"], "admin-created@example.test");
    let token_payload = decode_jwt_payload(signed_in["idToken"].as_str().unwrap());
    assert_eq!(token_payload["aud"], "bf-demo-a24dc");

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({
            "idToken": signed_in["idToken"].as_str().unwrap()
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(lookup["users"][0]["email"], "admin-created@example.test");
}

#[tokio::test]
async fn auth_recaptcha_discovery_and_mfa_codes_use_configured_project() {
    let base_url = spawn_app_for_project("bf-demo-a24dc").await;
    let client = reqwest::Client::new();

    let enterprise = client
        .get(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/recaptchaConfig?key=fake&clientType=CLIENT_TYPE_WEB&version=RECAPTCHA_ENTERPRISE"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(enterprise.status(), StatusCode::NOT_IMPLEMENTED);
    let enterprise_body: Value = enterprise.json().await.unwrap();
    assert_eq!(enterprise_body["error"]["code"], 501);
    assert_eq!(enterprise_body["error"]["status"], "NOT_IMPLEMENTED");
    assert_eq!(
        enterprise_body["error"]["errors"][0]["reason"],
        "unimplemented"
    );

    let recaptcha: Value = client
        .get(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/recaptchaParams?key=fake"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        recaptcha["kind"],
        "identitytoolkit#GetRecaptchaParamResponse"
    );
    assert_eq!(
        recaptcha["recaptchaSiteKey"],
        "Fake-key__Do-not-send-this-to-Recaptcha_"
    );

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": "configured-mfa@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = decode_jwt_payload(created["idToken"].as_str().unwrap());
    assert_eq!(token["aud"], "bf-demo-a24dc");

    let started: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/accounts/mfaEnrollment:start?key=fake"
        ))
        .json(&json!({
            "idToken": created["idToken"],
            "phoneEnrollmentInfo": {
                "phoneNumber": "+15555550123",
                "recaptchaToken": "emulator-token",
                "clientType": "CLIENT_TYPE_WEB"
            }
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let session = started["phoneSessionInfo"]["sessionInfo"].as_str().unwrap();

    let configured_codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/bf-demo-a24dc/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        configured_codes["verificationCodes"][0]["sessionInfo"],
        session
    );
    assert_eq!(
        configured_codes["verificationCodes"][0]["phoneNumber"],
        "+15555550123"
    );

    let default_codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(default_codes["verificationCodes"], json!([]));
}

#[tokio::test]
async fn auth_duplicate_email_matches_emulator_error_shape() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let url = format!("{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake");
    let payload = json!({
        "email": "dupe@example.test",
        "password": "secret123",
        "returnSecureToken": true
    });

    client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let response = client.post(url).json(&payload).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["message"], "EMAIL_EXISTS");
}

#[tokio::test]
async fn auth_custom_token_sign_in_creates_local_user() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let token = unsigned_jwt(json!({ "uid": "agent-user-1" }));

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithCustomToken?key=fake"
        ))
        .json(&json!({
            "token": token,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(signed_in["localId"], "agent-user-1");
    assert_eq!(signed_in["email"], "agent-user-1@custom-token.local");
    assert!(signed_in["idToken"].as_str().unwrap().contains('.'));
}

#[tokio::test]
async fn auth_idp_sign_in_reuses_provider_identity() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let url =
        format!("{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key=fake");
    let payload = json!({
        "requestUri": "http://localhost",
        "postBody": "providerId=google.com&rawId=google-123&email=Alice%40Example.TEST",
        "returnSecureToken": true
    });

    let first: Value = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let second: Value = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(first["localId"], second["localId"]);
    assert_eq!(first["email"], "alice@example.test");
}

#[tokio::test]
async fn auth_admin_create_normalizes_email_case() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "Mixed.Case@Example.TEST",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(created["email"], "mixed.case@example.test");

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:lookup?key=fake"
        ))
        .json(&json!({
            "email": ["mixed.case@example.test"]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lookup["users"][0]["email"], "mixed.case@example.test");
}

#[tokio::test]
async fn auth_email_link_oob_flow_signs_in_user_once() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let sent: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:sendOobCode?key=fake"
        ))
        .json(&json!({
            "requestType": "EMAIL_SIGNIN",
            "email": "link@example.test",
            "continueUrl": "http://localhost/finish"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let oob_code = sent["oobCode"].as_str().unwrap();

    let listed: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/oobCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["oobCodes"].as_array().unwrap().len(), 1);

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithEmailLink?key=fake"
        ))
        .json(&json!({
            "email": "link@example.test",
            "oobCode": oob_code,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(signed_in["email"], "link@example.test");

    let replay = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithEmailLink?key=fake"
        ))
        .json(&json!({
            "email": "link@example.test",
            "oobCode": oob_code,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn auth_admin_password_reset_link_creates_oob_link() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let email = "reset@example.test";

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "localId": "reset-user",
            "email": email,
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let sent: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:sendOobCode?key=fake"
        ))
        .json(&json!({
            "requestType": "PASSWORD_RESET",
            "email": email,
            "returnOobLink": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let oob_code = sent["oobCode"].as_str().unwrap();
    let oob_link = sent["oobLink"].as_str().unwrap();
    assert_eq!(sent["email"], email);
    assert!(oob_link.starts_with(&base_url));
    assert!(oob_link.contains("mode=resetPassword"));
    assert!(oob_link.contains(&format!("oobCode={oob_code}")));

    let action: Value = client
        .get(oob_link)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(action["oobCode"], oob_code);
    assert_eq!(action["email"], email);

    let listed: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-firelite/oobCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed_code = &listed["oobCodes"].as_array().unwrap()[0];
    assert_eq!(listed_code["email"], email);
    assert_eq!(listed_code["oobCode"], oob_code);
    assert_eq!(listed_code["requestType"], "PASSWORD_RESET");

    let missing = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:sendOobCode?key=fake"
        ))
        .json(&json!({
            "requestType": "PASSWORD_RESET",
            "email": "missing@example.test",
            "returnOobLink": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

async fn spawn_app() -> String {
    spawn_app_for_project("demo-firelite").await
}

async fn spawn_app_for_project(project_id: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = server::app_for_project(project_id.to_string());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn unsigned_jwt(payload: Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("{header}.{payload}.")
}

fn decode_jwt_payload(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    let decoded = URL_SAFE_NO_PAD.decode(payload).unwrap();
    serde_json::from_slice(&decoded).unwrap()
}
