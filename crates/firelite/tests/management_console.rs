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

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}
