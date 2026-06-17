use firelite::server;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::net::TcpListener;

#[tokio::test]
async fn storage_gcs_upload_metadata_download_list_delete_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let bucket = "demo-firelite.appspot.com";
    let object = "folder%2Fhello.txt";

    let uploaded: Value = client
        .post(format!(
            "{base_url}/upload/storage/v1/b/{bucket}/o?uploadType=media&name=folder/hello.txt"
        ))
        .header("content-type", "text/plain")
        .body("hello storage")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(uploaded["bucket"], bucket);
    assert_eq!(uploaded["name"], "folder/hello.txt");
    assert_eq!(uploaded["size"], "13");
    assert_eq!(uploaded["contentType"], "text/plain");

    let metadata: Value = client
        .get(format!("{base_url}/storage/v1/b/{bucket}/o/{object}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["name"], "folder/hello.txt");

    let downloaded = client
        .get(format!(
            "{base_url}/storage/v1/b/{bucket}/o/{object}?alt=media"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(downloaded, "hello storage");

    let listed: Value = client
        .get(format!("{base_url}/storage/v1/b/{bucket}/o?prefix=folder/"))
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
        .delete(format!("{base_url}/storage/v1/b/{bucket}/o/{object}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let missing = client
        .get(format!("{base_url}/storage/v1/b/{bucket}/o/{object}"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn storage_firebase_v0_upload_and_project_scoped_reset() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let bucket = "demo-storage.appspot.com";

    client
        .post(format!("{base_url}/v0/b/{bucket}/o?name=avatars/alice.png"))
        .header("content-type", "image/png")
        .body(vec![0, 1, 2, 3])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed: Value = client
        .get(format!(
            "{base_url}/emulator/v1/projects/demo-storage/storage/buckets/{bucket}/objects"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["items"][0]["name"], "avatars/alice.png");

    client
        .delete(format!(
            "{base_url}/emulator/v1/projects/demo-storage/storage/buckets/{bucket}/objects"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed_after_reset: Value = client
        .get(format!("{base_url}/v0/b/{bucket}/o"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed_after_reset["items"].as_array().unwrap().len(), 0);
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}
