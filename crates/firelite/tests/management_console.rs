use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use firelite::{server, tasks::FunctionsTarget};
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
async fn cloud_tasks_create_dispatches_to_configured_function_queue() {
    let state = server::app_state();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let worker_url = spawn_capturing_functions_worker(captured.clone()).await;
    let worker_port = worker_url
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    state.tasks.set_functions_target(FunctionsTarget {
        project_id: "demo-firelite".to_string(),
        functions_host: "127.0.0.1".to_string(),
        functions_port: worker_port,
        filters: vec!["jobs.run".to_string()],
    });
    let base_url = spawn_app_with_state(state).await;
    let client = reqwest::Client::new();

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
