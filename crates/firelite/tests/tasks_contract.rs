use axum::{
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::post,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use firelite::server;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc};

#[derive(Debug)]
struct CapturedRequest {
    body: Bytes,
    queue_name: String,
    task_name: String,
}

#[tokio::test]
async fn cloud_tasks_create_dispatch_list_delete_flow() {
    let (target_url, mut received) = spawn_target().await;
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let queue_path = "projects/demo-firelite/locations/us-central1/queues/sendEmail";

    let created: Value = client
        .post(format!("{base_url}/v2/{queue_path}/tasks"))
        .json(&json!({
            "task": {
                "httpRequest": {
                    "httpMethod": "POST",
                    "url": target_url,
                    "headers": { "content-type": "application/json" },
                    "body": BASE64.encode(r#"{"email":"alice@example.test"}"#)
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
    let task_name = created["name"].as_str().unwrap();
    assert!(task_name.starts_with(&format!("{queue_path}/tasks/")));

    let captured = received.recv().await.expect("task dispatch request");
    assert_eq!(captured.body, Bytes::from_static(br#"{"email":"alice@example.test"}"#));
    assert_eq!(captured.queue_name, queue_path);
    assert_eq!(captured.task_name, task_name);

    let listed: Value = client
        .get(format!("{base_url}/v2/{queue_path}/tasks"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);

    let task_id = task_name.rsplit('/').next().unwrap();
    client
        .delete(format!("{base_url}/v2/{queue_path}/tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed_after_delete: Value = client
        .get(format!("{base_url}/v2/{queue_path}/tasks"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed_after_delete["tasks"].as_array().unwrap().len(), 0);
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_target() -> (String, mpsc::Receiver<CapturedRequest>) {
    let (tx, rx) = mpsc::channel(1);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/dispatch", post(capture_dispatch))
        .with_state(tx);

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{addr}/dispatch"), rx)
}

async fn capture_dispatch(
    State(tx): State<mpsc::Sender<CapturedRequest>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let captured = CapturedRequest {
        body,
        queue_name: header_value(&headers, "x-cloudtasks-queuename"),
        task_name: header_value(&headers, "x-cloudtasks-taskname"),
    };
    tx.send(captured).await.unwrap();
    "ok"
}

fn header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}
