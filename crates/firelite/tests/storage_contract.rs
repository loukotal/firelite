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

#[tokio::test]
async fn storage_gcs_resumable_upload_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let bucket = "demo-firelite.appspot.com";

    let started = client
        .post(format!(
            "{base_url}/upload/storage/v1/b/{bucket}/o?uploadType=resumable&name=reports/bank.csv"
        ))
        .header("x-upload-content-type", "text/csv")
        .json(&serde_json::json!({
            "name": "reports/bank.csv",
            "contentType": "text/csv"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let location = started
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(&base_url));
    assert!(location.contains("uploadType=resumable"));

    let uploaded: Value = client
        .put(location)
        .header("content-type", "text/csv")
        .header("content-range", "bytes 0-15/16")
        .body("date,amount\n1,2\n")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(uploaded["bucket"], bucket);
    assert_eq!(uploaded["name"], "reports/bank.csv");
    assert_eq!(uploaded["contentType"], "text/csv");
    assert_eq!(uploaded["size"], "16");
}

#[tokio::test]
async fn storage_firebase_resumable_upload_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let bucket = "demo-firelite.appspot.com";
    let started = client
        .post(format!(
            "{base_url}/v0/b/{bucket}/o?name=leases/agreement.pdf"
        ))
        .header("x-goog-upload-protocol", "resumable")
        .header("x-goog-upload-command", "start")
        .header("x-goog-upload-header-content-length", "8")
        .header("x-goog-upload-header-content-type", "application/pdf")
        .json(&serde_json::json!({ "name": "leases/agreement.pdf" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(started.headers()["x-goog-upload-status"], "active");
    let upload_url = started.headers()["x-goog-upload-url"]
        .to_str()
        .unwrap()
        .to_string();

    let first = client
        .post(&upload_url)
        .header("x-goog-upload-command", "upload")
        .header("x-goog-upload-offset", "0")
        .body("abcd")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(first.headers()["x-goog-upload-status"], "active");

    let query = client
        .post(&upload_url)
        .header("x-goog-upload-command", "query")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(query.headers()["x-goog-upload-size-received"], "4");

    let finalized: Value = client
        .post(&upload_url)
        .header("x-goog-upload-command", "upload, finalize")
        .header("x-goog-upload-offset", "4")
        .body("efgh")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(finalized["name"], "leases/agreement.pdf");
    assert_eq!(finalized["size"], "8");
    assert_eq!(finalized["contentType"], "application/pdf");

    let completed = client
        .post(&upload_url)
        .header("x-goog-upload-command", "query")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(completed.headers()["x-goog-upload-status"], "final");
    assert_eq!(completed.headers()["x-goog-upload-size-received"], "8");

    let downloaded = client
        .get(format!(
            "{base_url}/v0/b/{bucket}/o/leases%2Fagreement.pdf?alt=media"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&downloaded[..], b"abcdefgh");
}

#[tokio::test]
async fn storage_firebase_resumable_rejects_chunk_without_advancing_offset() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let started = client
        .post(format!(
            "{base_url}/v0/b/demo-firelite.appspot.com/o?name=too-large.txt"
        ))
        .header("x-goog-upload-protocol", "resumable")
        .header("x-goog-upload-command", "start")
        .header("x-goog-upload-header-content-length", "4")
        .json(&serde_json::json!({ "name": "too-large.txt" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let upload_url = started.headers()["x-goog-upload-url"].to_str().unwrap();

    let rejected = client
        .post(upload_url)
        .header("x-goog-upload-command", "upload")
        .header("x-goog-upload-offset", "0")
        .body("12345")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let query = client
        .post(upload_url)
        .header("x-goog-upload-command", "query")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(query.headers()["x-goog-upload-size-received"], "0");
}

#[tokio::test]
async fn storage_firebase_upload_preflight_exposes_resumable_headers() {
    let base_url = spawn_app().await;
    let response = reqwest::Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("{base_url}/v0/b/demo-firelite.appspot.com/o"),
        )
        .header("origin", "http://localhost:3010")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "x-goog-upload-command,x-goog-upload-offset",
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let allowed = response.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap();
    assert!(allowed.contains("x-goog-upload-offset"));
    let exposed = response.headers()["access-control-expose-headers"]
        .to_str()
        .unwrap();
    assert!(exposed.contains("x-goog-upload-url"));
    assert!(exposed.contains("x-goog-upload-status"));
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}
