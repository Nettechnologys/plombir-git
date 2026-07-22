//! WebSocket real-time notification push.
//!
//! Clients connect to `ws://host/api/v1/ws/notifications` and authenticate
//! via the `Sec-WebSocket-Protocol` subprotocol header (`bearer.<jwt>`).
//! Query-parameter fallback (`?token=<jwt>`) is retained for backward
//! compatibility but should not be used by new clients.
//!
//! Security: per-user notification channels and per-job log channels ensure
//! clients only receive the streams they explicitly subscribed to.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::AppState;

/// The broadcast channel capacity for real-time notifications.
const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;

/// A notification event sent over WebSocket.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

/// Internal state for the notification hub.
#[derive(Debug)]
struct NotificationHubInner {
    /// Per-user notification channels. Only the owning user receives
    /// messages pushed via `push_notification`.
    user_channels: RwLock<HashMap<i64, broadcast::Sender<NotificationEvent>>>,
    /// Per-job channels prevent logs from unrelated jobs from being fanned
    /// out to every connected client.
    job_channels: RwLock<HashMap<i64, broadcast::Sender<NotificationEvent>>>,
}

/// Global notification hub with per-user isolation.
///
/// Wrapped in `Arc` so it can be cheaply cloned into `AppState`.
#[derive(Debug, Clone)]
pub struct NotificationHub {
    inner: Arc<NotificationHubInner>,
}

impl Default for NotificationHub {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationHub {
    /// Create a new notification hub.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(NotificationHubInner {
                user_channels: RwLock::new(HashMap::new()),
                job_channels: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Push a notification to a **specific user's** channel.
    /// Creates the channel if it doesn't exist yet.
    pub async fn push_notification(&self, user_id: i64, event_type: &str, data: serde_json::Value) {
        let event = NotificationEvent {
            event_type: event_type.to_string(),
            data: serde_json::json!({
                "user_id": user_id,
                "payload": data,
            }),
        };

        let channels = self.inner.user_channels.read().await;
        if let Some(sender) = channels.get(&user_id) {
            let _ = sender.send(event);
        }
        // If the user has no active channel, the notification is silently
        // dropped. The REST API /notifications endpoint will still serve
        // persisted notifications when the user comes online.
    }

    /// Subscribe to a specific user's notification channel.
    /// Creates the channel if it doesn't exist.
    async fn subscribe_user(&self, user_id: i64) -> broadcast::Receiver<NotificationEvent> {
        let mut channels = self.inner.user_channels.write().await;
        let sender = channels
            .entry(user_id)
            .or_insert_with(|| broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    async fn cleanup_user_channel(&self, user_id: i64) {
        let mut channels = self.inner.user_channels.write().await;
        if channels
            .get(&user_id)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            channels.remove(&user_id);
        }
    }

    /// Subscribe to one job's log stream, creating its channel on demand.
    async fn subscribe_job(&self, job_id: i64) -> broadcast::Receiver<NotificationEvent> {
        let mut channels = self.inner.job_channels.write().await;
        let sender = channels
            .entry(job_id)
            .or_insert_with(|| broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    async fn cleanup_job_channel(&self, job_id: i64) {
        let mut channels = self.inner.job_channels.write().await;
        if channels
            .get(&job_id)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            channels.remove(&job_id);
        }
    }

    /// Broadcast a job log update only to subscribers of that job.
    pub async fn push_job_log(&self, job_id: i64, log: &str) {
        let event = NotificationEvent {
            event_type: "job_log".to_string(),
            data: serde_json::json!({
                "job_id": job_id,
                "log": log,
            }),
        };
        let channels = self.inner.job_channels.read().await;
        if let Some(sender) = channels.get(&job_id) {
            let _ = sender.send(event);
        }
    }
}

/// Query params for WebSocket upgrade (backward-compatible token fallback).
#[derive(Deserialize)]
pub struct WsQuery {
    /// JWT token for authentication (legacy — prefer Sec-WebSocket-Protocol).
    token: Option<String>,
}

/// Extract a Bearer token from the `Sec-WebSocket-Protocol` header.
///
/// The browser WebSocket API does not allow setting custom headers, so the
/// subprotocol field is the only way to pass a token without exposing it in
/// the URL (query params leak into server logs, browser history, and the
/// Referer header).
///
/// Returns `Some((protocol_string, token))` if a `bearer.` prefixed protocol
/// is found, `None` otherwise.
fn extract_bearer_from_protocol(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get("sec-websocket-protocol")?.to_str().ok()?;
    for proto in raw.split(',') {
        let trimmed = proto.trim();
        if let Some(token) = trimmed.strip_prefix("bearer.") {
            return Some((trimmed.to_string(), token.to_string()));
        }
    }
    None
}

/// Extract a Bearer token from the `Cookie` header (M-4: HttpOnly cookie auth).
///
/// Returns the raw token string if a valid `ironforge_token` cookie is present.
fn extract_token_from_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    for cookie in cookie_header.split(';') {
        let cookie = cookie.trim();
        if let Some(token) = cookie.strip_prefix("ironforge_token=") {
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// GET /api/v1/ws/notifications — WebSocket upgrade handler.
pub async fn ws_notifications_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<WsQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // M-4/M-5: Authenticate via HttpOnly cookie (preferred for browsers),
    // then Sec-WebSocket-Protocol subprotocol, then query parameter (legacy).
    let (proto_echo, token) = match extract_token_from_cookie(&headers) {
        Some(t) => (None, Some(t)),
        None => match extract_bearer_from_protocol(&headers) {
            Some((proto, token)) => (Some(proto), Some(token)),
            None => (None, query.token),
        },
    };

    let user_id = token
        .as_deref()
        .and_then(|t| rg_core::auth::jwt::validate_token(t, &state.jwt_secret))
        .and_then(|c| c.sub.parse::<i64>().ok());

    let upgrade = if let Some(proto) = proto_echo {
        ws.protocols([proto])
    } else {
        ws
    };

    upgrade.on_upgrade(move |socket| {
        handle_ws_connection(socket, state.notification_hub.clone(), user_id)
    })
}

/// Handle an individual WebSocket connection.
async fn handle_ws_connection(socket: WebSocket, hub: NotificationHub, user_id: Option<i64>) {
    let (mut sender, mut receiver) = socket.split();

    if user_id.is_none() {
        let _ = sender
            .send(Message::Text(
                serde_json::json!({"error": "authentication required"})
                    .to_string()
                    .into(),
            ))
            .await;
        let _ = sender.close().await;
        return;
    }

    // user_id is guaranteed Some at this point due to the guard above.
    let Some(uid) = user_id else {
        tracing::error!("user_id became None after guard check — this is a logic bug");
        return;
    };
    tracing::info!(
        user_id = uid,
        "WebSocket client connected for notifications"
    );

    // General notifications never receive job logs. Those are isolated on
    // the dedicated /ws/job/:job_id endpoint.
    let mut user_rx = hub.subscribe_user(uid).await;

    // Send initial connection confirmation
    let welcome = serde_json::json!({
        "type": "connected",
        "user_id": uid,
    });
    if sender
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        drop(user_rx);
        hub.cleanup_user_channel(uid).await;
        return;
    }

    loop {
        tokio::select! {
            event = user_rx.recv() => match event {
                Ok(event) => {
                    let Ok(msg) = serde_json::to_string(&event) else {
                        continue;
                    };
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(user_id = uid, skipped, "notification WebSocket lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Ping(payload))) => {
                    if sender.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    }

    drop(user_rx);
    hub.cleanup_user_channel(uid).await;
    tracing::info!(user_id = uid, "WebSocket client disconnected");
}

/// Push a notification to the WebSocket hub for real-time delivery.
///
/// Spawns an async task to send to the user's channel without blocking
/// the caller. The notification is also persisted in the database, so
/// offline users will see it via the REST API on next fetch.
pub fn push_notification(
    hub: &NotificationHub,
    user_id: i64,
    event_type: &str,
    data: serde_json::Value,
) {
    let hub = hub.clone();
    let event_type = event_type.to_string();
    tokio::spawn(async move {
        hub.push_notification(user_id, &event_type, data).await;
    });
}

/// Broadcast a job log update to subscribers of that job.
pub async fn push_job_log(hub: &NotificationHub, job_id: i64, log: &str) {
    hub.push_job_log(job_id, log).await;
}

/// GET /api/v1/ws/job/:job_id — WebSocket for real-time job log streaming.
///
/// Authenticates via `Sec-WebSocket-Protocol: bearer.<jwt>` subprotocol
/// (preferred) or `?token=<jwt>` query parameter (legacy fallback).
/// Frontend subscribes to receive `job_log` events filtered by the specified job_id.
pub async fn ws_job_log_handler(
    ws: WebSocketUpgrade,
    Path(job_id): Path<i64>,
    Query(query): Query<WsQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // M-4/M-5: Authenticate via cookie (preferred), subprotocol, or query param
    let (proto_echo, token) = match extract_token_from_cookie(&headers) {
        Some(t) => (None, Some(t)),
        None => match extract_bearer_from_protocol(&headers) {
            Some((proto, token)) => (Some(proto), Some(token)),
            None => (None, query.token),
        },
    };

    let user_id = token
        .as_deref()
        .and_then(|t| rg_core::auth::jwt::validate_token(t, &state.jwt_secret))
        .and_then(|c| c.sub.parse::<i64>().ok());

    let Some(user_id) = user_id else {
        return crate::error::AppError::unauthorized("authentication required").into_response();
    };

    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return crate::error::AppError::not_found("job not found").into_response(),
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return crate::error::AppError::not_found("job not found").into_response(),
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return crate::error::AppError::not_found("job not found").into_response(),
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    };
    let repository = match rg_db::ops::repo_ops::find_by_id(&state.db, pipeline.repo_id).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return crate::error::AppError::not_found("job not found").into_response(),
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    };
    match rg_core::repo::service::can_read_repo(&state.db, &repository, Some(user_id)).await {
        Ok(true) => {}
        Ok(false) => return crate::error::AppError::forbidden("access denied").into_response(),
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    }

    let upgrade = if let Some(proto) = proto_echo {
        ws.protocols([proto])
    } else {
        ws
    };

    upgrade
        .on_upgrade(move |socket| {
            handle_job_log_connection(socket, state.notification_hub.clone(), job_id, user_id)
        })
        .into_response()
}

/// Handle a job log WebSocket connection.
async fn handle_job_log_connection(
    socket: WebSocket,
    hub: NotificationHub,
    job_id: i64,
    user_id: i64,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = hub.subscribe_job(job_id).await;

    // Send confirmation
    let welcome = serde_json::json!({
        "type": "connected",
        "job_id": job_id,
    });
    if sender
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        drop(rx);
        hub.cleanup_job_channel(job_id).await;
        return;
    }

    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => {
                    let Ok(msg) = serde_json::to_string(&event) else {
                        continue;
                    };
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(job_id, user_id, skipped, "job log WebSocket lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Ping(payload))) => {
                    if sender.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    }

    drop(rx);
    hub.cleanup_job_channel(job_id).await;
    tracing::info!(job_id, user_id, "job log WebSocket client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn job_log_channels_are_isolated_and_reclaimed() {
        let hub = NotificationHub::new();
        let mut job_one = hub.subscribe_job(101).await;
        let mut job_two = hub.subscribe_job(202).await;

        let producer_one = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.push_job_log(101, "one").await })
        };
        let producer_two = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.push_job_log(202, "two").await })
        };
        producer_one.await.unwrap();
        producer_two.await.unwrap();

        let event_one = timeout(Duration::from_secs(1), job_one.recv())
            .await
            .unwrap()
            .unwrap();
        let event_two = timeout(Duration::from_secs(1), job_two.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event_one.data["job_id"], 101);
        assert_eq!(event_one.data["log"], "one");
        assert_eq!(event_two.data["job_id"], 202);
        assert_eq!(event_two.data["log"], "two");
        assert!(timeout(Duration::from_millis(25), job_one.recv())
            .await
            .is_err());
        assert!(timeout(Duration::from_millis(25), job_two.recv())
            .await
            .is_err());

        drop(job_one);
        hub.cleanup_job_channel(101).await;
        assert!(!hub.inner.job_channels.read().await.contains_key(&101));
        assert!(hub.inner.job_channels.read().await.contains_key(&202));

        drop(job_two);
        hub.cleanup_job_channel(202).await;
        assert!(hub.inner.job_channels.read().await.is_empty());
    }

    #[tokio::test]
    async fn pushes_without_subscribers_do_not_create_channels() {
        let hub = NotificationHub::new();
        hub.push_job_log(303, "offline").await;
        assert!(hub.inner.job_channels.read().await.is_empty());
    }

    #[tokio::test]
    async fn user_notification_channels_are_reclaimed() {
        let hub = NotificationHub::new();
        let receiver = hub.subscribe_user(7).await;
        assert!(hub.inner.user_channels.read().await.contains_key(&7));

        drop(receiver);
        hub.cleanup_user_channel(7).await;
        assert!(hub.inner.user_channels.read().await.is_empty());
    }
}
