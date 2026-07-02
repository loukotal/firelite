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

async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::app()).await.unwrap();
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
