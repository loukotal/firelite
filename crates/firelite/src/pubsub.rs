use crate::server::AppState;
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use http_body_util::{BodyExt, Full};
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
            "/google.pubsub.v1.Publisher/CreateTopic",
            post(grpc_create_topic),
        )
        .route("/google.pubsub.v1.Publisher/Publish", post(grpc_publish))
        .route(
            "/google.pubsub.v1.Subscriber/CreateSubscription",
            post(grpc_create_subscription),
        )
        .route("/google.pubsub.v1.Subscriber/Pull", post(grpc_pull))
        .route(
            "/google.pubsub.v1.Subscriber/Acknowledge",
            post(grpc_acknowledge),
        )
        .route("/v1/projects/:project_id/topics", get(list_topics))
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
    let projects = state.pubsub.projects.read().expect("pubsub state poisoned");
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
    let projects = state.pubsub.projects.read().expect("pubsub state poisoned");
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
    Ok(Json(publish_messages(
        state,
        project_id,
        topic_name,
        payload.messages,
        false,
    )?))
}

async fn grpc_publish(State(state): State<SharedState>, body: Bytes) -> Response {
    match decode_grpc_publish_request(&body)
        .and_then(|request| {
            let (project_id, topic_name) = parse_topic_name(&request.topic)
                .ok_or_else(|| GrpcError::new(3, "invalid topic name"))?;
            publish_messages(state, project_id, topic_name, request.messages, true)
                .map_err(GrpcError::from)
        })
        .map(|response| encode_publish_response(&response.message_ids))
    {
        Ok(payload) => grpc_response(payload),
        Err(err) => grpc_error_response(err),
    }
}

async fn grpc_create_topic(State(state): State<SharedState>, body: Bytes) -> Response {
    match decode_grpc_topic_request(&body)
        .and_then(|request| {
            let (project_id, topic_name) = parse_topic_name(&request.name)
                .ok_or_else(|| GrpcError::new(3, "invalid topic name"))?;
            let mut projects = state
                .pubsub
                .projects
                .write()
                .expect("pubsub state poisoned");
            let project = projects.entry(project_id).or_default();
            let record = project.topics.entry(topic_name.clone()).or_default();
            record.labels = request.labels;
            Ok(encode_topic_response(&topic_name, record))
        })
        .map(grpc_response)
    {
        Ok(response) => response,
        Err(err) => grpc_error_response(err),
    }
}

async fn grpc_create_subscription(State(state): State<SharedState>, body: Bytes) -> Response {
    match decode_grpc_subscription_request(&body)
        .and_then(|request| {
            let (project_id, subscription_id) = parse_subscription_name(&request.name)
                .ok_or_else(|| GrpcError::new(3, "invalid subscription name"))?;
            let topic = normalize_topic_name(&project_id, &request.topic);
            let mut projects = state
                .pubsub
                .projects
                .write()
                .expect("pubsub state poisoned");
            let project = projects.entry(project_id.clone()).or_default();
            project
                .topics
                .entry(topic.clone())
                .or_insert_with(TopicRecord::default);
            let record = SubscriptionRecord {
                topic,
                ack_deadline_seconds: request.ack_deadline_seconds.unwrap_or(10),
                pending: VecDeque::new(),
                outstanding: BTreeMap::new(),
            };
            let subscription_name = subscription_name(&project_id, &subscription_id);
            project
                .subscriptions
                .insert(subscription_name.clone(), record.clone());
            Ok(encode_subscription_response(&subscription_name, &record))
        })
        .map(grpc_response)
    {
        Ok(response) => response,
        Err(err) => grpc_error_response(err),
    }
}

async fn grpc_pull(State(state): State<SharedState>, body: Bytes) -> Response {
    match decode_grpc_pull_request(&body)
        .and_then(|request| {
            let (project_id, subscription_id) = parse_subscription_name(&request.subscription)
                .ok_or_else(|| GrpcError::new(3, "invalid subscription name"))?;
            let subscription_name = subscription_name(&project_id, &subscription_id);
            let max_messages = request.max_messages.unwrap_or(1).max(1) as usize;
            let mut projects = state
                .pubsub
                .projects
                .write()
                .expect("pubsub state poisoned");
            let project = projects
                .get_mut(&project_id)
                .ok_or_else(|| GrpcError::new(5, "SUBSCRIPTION_NOT_FOUND"))?;
            let mut received_messages = Vec::new();
            for _ in 0..max_messages {
                let message = {
                    let subscription = project
                        .subscriptions
                        .get_mut(&subscription_name)
                        .ok_or_else(|| GrpcError::new(5, "SUBSCRIPTION_NOT_FOUND"))?;
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
                    .ok_or_else(|| GrpcError::new(5, "SUBSCRIPTION_NOT_FOUND"))?;
                subscription
                    .outstanding
                    .insert(ack_id.clone(), message.clone());
                received_messages.push(ReceivedMessage { ack_id, message });
            }
            Ok(encode_pull_response(&received_messages))
        })
        .map(grpc_response)
    {
        Ok(response) => response,
        Err(err) => grpc_error_response(err),
    }
}

async fn grpc_acknowledge(State(state): State<SharedState>, body: Bytes) -> Response {
    match decode_grpc_acknowledge_request(&body)
        .and_then(|request| {
            let (project_id, subscription_id) = parse_subscription_name(&request.subscription)
                .ok_or_else(|| GrpcError::new(3, "invalid subscription name"))?;
            let subscription_name = subscription_name(&project_id, &subscription_id);
            let mut projects = state
                .pubsub
                .projects
                .write()
                .expect("pubsub state poisoned");
            let subscription = projects
                .get_mut(&project_id)
                .and_then(|project| project.subscriptions.get_mut(&subscription_name))
                .ok_or_else(|| GrpcError::new(5, "SUBSCRIPTION_NOT_FOUND"))?;
            for ack_id in request.ack_ids {
                subscription.outstanding.remove(&ack_id);
            }
            Ok(Vec::new())
        })
        .map(grpc_response)
    {
        Ok(response) => response,
        Err(err) => grpc_error_response(err),
    }
}

fn publish_messages(
    state: SharedState,
    project_id: String,
    topic_name: String,
    messages: Vec<PublishMessage>,
    create_topic_if_missing: bool,
) -> Result<PublishResponse, PubsubError> {
    let mut projects = state
        .pubsub
        .projects
        .write()
        .expect("pubsub state poisoned");
    let project = if create_topic_if_missing {
        projects.entry(project_id).or_default()
    } else {
        projects
            .get_mut(&project_id)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"))?
    };
    if !project.topics.contains_key(&topic_name) {
        if create_topic_if_missing {
            project
                .topics
                .insert(topic_name.clone(), TopicRecord::default());
        } else {
            return Err(error(StatusCode::NOT_FOUND, "TOPIC_NOT_FOUND"));
        }
    }

    let mut message_ids = Vec::with_capacity(messages.len());
    for input in messages {
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

    Ok(PublishResponse { message_ids })
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
    let projects = state.pubsub.projects.read().expect("pubsub state poisoned");
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
    let projects = state.pubsub.projects.read().expect("pubsub state poisoned");
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
        return Ok(
            pull_subscription(state, project_id, subscription.to_string(), request)
                .await?
                .into_response(),
        );
    }

    if let Some(subscription) = subscription_action.strip_suffix(":acknowledge") {
        let request = serde_json::from_value(payload)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "INVALID_ACKNOWLEDGE_REQUEST"))?;
        return Ok(
            acknowledge_subscription(state, project_id, subscription.to_string(), request)
                .await?
                .into_response(),
        );
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
    let projects = state.pubsub.projects.read().expect("pubsub state poisoned");
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

fn parse_topic_name(topic: &str) -> Option<(String, String)> {
    let mut parts = topic.split('/');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("projects"), Some(project_id), Some("topics"), Some(topic_id), None) => {
            Some((project_id.to_string(), topic_name(project_id, topic_id)))
        }
        _ => None,
    }
}

fn parse_subscription_name(subscription: &str) -> Option<(String, String)> {
    let mut parts = subscription.split('/');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (
            Some("projects"),
            Some(project_id),
            Some("subscriptions"),
            Some(subscription_id),
            None,
        ) => Some((project_id.to_string(), subscription_id.to_string())),
        _ => None,
    }
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

#[derive(Debug)]
struct GrpcPublishRequest {
    topic: String,
    messages: Vec<PublishMessage>,
}

#[derive(Debug)]
struct GrpcTopicRequest {
    name: String,
    labels: BTreeMap<String, String>,
}

#[derive(Debug)]
struct GrpcSubscriptionRequest {
    name: String,
    topic: String,
    ack_deadline_seconds: Option<i32>,
}

#[derive(Debug)]
struct GrpcPullRequest {
    subscription: String,
    max_messages: Option<i32>,
}

#[derive(Debug)]
struct GrpcAcknowledgeRequest {
    subscription: String,
    ack_ids: Vec<String>,
}

#[derive(Debug)]
struct GrpcError {
    code: u8,
    message: String,
}

impl GrpcError {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<PubsubError> for GrpcError {
    fn from(value: PubsubError) -> Self {
        let code = match value.status {
            StatusCode::NOT_FOUND => 5,
            StatusCode::BAD_REQUEST => 3,
            _ => 13,
        };
        Self::new(code, value.message)
    }
}

fn decode_grpc_publish_request(body: &[u8]) -> Result<GrpcPublishRequest, GrpcError> {
    decode_publish_request_message(decode_grpc_frame(body)?)
}

fn decode_grpc_topic_request(body: &[u8]) -> Result<GrpcTopicRequest, GrpcError> {
    let bytes = decode_grpc_frame(body)?;
    let mut cursor = 0;
    let mut name = None;
    let mut labels = BTreeMap::new();

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => name = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => {
                let entry = read_len_delimited(bytes, &mut cursor)?;
                if let Some((key, value)) = decode_string_map_entry(entry)? {
                    labels.insert(key, value);
                }
            }
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(GrpcTopicRequest {
        name: name.ok_or_else(|| GrpcError::new(3, "missing topic name"))?,
        labels,
    })
}

fn decode_grpc_subscription_request(body: &[u8]) -> Result<GrpcSubscriptionRequest, GrpcError> {
    let bytes = decode_grpc_frame(body)?;
    let mut cursor = 0;
    let mut name = None;
    let mut topic = None;
    let mut ack_deadline_seconds = None;

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => name = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => topic = Some(read_string(bytes, &mut cursor)?),
            (5, 0) => ack_deadline_seconds = Some(read_varint(bytes, &mut cursor)? as i32),
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(GrpcSubscriptionRequest {
        name: name.ok_or_else(|| GrpcError::new(3, "missing subscription name"))?,
        topic: topic.ok_or_else(|| GrpcError::new(3, "missing subscription topic"))?,
        ack_deadline_seconds,
    })
}

fn decode_grpc_pull_request(body: &[u8]) -> Result<GrpcPullRequest, GrpcError> {
    let bytes = decode_grpc_frame(body)?;
    let mut cursor = 0;
    let mut subscription = None;
    let mut max_messages = None;

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => subscription = Some(read_string(bytes, &mut cursor)?),
            (3, 0) => max_messages = Some(read_varint(bytes, &mut cursor)? as i32),
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(GrpcPullRequest {
        subscription: subscription.ok_or_else(|| GrpcError::new(3, "missing subscription"))?,
        max_messages,
    })
}

fn decode_grpc_acknowledge_request(body: &[u8]) -> Result<GrpcAcknowledgeRequest, GrpcError> {
    let bytes = decode_grpc_frame(body)?;
    let mut cursor = 0;
    let mut subscription = None;
    let mut ack_ids = Vec::new();

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => subscription = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => ack_ids.push(read_string(bytes, &mut cursor)?),
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(GrpcAcknowledgeRequest {
        subscription: subscription.ok_or_else(|| GrpcError::new(3, "missing subscription"))?,
        ack_ids,
    })
}

fn decode_grpc_frame(body: &[u8]) -> Result<&[u8], GrpcError> {
    if body.len() < 5 {
        return Err(GrpcError::new(3, "missing grpc frame"));
    }
    if body[0] != 0 {
        return Err(GrpcError::new(
            12,
            "compressed grpc frames are not supported",
        ));
    }
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let end = 5usize
        .checked_add(len)
        .ok_or_else(|| GrpcError::new(3, "invalid grpc frame length"))?;
    if body.len() < end {
        return Err(GrpcError::new(3, "truncated grpc frame"));
    }

    Ok(&body[5..end])
}

fn decode_publish_request_message(bytes: &[u8]) -> Result<GrpcPublishRequest, GrpcError> {
    let mut cursor = 0;
    let mut topic = None;
    let mut messages = Vec::new();

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => topic = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => {
                let message_bytes = read_len_delimited(bytes, &mut cursor)?;
                messages.push(decode_pubsub_message(message_bytes)?);
            }
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(GrpcPublishRequest {
        topic: topic.ok_or_else(|| GrpcError::new(3, "missing topic"))?,
        messages,
    })
}

fn decode_pubsub_message(bytes: &[u8]) -> Result<PublishMessage, GrpcError> {
    let mut cursor = 0;
    let mut data = String::new();
    let mut attributes = BTreeMap::new();
    let mut ordering_key = None;

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => data = BASE64.encode(read_len_delimited(bytes, &mut cursor)?),
            (2, 2) => {
                let entry = read_len_delimited(bytes, &mut cursor)?;
                if let Some((key, value)) = decode_string_map_entry(entry)? {
                    attributes.insert(key, value);
                }
            }
            (5, 2) => ordering_key = Some(read_string(bytes, &mut cursor)?),
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(PublishMessage {
        data,
        attributes,
        ordering_key,
    })
}

fn decode_string_map_entry(bytes: &[u8]) -> Result<Option<(String, String)>, GrpcError> {
    let mut cursor = 0;
    let mut key = None;
    let mut value = None;

    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        match (field, wire_type) {
            (1, 2) => key = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => value = Some(read_string(bytes, &mut cursor)?),
            _ => skip_field(bytes, &mut cursor, wire_type)?,
        }
    }

    Ok(key.zip(value))
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, GrpcError> {
    let mut result = 0u64;
    let mut shift = 0;
    while shift < 64 {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| GrpcError::new(3, "truncated varint"))?;
        *cursor += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
    Err(GrpcError::new(3, "invalid varint"))
}

fn read_len_delimited<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], GrpcError> {
    let len = read_varint(bytes, cursor)? as usize;
    let end = (*cursor)
        .checked_add(len)
        .ok_or_else(|| GrpcError::new(3, "invalid length-delimited field"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| GrpcError::new(3, "truncated length-delimited field"))?;
    *cursor = end;
    Ok(value)
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Result<String, GrpcError> {
    String::from_utf8(read_len_delimited(bytes, cursor)?.to_vec())
        .map_err(|_| GrpcError::new(3, "invalid utf-8 string"))
}

fn skip_field(bytes: &[u8], cursor: &mut usize, wire_type: u64) -> Result<(), GrpcError> {
    match wire_type {
        0 => {
            read_varint(bytes, cursor)?;
        }
        1 => {
            *cursor = (*cursor)
                .checked_add(8)
                .ok_or_else(|| GrpcError::new(3, "invalid fixed64 field"))?;
        }
        2 => {
            read_len_delimited(bytes, cursor)?;
        }
        5 => {
            *cursor = (*cursor)
                .checked_add(4)
                .ok_or_else(|| GrpcError::new(3, "invalid fixed32 field"))?;
        }
        _ => return Err(GrpcError::new(3, "unsupported protobuf wire type")),
    }

    if *cursor > bytes.len() {
        return Err(GrpcError::new(3, "truncated protobuf field"));
    }
    Ok(())
}

fn encode_publish_response(message_ids: &[String]) -> Vec<u8> {
    let mut payload = Vec::new();
    for message_id in message_ids {
        write_len_delimited_field(&mut payload, 1, message_id.as_bytes());
    }
    payload
}

fn encode_topic_response(name: &str, topic: &TopicRecord) -> Vec<u8> {
    let mut payload = Vec::new();
    write_len_delimited_field(&mut payload, 1, name.as_bytes());
    for (key, value) in &topic.labels {
        let mut entry = Vec::new();
        write_len_delimited_field(&mut entry, 1, key.as_bytes());
        write_len_delimited_field(&mut entry, 2, value.as_bytes());
        write_len_delimited_field(&mut payload, 2, &entry);
    }
    payload
}

fn encode_subscription_response(name: &str, subscription: &SubscriptionRecord) -> Vec<u8> {
    let mut payload = Vec::new();
    write_len_delimited_field(&mut payload, 1, name.as_bytes());
    write_len_delimited_field(&mut payload, 2, subscription.topic.as_bytes());
    write_varint_field(
        &mut payload,
        5,
        subscription.ack_deadline_seconds.max(0) as u64,
    );
    payload
}

fn encode_pull_response(received_messages: &[ReceivedMessage]) -> Vec<u8> {
    let mut payload = Vec::new();
    for received in received_messages {
        let mut received_payload = Vec::new();
        write_len_delimited_field(&mut received_payload, 1, received.ack_id.as_bytes());
        let message_payload = encode_grpc_pubsub_message(&received.message);
        write_len_delimited_field(&mut received_payload, 2, &message_payload);
        write_len_delimited_field(&mut payload, 1, &received_payload);
    }
    payload
}

fn encode_grpc_pubsub_message(message: &PubsubMessage) -> Vec<u8> {
    let mut payload = Vec::new();
    let data = BASE64.decode(&message.data).unwrap_or_default();
    write_len_delimited_field(&mut payload, 1, &data);
    for (key, value) in &message.attributes {
        let mut entry = Vec::new();
        write_len_delimited_field(&mut entry, 1, key.as_bytes());
        write_len_delimited_field(&mut entry, 2, value.as_bytes());
        write_len_delimited_field(&mut payload, 2, &entry);
    }
    write_len_delimited_field(&mut payload, 3, message.message_id.as_bytes());
    if let Some(ordering_key) = &message.ordering_key {
        write_len_delimited_field(&mut payload, 5, ordering_key.as_bytes());
    }
    payload
}

fn write_len_delimited_field(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    write_varint(out, (field << 3) | 2);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn write_varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    write_varint(out, field << 3);
    write_varint(out, value);
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn grpc_response(payload: Vec<u8>) -> Response {
    let mut frame = Vec::with_capacity(payload.len() + 5);
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);

    let body = Full::new(Bytes::from(frame)).with_trailers(async {
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", HeaderValue::from_static("0"));
        Some(Ok(trailers))
    });

    let mut response = Body::new(body).into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    response
}

fn grpc_error_response(error: GrpcError) -> Response {
    let mut response = Body::empty().into_response();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    headers.insert(
        "grpc-status",
        HeaderValue::from_str(&error.code.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("13")),
    );
    headers.insert(
        "grpc-message",
        HeaderValue::from_str(&error.message)
            .unwrap_or_else(|_| HeaderValue::from_static("internal")),
    );
    response
}
