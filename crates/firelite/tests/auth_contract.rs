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
async fn auth_password_sign_in_finds_admin_created_user_project() {
    let base_url = spawn_app().await;
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
        "postBody": "providerId=google.com&rawId=google-123&email=alice%40example.test",
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

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
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
