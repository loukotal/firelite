use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use firelite::server;
use serde_json::Value;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::test]
async fn serves_management_console() {
    let base_url = spawn_app().await;
    let response = reqwest::get(format!("{base_url}/__/ui")).await.unwrap();

    assert!(response.status().is_success());
    let body = response.text().await.unwrap();
    assert!(body.contains("Firelite Console"));
    assert!(body.contains("Create User"));
    assert!(body.contains("Create Object"));
}

#[tokio::test]
async fn storage_management_create_list_delete_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let objects_url = format!("{base_url}/v0/b/demo-firelite.appspot.com/o");

    let created: Value = client
        .post(format!("{objects_url}?name=uploads%2Fsample.txt"))
        .header("content-type", "text/plain")
        .body("hello")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["name"], "uploads/sample.txt");
    assert_eq!(created["size"], "5");

    let listed: Value = client
        .get(&objects_url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);

    client
        .delete(format!("{objects_url}/uploads%2Fsample.txt"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed_after_delete: Value = client
        .get(&objects_url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed_after_delete["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn attaches_multiple_functions_workers() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    for (port, filters) in [(5001, vec!["api"]), (5002, vec!["e2e"])] {
        client
            .post(format!("{base_url}/__/control/attachments"))
            .json(&serde_json::json!({
                "projectId": "demo-firelite",
                "workdir": format!("/tmp/checkout-{port}"),
                "functionsHost": "127.0.0.1",
                "functionsPort": port,
                "filters": filters
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    let listed: Value = client
        .get(format!("{base_url}/__/control/attachments"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let attachments = listed["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 2);
    assert!(attachments
        .iter()
        .any(|attachment| attachment["id"] == "demo-firelite@127.0.0.1:5001"));
    assert!(attachments
        .iter()
        .any(|attachment| attachment["id"] == "demo-firelite@127.0.0.1:5002"));
}

#[tokio::test]
async fn proxies_function_routes_to_attached_worker() {
    let base_url = spawn_app().await;
    let worker_url = spawn_mock_functions_worker().await;
    let worker_port = worker_url
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base_url}/__/control/attachments"))
        .json(&serde_json::json!({
            "projectId": "demo-firelite",
            "workdir": "/tmp/checkout",
            "functionsHost": "127.0.0.1",
            "functionsPort": worker_port,
            "filters": ["api"]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let proxied: Value = client
        .post(format!(
            "{base_url}/demo-firelite/us-central1/api/users/1?debug=true"
        ))
        .header("x-firelite-test", "attached-worker")
        .body("hello")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        proxied["path"],
        "/demo-firelite/us-central1/api/users/1?debug=true"
    );
    assert_eq!(proxied["method"], "POST");
    assert_eq!(proxied["header"], "attached-worker");
    assert_eq!(proxied["body"], "hello");
}

#[tokio::test]
async fn cloud_tasks_create_dispatches_to_attached_function_queue() {
    let state = server::app_state();
    let base_url = spawn_app_with_state(state).await;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let worker_url = spawn_capturing_functions_worker(captured.clone()).await;
    let worker_port = worker_url
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base_url}/__/control/attachments"))
        .json(&serde_json::json!({
            "projectId": "demo-firelite",
            "workdir": "/tmp/checkout",
            "functionsHost": "127.0.0.1",
            "functionsPort": worker_port,
            "filters": ["jobs.run"]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let body = BASE64.encode(r#"{"data":{"jobTaskId":"task-1"}}"#);
    let created: Value = client
        .post(format!(
            "{base_url}/projects/demo-firelite/locations/us-central1/queues/jobs.run/tasks"
        ))
        .json(&serde_json::json!({
            "task": {
                "httpRequest": {
                    "headers": {
                        "Content-Type": "application/json",
                        "X-Firelite-Test": "task-dispatch"
                    },
                    "body": body
                }
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

    assert!(created["name"]
        .as_str()
        .unwrap()
        .starts_with("projects/demo-firelite/locations/us-central1/queues/jobs.run/tasks/"));

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0]["method"], "POST");
    assert_eq!(captured[0]["path"], "/demo-firelite/us-central1/jobs.run");
    assert_eq!(captured[0]["header"], "task-dispatch");
    assert_eq!(captured[0]["body"], r#"{"data":{"jobTaskId":"task-1"}}"#);
    assert_eq!(
        captured[0]["queue"],
        "projects/demo-firelite/locations/us-central1/queues/jobs.run"
    );
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
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

async fn spawn_mock_functions_worker() -> String {
    use axum::{
        body::Bytes,
        extract::OriginalUri,
        http::{HeaderMap, Method},
        response::Json,
        routing::any,
        Router,
    };

    async fn handler(
        OriginalUri(uri): OriginalUri,
        method: Method,
        headers: HeaderMap,
        body: Bytes,
    ) -> Json<Value> {
        Json(serde_json::json!({
            "method": method.as_str(),
            "path": uri.to_string(),
            "header": headers
                .get("x-firelite-test")
                .and_then(|value| value.to_str().ok()),
            "body": String::from_utf8_lossy(&body),
        }))
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/*path", any(handler)))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_capturing_functions_worker(captured: Arc<Mutex<Vec<Value>>>) -> String {
    use axum::{
        body::Bytes,
        extract::{OriginalUri, State},
        http::{HeaderMap, Method},
        routing::any,
        Router,
    };

    async fn handler(
        State(captured): State<Arc<Mutex<Vec<Value>>>>,
        OriginalUri(uri): OriginalUri,
        method: Method,
        headers: HeaderMap,
        body: Bytes,
    ) -> &'static str {
        captured.lock().await.push(serde_json::json!({
            "method": method.as_str(),
            "path": uri.to_string(),
            "header": headers
                .get("x-firelite-test")
                .and_then(|value| value.to_str().ok()),
            "queue": headers
                .get("x-cloudtasks-queuename")
                .and_then(|value| value.to_str().ok()),
            "body": String::from_utf8_lossy(&body),
        }));
        "ok"
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/*path", any(handler))
                .with_state(captured),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}
