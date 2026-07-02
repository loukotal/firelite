use crate::server::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/v1/projects/:project_id/topics",
            get(list_topics),
        )
        .route(
            "/v1/projects/:project_id/topics/:topic",
            get(get_topic)
                .put(create_topic)
                .delete(delete_topic)
                .post(publish_topic),
        )
        .route(
            "/v1/projects/:project_id/subscriptions",
            get(list_subscriptions),
        )
        .route(
            "/v1/projects/:project_id/subscriptions/:subscription",
            get(get_subscription)
                .put(create_subscription)
                .delete(delete_subscription)
                .post(subscription_action),
        )
        .route(
            "/emulator/v1/projects/:project_id/pubsub",
            get(project_snapshot).delete(reset_project),
        )
}

#[derive(Debug, Clone, Default)]
pub struct PubsubState {
    projects: Arc<RwLock<HashMap<String, ProjectPubsubState>>>,
}

#[derive(Debug, Clone, Default)]
struct ProjectPubsubState {
    topics: BTreeMap<String, TopicRecord>,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    next_message_id: u64,
    next_ack_id: u64,
}

#[derive(Debug, Clone, Default)]
struct TopicRecord {
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct SubscriptionRecord {
    topic: String,
    ack_deadline_seconds: i32,
    pending: VecDeque<PubsubMessage>,
    outstanding: BTreeMap<String, PubsubMessage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PubsubMessage {
    data: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attributes: BTreeMap<String, String>,
    message_id: String,
    publish_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ordering_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TopicPayload {
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopicResponse {
    name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ListTopicsResponse {
    topics: Vec<TopicResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishRequest {
    #[serde(default)]
    messages: Vec<PublishMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishMessage {
    #[serde(default)]
    data: String,
    #[serde(default)]
    attributes: BTreeMap<String, String>,
    ordering_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishResponse {
    message_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionPayload {
    topic: String,
    ack_deadline_seconds: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionResponse {
    name: String,
    topic: String,
    ack_deadline_seconds: i32,
}

#[derive(Debug, Serialize)]
struct ListSubscriptionsResponse {
    subscriptions: Vec<SubscriptionResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    max_messages: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PullResponse {
    received_messages: Vec<ReceivedMessage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedMessage {
    ack_id: String,
    message: PubsubMessage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcknowledgeRequest {
    #[serde(default)]
    ack_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EmptyResponse {}

#[derive(Debug, Serialize)]
struct ProjectSnapshot {
    topics: Vec<TopicResponse>,
    subscriptions: Vec<SubscriptionResponse>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: u16,
    message: &'static str,
    status: &'static str,
}

#[derive(Debug)]
struct PubsubError {
    status: StatusCode,
    message: &'static str,
}

impl IntoResponse for PubsubError {
    fn into_response(self) -> Response {
        let status = self.status;
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: status.as_u16(),
                message: self.message,
                status: status.canonical_reason().unwrap_or("ERROR"),
            },
        };
        (status, Json(body)).into_response()
    }
}

type PubsubResult<T> = Result<Json<T>, PubsubError>;

async fn create_topic(
    State(state): State<SharedState>,
    Path((project_id, topic)): Path<(String, String)>,
    Json(payload): Json<TopicPayload>,
) -> PubsubResult<TopicResponse> {
    let topic_name = topic_name(&project_id, &topic);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects.entry(project_id).or_default();
    let record = project.topics.entry(topic_name.clone()).or_default();
    record.labels = payload.labels;

    Ok(Json(topic_response(&topic_name, record)))
}

async fn get_topic(
    State(state): State<SharedState>,
    Path((project_id, topic)): Path<(String, String)>,
) -> PubsubResult<TopicResponse> {
    let topic_name = topic_name(&project_id, &topic);
    let projects = state
        .pubsub
        .projects
        .read()
        .expect("pubsub state poisoned");
    let topic = projects
        .get(&project_id)
        .and_then(|project| project.topics.get(&topic_name))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"))?;

    Ok(Json(topic_response(&topic_name, topic)))
}

async fn list_topics(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> PubsubResult<ListTopicsResponse> {
    let projects = state
        .pubsub
        .projects
        .read()
        .expect("pubsub state poisoned");
    let topics = projects
        .get(&project_id)
        .map(|project| {
            project
                .topics
                .iter()
                .map(|(name, topic)| topic_response(name, topic))
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(ListTopicsResponse { topics }))
}

async fn delete_topic(
    State(state): State<SharedState>,
    Path((project_id, topic)): Path<(String, String)>,
) -> PubsubResult<EmptyResponse> {
    let topic_name = topic_name(&project_id, &topic);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"))?;
    if project.topics.remove(&topic_name).is_none() {
        return Err(error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"));
    }
    project
        .subscriptions
        .retain(|_, subscription| subscription.topic != topic_name);

    Ok(Json(EmptyResponse {}))
}

async fn publish_topic(
    State(state): State<SharedState>,
    Path((project_id, topic_action)): Path<(String, String)>,
    Json(payload): Json<PublishRequest>,
) -> PubsubResult<PublishResponse> {
    let topic = topic_action
        .strip_suffix(":publish")
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "UNSUPPORTED_TOPIC_ACTION"))?;
    let topic_name = topic_name(&project_id, topic);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"))?;
    if !project.topics.contains_key(&topic_name) {
        return Err(error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"));
    }

    let mut message_ids = Vec::with_capacity(payload.messages.len());
    for input in payload.messages {
        project.next_message_id += 1;
        let message_id = project.next_message_id.to_string();
        let message = PubsubMessage {
            data: input.data,
            attributes: input.attributes,
            message_id: message_id.clone(),
            publish_time: now_ms().to_string(),
            ordering_key: input.ordering_key,
        };

        for subscription in project.subscriptions.values_mut() {
            if subscription.topic == topic_name {
                subscription.pending.push_back(message.clone());
            }
        }
        message_ids.push(message_id);
    }

    Ok(Json(PublishResponse { message_ids }))
}

async fn create_subscription(
    State(state): State<SharedState>,
    Path((project_id, subscription)): Path<(String, String)>,
    Json(payload): Json<SubscriptionPayload>,
) -> PubsubResult<SubscriptionResponse> {
    let subscription_name = subscription_name(&project_id, &subscription);
    let topic = normalize_topic_name(&project_id, &payload.topic);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"))?;
    if !project.topics.contains_key(&topic) {
        return Err(error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"));
    }

    let record = SubscriptionRecord {
        topic,
        ack_deadline_seconds: payload.ack_deadline_seconds.unwrap_or(10),
        pending: VecDeque::new(),
        outstanding: BTreeMap::new(),
    };
    project
        .subscriptions
        .insert(subscription_name.clone(), record.clone());

    Ok(Json(subscription_response(&subscription_name, &record)))
}

async fn get_subscription(
    State(state): State<SharedState>,
    Path((project_id, subscription)): Path<(String, String)>,
) -> PubsubResult<SubscriptionResponse> {
    let subscription_name = subscription_name(&project_id, &subscription);
    let projects = state
        .pubsub
        .projects
        .read()
        .expect("pubsub state poisoned");
    let subscription = projects
        .get(&project_id)
        .and_then(|project| project.subscriptions.get(&subscription_name))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"))?;

    Ok(Json(subscription_response(
        &subscription_name,
        subscription,
    )))
}

async fn list_subscriptions(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> PubsubResult<ListSubscriptionsResponse> {
    let projects = state
        .pubsub
        .projects
        .read()
        .expect("pubsub state poisoned");
    let subscriptions = projects
        .get(&project_id)
        .map(|project| {
            project
                .subscriptions
                .iter()
                .map(|(name, subscription)| subscription_response(name, subscription))
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(ListSubscriptionsResponse { subscriptions }))
}

async fn delete_subscription(
    State(state): State<SharedState>,
    Path((project_id, subscription)): Path<(String, String)>,
) -> PubsubResult<EmptyResponse> {
    let subscription_name = subscription_name(&project_id, &subscription);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"))?;
    if project.subscriptions.remove(&subscription_name).is_none() {
        return Err(error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"));
    }

    Ok(Json(EmptyResponse {}))
}

async fn subscription_action(
    State(state): State<SharedState>,
    Path((project_id, subscription_action)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Response, PubsubError> {
    if let Some(subscription) = subscription_action.strip_suffix(":pull") {
        let request = serde_json::from_value(payload)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_PULL_REQUEST"))?;
        return Ok(pull_subscription(state, project_id, subscription.to_string(), request)
            .await?
            .into_response());
    }

    if let Some(subscription) = subscription_action.strip_suffix(":acknowledge") {
        let request = serde_json::from_value(payload)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ACKNOWLEDGE_REQUEST"))?;
        return Ok(acknowledge_subscription(state, project_id, subscription.to_string(), request)
            .await?
            .into_response());
    }

    Err(error(
        StatusCode::NOT_FOUND,
        "UNSUPPORTED_SUBSCRIPTION_ACTION",
    ))
}

async fn pull_subscription(
    state: SharedState,
    project_id: String,
    subscription: String,
    request: PullRequest,
) -> PubsubResult<PullResponse> {
    let subscription_name = subscription_name(&project_id, &subscription);
    let max_messages = request.max_messages.unwrap_or(1).max(1);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = projects
        .get_mut(&project_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"))?;

    let mut received_messages = Vec::new();
    for _ in 0..max_messages {
        let message = {
            let subscription = project
                .subscriptions
                .get_mut(&subscription_name)
                .ok_or_else(|| error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"))?;
            subscription.pending.pop_front()
        };

        let Some(message) = message else {
            break;
        };

        project.next_ack_id += 1;
        let ack_id = format!("ack-{}", project.next_ack_id);
        let subscription = project
            .subscriptions
            .get_mut(&subscription_name)
            .expect("subscription disappeared during pull");
        subscription
            .outstanding
            .insert(ack_id.clone(), message.clone());
        received_messages.push(ReceivedMessage { ack_id, message });
    }

    Ok(Json(PullResponse { received_messages }))
}

async fn acknowledge_subscription(
    state: SharedState,
    project_id: String,
    subscription: String,
    request: AcknowledgeRequest,
) -> PubsubResult<EmptyResponse> {
    let subscription_name = subscription_name(&project_id, &subscription);
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let subscription = projects
        .get_mut(&project_id)
        .and_then(|project| project.subscriptions.get_mut(&subscription_name))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "SUBSCRIPTION_NOT_FOUND"))?;
    for ack_id in request.ack_ids {
        subscription.outstanding.remove(&ack_id);
    }

    Ok(Json(EmptyResponse {}))
}

async fn project_snapshot(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> PubsubResult<ProjectSnapshot> {
    let projects = state
        .pubsub
        .projects
        .read()
        .expect("pubsub state poisoned");
    let Some(project) = projects.get(&project_id) else {
        return Ok(Json(ProjectSnapshot {
            topics: Vec::new(),
            subscriptions: Vec::new(),
        }));
    };

    Ok(Json(ProjectSnapshot {
        topics: project
            .topics
            .iter()
            .map(|(name, topic)| topic_response(name, topic))
            .collect(),
        subscriptions: project
            .subscriptions
            .iter()
            .map(|(name, subscription)| subscription_response(name, subscription))
            .collect(),
    }))
}

async fn reset_project(
    State(state): State<SharedState>,
    Path(project_id): Path<String>,
) -> PubsubResult<EmptyResponse> {
    state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned")
        .remove(&project_id);

    Ok(Json(EmptyResponse {}))
}

fn topic_response(name: &str, topic: &TopicRecord) -> TopicResponse {
    TopicResponse {
        name: name.to_string(),
        labels: topic.labels.clone(),
    }
}

fn subscription_response(name: &str, subscription: &SubscriptionRecord) -> SubscriptionResponse {
    SubscriptionResponse {
        name: name.to_string(),
        topic: subscription.topic.clone(),
        ack_deadline_seconds: subscription.ack_deadline_seconds,
    }
}

fn normalize_topic_name(project_id: &str, topic: &str) -> String {
    if topic.starts_with("projects/") {
        topic.to_string()
    } else {
        topic_name(project_id, topic)
    }
}

fn topic_name(project_id: &str, topic: &str) -> String {
    format!("projects/{project_id}/topics/{topic}")
}

fn subscription_name(project_id: &str, subscription: &str) -> String {
    format!("projects/{project_id}/subscriptions/{subscription}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as u64
}

fn error(status: StatusCode, message: &'static str) -> PubsubError {
    PubsubError { status, message }
}
