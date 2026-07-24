use firelite::{
    functions::{self, FunctionsConfig},
    server,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout, Duration},
};

#[tokio::test]
async fn pubsub_topic_subscription_publish_pull_ack_flow() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let project = "demo-firelite";
    let topic = "events";
    let subscription = "events-sub";

    let created_topic: Value = client
        .put(format!("{base_url}/v1/projects/{project}/topics/{topic}"))
        .json(&json!({ "labels": { "env": "local" } }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created_topic["name"],
        "projects/demo-firelite/topics/events"
    );
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
        pulled_after_ack["receivedMessages"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn pubsub_project_reset_clears_topics_and_subscriptions() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    client
        .put(format!("{base_url}/v1/projects/demo-reset/topics/events"))
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
        .delete(format!("{base_url}/emulator/v1/projects/demo-reset/pubsub"))
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

#[tokio::test]
async fn pubsub_grpc_publish_accepts_node_sdk_shape() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();
    let project = "demo-firelite";
    let topic = "sdk-events";
    let subscription = "sdk-events-sub";

    let created_topic: Value = client
        .put(format!("{base_url}/v1/projects/{project}/topics/{topic}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created_topic["name"],
        "projects/demo-firelite/topics/sdk-events"
    );

    client
        .put(format!(
            "{base_url}/v1/projects/{project}/subscriptions/{subscription}"
        ))
        .json(&json!({ "topic": "projects/demo-firelite/topics/sdk-events" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let grpc_response = client
        .post(format!("{base_url}/google.pubsub.v1.Publisher/Publish"))
        .header("content-type", "application/grpc")
        .body(grpc_frame(publish_request_message(
            "projects/demo-firelite/topics/sdk-events",
            b"hello",
            &[("type", "greeting")],
        )))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let message_ids = decode_publish_response(&grpc_response.bytes().await.unwrap());
    assert_eq!(message_ids.len(), 1);

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
    assert_eq!(received["message"]["data"], "aGVsbG8=");
    assert_eq!(received["message"]["attributes"]["type"], "greeting");
}

#[tokio::test]
async fn pubsub_grpc_publish_auto_creates_missing_topic() {
    let base_url = spawn_app().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base_url}/google.pubsub.v1.Publisher/Publish"))
        .header("content-type", "application/grpc")
        .body(grpc_frame(publish_request_message(
            "projects/demo-firelite/topics/auto-created",
            b"hello",
            &[],
        )))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let topic: Value = client
        .get(format!(
            "{base_url}/v1/projects/demo-firelite/topics/auto-created"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(topic["name"], "projects/demo-firelite/topics/auto-created");
}

#[tokio::test]
async fn pubsub_publish_dispatches_gen1_and_gen2_function_event_shapes() {
    let dir =
        std::env::temp_dir().join(format!("firelite-pubsub-contract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let gen1_path = dir.join("gen1.json");
    let gen2_path = dir.join("gen2.json");
    let gen1_path_js = serde_json::to_string(&gen1_path.to_string_lossy()).unwrap();
    let gen2_path_js = serde_json::to_string(&gen2_path.to_string_lossy()).unwrap();
    std::fs::write(
        dir.join("index.js"),
        format!(
            r#"
const fs = require("node:fs");
exports.gen1 = async (message, context) => {{
  fs.appendFileSync({gen1_path_js}, JSON.stringify({{ message, context }}) + "\n");
}};
exports.gen1.__trigger = {{
  name: "gen1",
  eventTrigger: {{
    eventType: "google.pubsub.topic.publish",
    resource: "projects/demo-firelite/topics/events"
  }}
}};
exports.gen2 = async (event) => {{
  fs.appendFileSync({gen2_path_js}, JSON.stringify(event) + "\n");
}};
exports.gen2.__endpoint = {{
  platform: "gcfv2",
  id: "gen2",
  eventTrigger: {{
    eventType: "google.cloud.pubsub.topic.v1.messagePublished",
    eventFilters: {{ topic: "events" }}
  }}
}};
"#
        ),
    )
    .unwrap();
    let functions_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let functions_addr = functions_listener.local_addr().unwrap();
    drop(functions_listener);
    let prepared = functions::prepare(FunctionsConfig {
        project_id: "demo-firelite".to_string(),
        source_dir: dir,
        addr: functions_addr,
        filters: Vec::new(),
        reload_on_change: false,
    })
    .await
    .unwrap();
    let state =
        server::app_state_with_functions_for_project("demo-firelite", Some(prepared.handle()));
    let base_url = spawn_app_with_state(state).await;
    let client = reqwest::Client::new();

    client
        .put(format!(
            "{base_url}/v1/projects/demo-firelite/topics/events"
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(format!(
            "{base_url}/v1/projects/demo-firelite/topics/events:publish"
        ))
        .json(&json!({
            "messages": [{
                "data": "aGVsbG8=",
                "attributes": { "kind": "greeting" },
                "orderingKey": "ordered"
            }]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    timeout(Duration::from_secs(3), async {
        while !gen1_path.exists() || !gen2_path.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both Pub/Sub functions should receive the event");

    let gen1: Value = serde_json::from_slice(&std::fs::read(&gen1_path).unwrap()).unwrap();
    assert_eq!(gen1["message"]["data"], "aGVsbG8=");
    assert_eq!(gen1["message"]["attributes"]["kind"], "greeting");
    assert_eq!(gen1["message"]["orderingKey"], "ordered");
    assert_eq!(gen1["context"]["eventType"], "google.pubsub.topic.publish");
    assert_eq!(
        gen1["context"]["resource"]["name"],
        "projects/demo-firelite/topics/events"
    );

    let gen2: Value = serde_json::from_slice(&std::fs::read(&gen2_path).unwrap()).unwrap();
    assert_eq!(
        gen2["type"],
        "google.cloud.pubsub.topic.v1.messagePublished"
    );
    assert_eq!(
        gen2["source"],
        "//pubsub.googleapis.com/projects/demo-firelite/topics/events"
    );
    assert_eq!(gen2["data"]["message"]["data"], "aGVsbG8=");
    assert_eq!(gen2["data"]["message"]["orderingKey"], "ordered");
    assert_eq!(
        gen1["message"]["messageId"],
        gen2["data"]["message"]["messageId"]
    );
    assert_eq!(
        gen1["message"]["publishTime"],
        gen2["data"]["message"]["publishTime"]
    );

    client
        .post(format!("{base_url}/google.pubsub.v1.Publisher/Publish"))
        .header("content-type", "application/grpc")
        .body(grpc_frame(publish_request_message(
            "projects/demo-firelite/topics/events",
            b"grpc",
            &[],
        )))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while std::fs::read_to_string(&gen1_path).unwrap().lines().count() < 2
            || std::fs::read_to_string(&gen2_path).unwrap().lines().count() < 2
        {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("gRPC publish should dispatch both functions");

    client
        .post(format!("{base_url}/google.pubsub.v1.Publisher/Publish"))
        .header("content-type", "application/grpc")
        .body(grpc_frame(publish_request_message(
            "projects/other-project/topics/events",
            b"ignored",
            &[],
        )))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(&gen1_path).unwrap().lines().count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(&gen2_path).unwrap().lines().count(),
        2
    );

    client
        .put(format!(
            "{base_url}/v1/projects/demo-firelite/topics/other-events"
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(format!(
            "{base_url}/v1/projects/demo-firelite/topics/other-events:publish"
        ))
        .json(&json!({ "messages": [{ "data": "aWdub3JlZA==" }] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(gen1_path).unwrap().lines().count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(gen2_path).unwrap().lines().count(),
        2
    );

    drop(prepared);
}

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
    });
    format!("http://{addr}")
}

async fn spawn_app_with_state(state: std::sync::Arc<server::AppState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::pubsub_app_with_state(state))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

fn publish_request_message(topic: &str, data: &[u8], attributes: &[(&str, &str)]) -> Vec<u8> {
    let mut message = Vec::new();
    write_len_delimited_field(&mut message, 1, data);
    for (key, value) in attributes {
        let mut entry = Vec::new();
        write_len_delimited_field(&mut entry, 1, key.as_bytes());
        write_len_delimited_field(&mut entry, 2, value.as_bytes());
        write_len_delimited_field(&mut message, 2, &entry);
    }

    let mut request = Vec::new();
    write_len_delimited_field(&mut request, 1, topic.as_bytes());
    write_len_delimited_field(&mut request, 2, &message);
    request
}

fn grpc_frame(message: Vec<u8>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(message.len() + 5);
    frame.push(0);
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(&message);
    frame
}

fn decode_publish_response(frame: &[u8]) -> Vec<String> {
    assert!(frame.len() >= 5);
    assert_eq!(frame[0], 0);
    let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
    assert!(frame.len() >= len + 5);
    let mut cursor = 5;
    let end = cursor + len;
    let mut message_ids = Vec::new();
    while cursor < end {
        let tag = read_varint(frame, &mut cursor);
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        assert_eq!(wire_type, 2);
        let value = read_len_delimited(frame, &mut cursor);
        if field == 1 {
            message_ids.push(String::from_utf8(value.to_vec()).unwrap());
        }
    }
    message_ids
}

fn write_len_delimited_field(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    write_varint(out, (field << 3) | 2);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> u64 {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let byte = bytes[*cursor];
        *cursor += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return result;
        }
        shift += 7;
    }
}

fn read_len_delimited<'a>(bytes: &'a [u8], cursor: &mut usize) -> &'a [u8] {
    let len = read_varint(bytes, cursor) as usize;
    let start = *cursor;
    let end = start + len;
    *cursor = end;
    &bytes[start..end]
}
