use firelite::server;
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[tokio::test]
async fn pubsub_topic_subscription_publish_pull_ack_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let project = "demo-firelite";
    let topic = "events";
    let subscription = "events-sub";

    let created_topic: Value = client
        .put(format!(
            "{base_url}/v1/projects/{project}/topics/{topic}"
        ))
        .json(&json!({ "labels": { "env": "local" } }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created_topic["name"], "projects/demo-firelite/topics/events");
    assert_eq!(created_topic["labels"]["env"], "local");

    let created_subscription: Value = client
        .put(format!(
            "{base_url}/v1/projects/{project}/subscriptions/{subscription}"
        ))
        .json(&json!({
            "topic": "projects/demo-firelite/topics/events",
            "ackDeadlineSeconds": 20
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created_subscription["name"],
        "projects/demo-firelite/subscriptions/events-sub"
    );
    assert_eq!(created_subscription["ackDeadlineSeconds"], 20);

    let published: Value = client
        .post(format!(
            "{base_url}/v1/projects/{project}/topics/{topic}:publish"
        ))
        .json(&json!({
            "messages": [{
                "data": "aGVsbG8=",
                "attributes": { "type": "greeting" }
            }]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(published["messageIds"].as_array().unwrap().len(), 1);

    let pulled: Value = client
        .post(format!(
            "{base_url}/v1/projects/{project}/subscriptions/{subscription}:pull"
        ))
        .json(&json!({ "maxMessages": 1 }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let received = &pulled["receivedMessages"][0];
    let ack_id = received["ackId"].as_str().unwrap();
    assert_eq!(received["message"]["data"], "aGVsbG8=");
    assert_eq!(received["message"]["attributes"]["type"], "greeting");

    client
        .post(format!(
            "{base_url}/v1/projects/{project}/subscriptions/{subscription}:acknowledge"
        ))
        .json(&json!({ "ackIds": [ack_id] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let pulled_after_ack: Value = client
        .post(format!(
            "{base_url}/v1/projects/{project}/subscriptions/{subscription}:pull"
        ))
        .json(&json!({ "maxMessages": 1 }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        pulled_after_ack["receivedMessages"].as_array().unwrap().len(),
        0
    );
}

#[tokio::test]
async fn pubsub_project_reset_clears_topics_and_subscriptions() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    client
        .put(format!(
            "{base_url}/v1/projects/demo-reset/topics/events"
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .put(format!(
            "{base_url}/v1/projects/demo-reset/subscriptions/events-sub"
        ))
        .json(&json!({ "topic": "projects/demo-reset/topics/events" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    client
        .delete(format!(
            "{base_url}/emulator/v1/projects/demo-reset/pubsub"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let listed: Value = client
        .get(format!("{base_url}/v1/projects/demo-reset/topics"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["topics"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn pubsub_publish_requires_existing_topic() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    let missing = client
        .post(format!(
            "{base_url}/v1/projects/demo-firelite/topics/missing:publish"
        ))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .unwrap();

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}
