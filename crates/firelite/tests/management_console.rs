use firelite::server;
use serde_json::Value;
use tokio::net::TcpListener;

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

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
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
