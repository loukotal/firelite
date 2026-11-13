use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use firelite::server;
use reqwest::{header, StatusCode};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use uuid::Uuid;

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
    let enrollment_retried: Value = client
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
    let enrollment_retry_session = enrollment_retried["phoneSessionInfo"]["sessionInfo"]
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
        2
    );
    assert_eq!(
        enrollment_codes["verificationCodes"][0]["sessionInfo"],
        enrollment_session
    );
    assert_eq!(
        enrollment_codes["verificationCodes"][0]["phoneNumber"],
        "+15555550123"
    );
    assert_eq!(
        enrollment_codes["verificationCodes"][1]["sessionInfo"],
        enrollment_retry_session
    );
    let enrollment_code = enrollment_codes["verificationCodes"][1]["code"]
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
                "sessionInfo": enrollment_retry_session,
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
        2
    );
    assert_eq!(
        sign_in_codes["verificationCodes"][1]["sessionInfo"],
        sign_in_session
    );
    let sign_in_code = sign_in_codes["verificationCodes"][1]["code"]
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
async fn auth_admin_update_changes_password() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "email": "updated-password@example.test",
            "password": "old-password"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let signed_in_before_update: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "updated-password@example.test",
            "password": "old-password",
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
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": created["localId"].as_str().unwrap(),
            "password": "new-password"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let old_token = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": signed_in_before_update["idToken"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(old_token.status(), reqwest::StatusCode::BAD_REQUEST);

    let old_refresh_token = client
        .post(format!(
            "{base_url}/securetoken.googleapis.com/v1/token?key=fake"
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                signed_in_before_update["refreshToken"]
                    .as_str()
                    .expect("refresh token"),
            ),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(old_refresh_token.status(), reqwest::StatusCode::BAD_REQUEST);

    let old_password = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "updated-password@example.test",
            "password": "old-password",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(old_password.status(), reqwest::StatusCode::BAD_REQUEST);

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "updated-password@example.test",
            "password": "new-password",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
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

    let enterprise: Value = client
        .get(format!(
            "{base_url}/identitytoolkit.googleapis.com/v2/recaptchaConfig?key=fake&clientType=CLIENT_TYPE_WEB&version=RECAPTCHA_ENTERPRISE"
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
        enterprise["kind"],
        "identitytoolkit#GetRecaptchaConfigResponse"
    );
    assert_eq!(
        enterprise["recaptchaKey"],
        "Fake-key__Do-not-send-this-to-Recaptcha_"
    );
    assert_eq!(
        enterprise["recaptchaEnforcementState"],
        json!([
            { "provider": "PHONE_PROVIDER", "enforcementState": "OFF" },
            { "provider": "EMAIL_PASSWORD_PROVIDER", "enforcementState": "OFF" }
        ])
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
async fn auth_first_factor_phone_code_signs_in_and_is_single_use() {
    let base_url = spawn_app_for_project("demo-phone").await;
    let client = reqwest::Client::new();

    let sent: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:sendVerificationCode?key=fake"
        ))
        .json(&json!({
            "phoneNumber": "+15555550124",
            "recaptchaToken": "emulator-token",
            "clientType": "CLIENT_TYPE_WEB"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_info = sent["sessionInfo"].as_str().unwrap();

    let codes: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-phone/verificationCodes"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let verification = codes["verificationCodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|code| code["sessionInfo"] == session_info)
        .unwrap();
    let code = verification["code"].as_str().unwrap();

    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPhoneNumber?key=fake"
        ))
        .json(&json!({
            "sessionInfo": session_info,
            "code": code,
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
    assert_eq!(signed_in["phoneNumber"], "+15555550124");
    assert_eq!(signed_in["isNewUser"], true);
    assert!(signed_in["idToken"].as_str().unwrap().contains('.'));

    let reused = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPhoneNumber?key=fake"
        ))
        .json(&json!({
            "sessionInfo": session_info,
            "code": code,
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reused.status(), StatusCode::BAD_REQUEST);
    let reused_body: Value = reused.json().await.unwrap();
    assert_eq!(reused_body["error"]["message"], "INVALID_SESSION_INFO");
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
async fn auth_phone_credential_link_preserves_the_authenticated_user() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": "phone-link@example.test",
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

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({
            "localId": local_id,
            "customAttributes": "{\"role\":\"member\"}"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let verification: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:sendVerificationCode?key=fake"
        ))
        .json(&json!({ "phoneNumber": "+15555550123" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_info = verification["sessionInfo"].as_str().unwrap();
    let codes: Value = client
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
    let code = codes["verificationCodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionInfo"] == session_info)
        .unwrap()["code"]
        .as_str()
        .unwrap();

    let updated: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPhoneNumber?key=fake"
        ))
        .json(&json!({
            "idToken": created["idToken"],
            "sessionInfo": session_info,
            "code": code,
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
    assert_eq!(updated["localId"], local_id);
    assert_eq!(updated["email"], "phone-link@example.test");
    assert_eq!(updated["phoneNumber"], "+15555550123");
    assert_eq!(
        decode_jwt_payload(updated["idToken"].as_str().unwrap())["role"],
        "member"
    );

    let accounts: Value = client
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
    assert_eq!(accounts["users"].as_array().unwrap().len(), 1);
    let user = &accounts["users"][0];
    assert_eq!(user["localId"], local_id);
    assert_eq!(user["email"], "phone-link@example.test");
    assert_eq!(user["phoneNumber"], "+15555550123");
    assert_eq!(user["customAttributes"], "{\"role\":\"member\"}");
    assert_eq!(user["providerUserInfo"].as_array().unwrap().len(), 2);
    assert!(user["providerUserInfo"]
        .as_array()
        .unwrap()
        .iter()
        .any(|provider| provider["providerId"] == "password"));
    assert!(user["providerUserInfo"]
        .as_array()
        .unwrap()
        .iter()
        .any(|provider| provider["providerId"] == "phone"));
}

#[tokio::test]
async fn auth_custom_token_claims_and_provider_are_in_the_exchanged_id_token_without_persisting() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let url = format!(
        "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithCustomToken?key=fake"
    );

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts?key=fake"
        ))
        .json(&json!({
            "localId": "agent-user-with-claims",
            "email": "agent-user-with-claims@example.test",
            "password": "secret123"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let signed_in: Value = client
        .post(&url)
        .json(&json!({
            "token": unsigned_jwt(json!({
                "uid": "agent-user-with-claims",
                "claims": { "role": "embed-onboarding", "prepared": true }
            })),
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
    let claims = decode_jwt_payload(signed_in["idToken"].as_str().unwrap());
    assert_eq!(claims["role"], "embed-onboarding");
    assert_eq!(claims["prepared"], true);
    assert_eq!(claims["firebase"]["sign_in_provider"], "custom");

    let signed_in_without_claims: Value = client
        .post(url)
        .json(&json!({
            "token": unsigned_jwt(json!({ "uid": "agent-user-with-claims" })),
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
    let claims = decode_jwt_payload(signed_in_without_claims["idToken"].as_str().unwrap());
    assert!(claims.get("role").is_none());
    assert!(claims.get("prepared").is_none());
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
async fn auth_google_credential_sign_in_populates_profile_and_reuses_identity() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let claims = json!({
        "sub": "112233445566",
        "email": "Grace@Example.TEST",
        "email_verified": true,
        "name": "Grace Hopper",
        "given_name": "Grace",
        "family_name": "Hopper",
        "picture": "https://example.test/grace.png"
    });

    let first = sign_in_with_idp(
        &client,
        &base_url,
        json!({
            "requestUri": "http://localhost",
            "postBody": format!(
                "id_token={}&providerId=google.com",
                url_encode(&claims.to_string())
            ),
            "returnSecureToken": true,
            "returnIdpCredential": true
        }),
    )
    .await;
    assert_eq!(first["providerId"], "google.com");
    assert_eq!(
        first["federatedId"],
        "https://accounts.google.com/112233445566"
    );
    assert_eq!(first["email"], "grace@example.test");
    assert_eq!(first["emailVerified"], true);
    assert_eq!(first["displayName"], "Grace Hopper");
    assert_eq!(first["firstName"], "Grace");
    assert_eq!(first["lastName"], "Hopper");
    assert_eq!(first["photoUrl"], "https://example.test/grace.png");
    assert_eq!(first["isNewUser"], true);
    assert_eq!(first["oauthIdToken"], claims.to_string());
    let raw_user_info: Value =
        serde_json::from_str(first["rawUserInfo"].as_str().unwrap()).unwrap();
    assert_eq!(raw_user_info["id"], "112233445566");
    assert_eq!(raw_user_info["verified_email"], true);

    let token = decode_jwt_payload(first["idToken"].as_str().unwrap());
    assert_eq!(token["firebase"]["sign_in_provider"], "google.com");
    assert_eq!(
        token["firebase"]["identities"]["google.com"],
        json!(["112233445566"])
    );
    assert_eq!(token["email"], "grace@example.test");
    assert_eq!(token["email_verified"], true);
    assert_eq!(token["name"], "Grace Hopper");
    assert_eq!(token["picture"], "https://example.test/grace.png");

    // The same Google identity as an unsigned JWT, sent the way the popup flow
    // does: in the request URI, with no post body.
    let jwt = unsigned_jwt(json!({ "sub": "112233445566", "email": "grace@example.test" }));
    let second = sign_in_with_idp(
        &client,
        &base_url,
        json!({
            "requestUri": format!(
                "{base_url}/emulator/auth/handler?providerId=google.com&id_token={jwt}"
            ),
            "postBody": null,
            "sessionId": "ValueNotUsedByAuthEmulator",
            "returnSecureToken": true,
            "returnIdpCredential": true
        }),
    )
    .await;
    assert_eq!(second["localId"], first["localId"]);
    assert_eq!(second["isNewUser"], false);
    assert_eq!(second["emailVerified"], true);

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": second["idToken"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let user = &lookup["users"][0];
    assert_eq!(user["displayName"], "Grace Hopper");
    assert_eq!(user["photoUrl"], "https://example.test/grace.png");
    assert_eq!(user["emailVerified"], true);
    assert_eq!(
        user["providerUserInfo"],
        json!([{
            "providerId": "google.com",
            "rawId": "112233445566",
            "federatedId": "112233445566",
            "email": "grace@example.test",
            "displayName": "Grace Hopper",
            "photoUrl": "https://example.test/grace.png"
        }])
    );
}

#[tokio::test]
async fn auth_google_sign_in_with_existing_email_follows_trusted_provider_rules() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let sign_up = |email: &'static str| {
        let client = client.clone();
        let base_url = base_url.clone();
        async move {
            client
                .post(format!(
                    "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
                ))
                .json(
                    &json!({ "email": email, "password": "secret123", "returnSecureToken": true }),
                )
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let google_body = |sub: &str, email: &str, verified: bool| {
        json!({
            "requestUri": "http://localhost",
            "postBody": format!(
                "id_token={}&providerId=google.com",
                url_encode(&json!({ "sub": sub, "email": email, "email_verified": verified }).to_string())
            ),
            "returnSecureToken": true
        })
    };

    // An unverified Google email never silently joins an existing account.
    let existing = sign_up("ada@example.test").await;
    let unverified = sign_in_with_idp(
        &client,
        &base_url,
        google_body("google-ada", "ada@example.test", false),
    )
    .await;
    assert_eq!(unverified["needConfirmation"], true);
    assert_eq!(unverified["localId"], existing["localId"]);
    assert!(unverified.get("idToken").is_none());

    // A verified Google email takes over the unverified password account.
    let verified = sign_in_with_idp(
        &client,
        &base_url,
        google_body("google-ada", "ada@example.test", true),
    )
    .await;
    assert_eq!(verified["localId"], existing["localId"]);
    assert_eq!(verified["isNewUser"], false);
    assert_eq!(verified["emailVerified"], true);
    let password_sign_in = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({ "email": "ada@example.test", "password": "secret123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(password_sign_in.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn auth_google_credential_links_to_signed_in_user() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let anonymous: Value = client
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
    let post_body = format!(
        "id_token={}&providerId=google.com",
        url_encode(
            &json!({ "sub": "google-linked", "email": "linked@example.test", "email_verified": true })
                .to_string()
        )
    );

    let linked = sign_in_with_idp(
        &client,
        &base_url,
        json!({
            "requestUri": "http://localhost",
            "postBody": post_body,
            "idToken": anonymous["idToken"],
            "returnSecureToken": true
        }),
    )
    .await;
    assert_eq!(linked["localId"], anonymous["localId"]);
    assert_eq!(linked["email"], "linked@example.test");
    let token = decode_jwt_payload(linked["idToken"].as_str().unwrap());
    assert_eq!(token["email"], "linked@example.test");
    assert_eq!(token["email_verified"], true);

    let other: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({ "returnSecureToken": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url =
        format!("{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key=fake");
    let conflict = client
        .post(&url)
        .json(&json!({
            "requestUri": "http://localhost",
            "postBody": post_body,
            "idToken": other["idToken"],
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::BAD_REQUEST);
    let conflict: Value = conflict.json().await.unwrap();
    assert_eq!(
        conflict["error"]["message"],
        "FEDERATED_USER_ID_ALREADY_LINKED"
    );

    let credential_error = sign_in_with_idp(
        &client,
        &base_url,
        json!({
            "requestUri": "http://localhost",
            "postBody": post_body,
            "idToken": other["idToken"],
            "returnSecureToken": true,
            "returnIdpCredential": true
        }),
    )
    .await;
    assert_eq!(
        credential_error["errorMessage"],
        "FEDERATED_USER_ID_ALREADY_LINKED"
    );
    assert_eq!(credential_error["providerId"], "google.com");
}

#[tokio::test]
async fn auth_google_sign_in_joins_verified_password_account_and_keeps_password() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let created = password_sign_up(&client, &base_url, "verified@example.test").await;
    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/projects/demo-firelite/accounts:update?key=fake"
        ))
        .json(&json!({ "localId": created["localId"], "emailVerified": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let google = sign_in_with_idp(
        &client,
        &base_url,
        google_credential_body("google-verified", "verified@example.test", None),
    )
    .await;
    assert_eq!(google["localId"], created["localId"]);
    assert_eq!(google["isNewUser"], false);

    // The pre-existing session and the password both survive.
    assert_password_sign_in(
        &client,
        &base_url,
        "verified@example.test",
        &created["localId"],
    )
    .await;
    assert_lookup_providers(
        &client,
        &base_url,
        &created["idToken"],
        &["google.com", "password"],
    )
    .await;
}

#[tokio::test]
async fn auth_google_credential_links_to_password_user_and_keeps_password() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    // Unverified on purpose: explicit linking keeps the password regardless.
    let created = password_sign_up(&client, &base_url, "linker@example.test").await;

    let linked = sign_in_with_idp(
        &client,
        &base_url,
        google_credential_body(
            "google-linker",
            "linker@example.test",
            Some(created["idToken"].clone()),
        ),
    )
    .await;
    assert_eq!(linked["localId"], created["localId"]);
    assert_eq!(linked["emailVerified"], true);

    assert_password_sign_in(
        &client,
        &base_url,
        "linker@example.test",
        &created["localId"],
    )
    .await;
    assert_lookup_providers(
        &client,
        &base_url,
        &linked["idToken"],
        &["google.com", "password"],
    )
    .await;

    // Signing in with Google later lands on the same linked account.
    let google = sign_in_with_idp(
        &client,
        &base_url,
        google_credential_body("google-linker", "linker@example.test", None),
    )
    .await;
    assert_eq!(google["localId"], created["localId"]);
    assert_eq!(google["isNewUser"], false);
}

async fn password_sign_up(client: &reqwest::Client, base_url: &str, email: &str) -> Value {
    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({ "email": email, "password": "secret123", "returnSecureToken": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn google_credential_body(sub: &str, email: &str, id_token: Option<Value>) -> Value {
    let mut body = json!({
        "requestUri": "http://localhost",
        "postBody": format!(
            "id_token={}&providerId=google.com",
            url_encode(&json!({ "sub": sub, "email": email, "email_verified": true }).to_string())
        ),
        "returnSecureToken": true
    });
    if let Some(id_token) = id_token {
        body["idToken"] = id_token;
    }
    body
}

async fn assert_password_sign_in(
    client: &reqwest::Client,
    base_url: &str,
    email: &str,
    local_id: &Value,
) {
    let signed_in: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({ "email": email, "password": "secret123", "returnSecureToken": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(&signed_in["localId"], local_id);
}

async fn assert_lookup_providers(
    client: &reqwest::Client,
    base_url: &str,
    id_token: &Value,
    expected: &[&str],
) {
    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": id_token }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut providers = lookup["users"][0]["providerUserInfo"]
        .as_array()
        .unwrap()
        .iter()
        .map(|provider| provider["providerId"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    providers.sort();
    assert_eq!(providers, expected);
}

#[tokio::test]
async fn auth_idp_sign_in_rejects_invalid_credentials() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let url =
        format!("{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key=fake");
    for (post_body, message) in [
        (
            format!(
                "id_token={}&providerId=google.com",
                url_encode(r#"{"email":"x@example.test"}"#)
            ),
            "INVALID_IDP_RESPONSE",
        ),
        (
            "id_token=not-a-token&providerId=google.com".to_string(),
            "INVALID_IDP_RESPONSE",
        ),
        (
            format!("id_token={}", url_encode(r#"{"sub":"123"}"#)),
            "INVALID_CREDENTIAL_OR_PROVIDER_ID",
        ),
    ] {
        let response = client
            .post(&url)
            .json(&json!({ "requestUri": "http://localhost", "postBody": post_body }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{post_body}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["message"], message, "{post_body}");
    }
}

#[tokio::test]
async fn auth_idp_widget_lists_provider_accounts_and_serves_helper_iframe() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    sign_in_with_idp(
        &client,
        &base_url,
        json!({
            "requestUri": "http://localhost",
            "postBody": format!(
                "id_token={}&providerId=google.com",
                url_encode(
                    &json!({ "sub": "google-widget", "email": "widget@example.test", "name": "<Widget>" })
                        .to_string()
                )
            )
        }),
    )
    .await;

    let missing = client
        .get(format!("{base_url}/emulator/auth/handler"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

    let widget = client
        .get(format!(
            "{base_url}/emulator/auth/handler?apiKey=fake&appName=%5BDEFAULT%5D&authType=signInViaPopup&providerId=google.com&eventId=1"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(widget.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let html = widget.text().await.unwrap();
    assert!(html.contains("widget@example.test"));
    assert!(html.contains("&lt;Widget&gt;"));
    assert!(!html.contains("<Widget>"));
    assert!(html.contains("google-widget"));

    let other_provider = client
        .get(format!(
            "{base_url}/emulator/auth/handler?apiKey=fake&appName=%5BDEFAULT%5D&authType=signInViaPopup&providerId=github.com"
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!other_provider.contains("widget@example.test"));

    let iframe = client
        .get(format!(
            "{base_url}/emulator/auth/iframe?apiKey=fake&appName=%5BDEFAULT%5D"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(iframe.contains("gapi.iframes"));
}

async fn sign_in_with_idp(client: &reqwest::Client, base_url: &str, payload: Value) -> Value {
    let response = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key=fake"
        ))
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert!(status.is_success(), "signInWithIdp failed: {status} {body}");
    body
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
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

    let lookup: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:lookup?key=fake"
        ))
        .json(&json!({ "idToken": signed_in["idToken"] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lookup["users"][0]["emailVerified"], true);

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
async fn auth_verify_email_oob_flow_marks_user_verified_once() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let email = "verify@example.test";

    let created: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": email,
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

    let sent: Value = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:sendOobCode?key=fake"
        ))
        .json(&json!({
            "requestType": "VERIFY_EMAIL",
            "idToken": created["idToken"],
            "continueUrl": "http://localhost/verified",
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
    assert_eq!(sent["email"], email);
    assert!(sent["oobLink"]
        .as_str()
        .unwrap()
        .contains("mode=verifyEmail"));

    let applied: Value = client
        .get(sent["oobLink"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(applied["emailVerified"], true);

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
    assert_eq!(lookup["users"][0]["emailVerified"], true);

    let replay = client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:update?key=fake"
        ))
        .json(&json!({ "oobCode": sent["oobCode"] }))
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

#[tokio::test]
async fn auth_users_persist_across_app_restarts_and_can_be_reset() {
    let database = temporary_database_path();
    let client = reqwest::Client::new();
    let state = server::app_state_with_functions_for_project_persistent(
        "demo-persist",
        None,
        database.clone(),
    )
    .unwrap();
    let base_url = spawn_app_with_state(state).await;

    client
        .post(format!(
            "{base_url}/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake"
        ))
        .json(&json!({
            "email": "persisted@example.test",
            "password": "secret123",
            "returnSecureToken": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let restarted_state = server::app_state_with_functions_for_project_persistent(
        "demo-persist",
        None,
        database.clone(),
    )
    .unwrap();
    let restarted_url = spawn_app_with_state(restarted_state).await;
    let signed_in: Value = client
        .post(format!(
            "{restarted_url}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake"
        ))
        .json(&json!({
            "email": "persisted@example.test",
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
    assert_eq!(signed_in["email"], "persisted@example.test");

    firelite::auth::AuthState::reset_persisted_project(&database, "demo-persist").unwrap();
    let reset_state = server::app_state_with_functions_for_project_persistent(
        "demo-persist",
        None,
        database.clone(),
    )
    .unwrap();
    let reset_url = spawn_app_with_state(reset_state).await;
    let listed: Value = client
        .get(format!(
            "{reset_url}/emulator/v1/projects/demo-persist/accounts"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed["users"].as_array().unwrap().is_empty());

    std::fs::remove_file(database).unwrap();
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

async fn spawn_app_with_state(state: Arc<server::AppState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app_with_state(state))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

fn temporary_database_path() -> PathBuf {
    std::env::temp_dir().join(format!("firelite-auth-{}.sqlite", Uuid::new_v4()))
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
