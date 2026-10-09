//! WebSocket real-time notification push.
//!
//! Clients connect to `ws://host/api/v1/ws/notifications` and authenticate with
//! the HttpOnly session cookie (what a browser sends on a same-origin upgrade)
//! or a `Sec-WebSocket-Protocol: bearer.<jwt>` subprotocol. The legacy
//! `?token=<jwt>` query parameter is no longer accepted: a URL is copied into
//! access logs, proxy logs, browser history and `Referer` headers, and the
//! request span used to carry it into every log line the server wrote
//! (security audit finding #9).
//!
//! Reading a session out of a handshake is not this module's job — it belongs to
//! [`crate::api::auth::ws_session`], which owns the cookie's name and is shared
//! with the revocation gate. This module holds the socket loops.
//!
//! Security: per-user notification channels and per-job log channels ensure
//! clients only receive the streams they explicitly subscribed to.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::api::auth::{ws_session, WsSession, WsSessionUser};
use crate::AppState;

/// RAII guard for the `plombir_git_ws_connections` gauge: bumps it on
/// construction and decrements on drop, so every exit path of a socket loop
/// (normal close, welcome-send failure, lag/error break, task cancellation)
/// settles the gauge through a single construction site — the same pattern as
/// the git-transport `GitOpTimer`.
struct WsConnGuard;

impl WsConnGuard {
    fn new() -> Self {
        crate::metrics::recorder::ws_connected();
        Self
    }
}

impl Drop for WsConnGuard {
    fn drop(&mut self) {
        crate::metrics::recorder::ws_disconnected();
    }
}

/// The broadcast channel capacity for real-time notifications.
const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;

/// How often a socket that is already open re-asks whether it may still be open.
///
/// The revocation gate ([`crate::api::auth::session_standing_middleware`]) is a
/// *request* middleware, and a WebSocket makes exactly one request — the
/// handshake. After that there is nothing for the middleware to intercept, so
/// neither a deactivation nor a revoked session reaches an open socket through
/// any path at all: the offboarded — or logged-out — user's tab keeps receiving
/// pushes until they close it. This is that same gate, asking both of its
/// questions, sampled, which bounds the window by an interval instead of by how
/// long someone leaves a browser open. Cost is one primary-key read per socket per
/// interval — the sockets are idle between pushes, so this is what they do.
pub const DEFAULT_WS_SESSION_RECHECK_SECS: u64 = 30;

/// Does the session behind an open socket still stand?
///
/// Both halves of [`crate::api::auth::session_standing_middleware`]'s question
/// are asked, and for the same reason it asks both: an account can keep standing
/// while the *session* is revoked. A `POST /users/logout` and a password reset
/// leave `is_usable()` true and bump `users.session_version` instead, so a check
/// that reads only the first is blind to exactly the two acts a user performs to
/// end a session on purpose — and an open tab is not bounded by anything but how
/// long it stays open (card_7898025803a6).
///
/// Fails closed, deliberately: "disabled", "revoked" and "could not tell" all
/// end the socket, because the alternative is that a database hiccup becomes the
/// reason a revoked session keeps its stream. Closing is not a verdict the
/// client has to live with — it reconnects, and the handshake goes through the
/// same middleware every other request does, which answers `503` rather than
/// `401` when it is the database that is unwell.
async fn account_still_stands(db: &sea_orm::DatabaseConnection, session: WsSessionUser) -> bool {
    match rg_db::ops::user_ops::find_by_id(db, session.user_id).await {
        Ok(Some(user)) => user.is_usable() && user.session_version == session.session_version,
        Ok(None) => false,
        Err(error) => {
            tracing::error!(
                user_id = session.user_id,
                error = %format!("{error:#}"),
                "could not verify account standing for an open WebSocket"
            );
            false
        }
    }
}

/// May this user still read this repository's job logs?
///
/// Both halves are re-asked, not just the account: the handshake's answer came
/// from `check_read_for`, and that answer can stop being true without the
/// account going anywhere — the repository flips to private, a collaborator is
/// removed. The repository row is re-read for the same reason.
///
/// Account standing is finalized *after* that independent repository proof.
/// A snapshot before it is not a final verdict: retirement or physical deletion
/// can win while `check_read_for` is in flight, and the socket must not publish
/// a stale `true` afterwards.
async fn job_log_access_still_stands(
    state: &AppState,
    repo_id: i64,
    session: WsSessionUser,
) -> bool {
    let repository_still_stands = match rg_db::ops::repo_ops::find_by_id(&state.db, repo_id).await {
        Ok(Some(repository)) => {
            crate::api::repo_access::check_read_for(state, &repository, Some(session.user_id))
                .await
                .is_ok()
        }
        Ok(None) => false,
        Err(error) => {
            tracing::error!(
                repo_id,
                user_id = session.user_id,
                error = %format!("{error:#}"),
                "could not verify repository access for an open job-log WebSocket"
            );
            false
        }
    };
    if !repository_still_stands {
        return false;
    }

    match rg_db::ops::user_ops::finalize_standing_credential_owner(&state.db, session.user_id).await
    {
        Ok(Some(user)) => user.is_usable() && user.session_version == session.session_version,
        Ok(None) => false,
        Err(error) => {
            // An open socket has no HTTP response left on which to preserve a
            // 503. Close fail-closed, but keep the server failure distinct from
            // a revocation in the operator log.
            tracing::error!(
                repo_id,
                user_id = session.user_id,
                error = %format!("{error:#}"),
                "could not finalize account standing for an open job-log WebSocket"
            );
            false
        }
    }
}

/// The re-check cadence for one socket, floored at a second so a misconfigured
/// zero cannot turn the arm into a busy loop against the database.
fn session_recheck_interval(state: &AppState) -> tokio::time::Interval {
    let period = std::time::Duration::from_secs(state.ws_session_recheck_secs.max(1));
    // `interval_at` rather than `interval`: the first tick of the latter fires
    // immediately, and the handshake checked standing microseconds ago.
    tokio::time::interval_at(tokio::time::Instant::now() + period, period)
}

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
            if sender.send(event).is_err() {
                // The channel exists but currently has no receivers.
            }
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
            if sender.send(event).is_err() {
                // The channel exists but currently has no receivers.
            }
        }
    }
}

/// GET /api/v1/ws/notifications — WebSocket upgrade handler.
pub async fn ws_notifications_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // M-4/M-5: the cookie is the browser's only shape here, so the reading of
    // it belongs with the constant that names it — see `api::auth::ws_session`.
    let WsSession {
        protocol_echo,
        user,
    } = ws_session(&headers, &state.jwt_secret);

    let Some(session) = user else {
        return crate::error::AppError::unauthorized("authentication required").into_response();
    };

    let upgrade = if let Some(proto) = protocol_echo {
        ws.protocols([proto])
    } else {
        ws
    };

    upgrade
        .on_upgrade(move |socket| handle_ws_connection(socket, state, session))
        .into_response()
}

/// Handle an individual WebSocket connection.
async fn handle_ws_connection(socket: WebSocket, state: AppState, session: WsSessionUser) {
    let user_id = session.user_id;
    let hub = state.notification_hub.clone();
    let (mut sender, mut receiver) = socket.split();

    tracing::info!(user_id, "WebSocket client connected for notifications");
    let _ws_guard = WsConnGuard::new();

    // General notifications never receive job logs. Those are isolated on
    // the dedicated /ws/job/:job_id endpoint.
    let mut user_rx = hub.subscribe_user(user_id).await;

    // Send initial connection confirmation
    let welcome = serde_json::json!({
        "type": "connected",
        "user_id": user_id,
    });
    if sender
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        drop(user_rx);
        hub.cleanup_user_channel(user_id).await;
        return;
    }

    let mut recheck = session_recheck_interval(&state);

    loop {
        tokio::select! {
            _ = recheck.tick() => {
                if !account_still_stands(&state.db, session).await {
                    tracing::warn!(
                        user_id,
                        "closing a notification WebSocket: its session no longer stands"
                    );
                    if sender
                        .send(Message::Text(
                            serde_json::json!({"error": "session revoked"}).to_string().into(),
                        ))
                        .await
                        .is_err()
                    {
                        // Client is already gone; the close below is a formality.
                    }
                    if sender.close().await.is_err() {
                        // Client already disconnected.
                    }
                    break;
                }
            },
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
                    tracing::warn!(user_id, skipped, "notification WebSocket lagged");
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
    hub.cleanup_user_channel(user_id).await;
    tracing::info!(user_id, "WebSocket client disconnected");
}

/// Push a notification to the WebSocket hub for real-time delivery.
///
/// Spawns an async task to send to the user's channel without blocking
/// the caller. The notification is also persisted in the database, so
/// offline users will see it via the REST API on next fetch.
///
/// The task is routed through the shared delivery tracker rather than a bare
/// `tokio::spawn` so graceful shutdown can await the persisted notification row
/// instead of severing it mid-write on SIGTERM.
pub fn push_notification(
    hub: &NotificationHub,
    user_id: i64,
    event_type: &str,
    data: serde_json::Value,
) {
    let hub = hub.clone();
    let event_type = event_type.to_string();
    rg_core::task_tracker::delivery_tracker().spawn(async move {
        hub.push_notification(user_id, &event_type, data).await;
    });
}

/// The hub is the concrete sink behind `rg-core`'s transport-neutral
/// [`rg_core::push_hooks::PushNotifier`] seam: the post-push hooks live in
/// `rg-core` (so the SSH transport runs them too) and cannot name an HTTP type,
/// but they still have to reach the WebSocket clients this hub owns.
impl rg_core::push_hooks::PushNotifier for NotificationHub {
    fn notify(&self, user_id: i64, event_type: &str, data: serde_json::Value) {
        push_notification(self, user_id, event_type, data);
    }
}

/// Broadcast a job log update to subscribers of that job.
pub async fn push_job_log(hub: &NotificationHub, job_id: i64, log: &str) {
    hub.push_job_log(job_id, log).await;
}

/// The one answer for a job id the caller may not know the fate of: absent, or
/// living in a repository they cannot see.
///
/// `{job_id}` is an instance-wide primary key and this route's path names no
/// repository, so the two have to be the same reply — the rule
/// [`crate::api::repo_access::RepoAnchor::masked`] states for the REST routes of
/// exactly this shape, and the rule `api::boards` writes down in one line: "a
/// mismatch answers 404, not 403: a 403 would confirm the id exists".
///
/// It is a function rather than four literals because the masking is only worth
/// what the answers have in common: an outsider told `job not found` for one id
/// and `access denied` for another has learned precisely the thing the `404` was
/// there not to tell him — and so has one told `job not found` and `not found`.
fn job_not_found() -> crate::error::AppError {
    crate::error::AppError::not_found("job not found")
}

/// GET /api/v1/ws/job/:job_id — WebSocket for real-time job log streaming.
///
/// Authenticates via the HttpOnly cookie or a `Sec-WebSocket-Protocol:
/// bearer.<jwt>` subprotocol; a `?token=<jwt>` query parameter is not read.
/// Frontend subscribes to receive `job_log` events filtered by the specified job_id.
pub async fn ws_job_log_handler(
    ws: WebSocketUpgrade,
    Path(job_id): Path<i64>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // The same reading of a handshake as the notification socket above, from the
    // same place — see `api::auth::ws_session`.
    let WsSession {
        protocol_echo,
        user,
    } = ws_session(&headers, &state.jwt_secret);

    let Some(session) = user else {
        return crate::error::AppError::unauthorized("authentication required").into_response();
    };
    let user_id = session.user_id;

    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return job_not_found().into_response(),
        Err(error) => return crate::error::AppError::from(error).into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return job_not_found().into_response(),
        Err(error) => return crate::error::AppError::from(error).into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return job_not_found().into_response(),
        Err(error) => return crate::error::AppError::from(error).into_response(),
    };
    let repository = match rg_db::ops::repo_ops::find_by_id(&state.db, pipeline.repo_id).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return job_not_found().into_response(),
        Err(error) => return crate::error::AppError::from(error).into_response(),
    };
    // The handshake resolved *who* is calling out of the cookie or the
    // subprotocol, because a browser cannot set `Authorization` on a WebSocket. What
    // that user may read is the shared repository gate's decision, exactly as
    // it would be on the REST route serving the same logs.
    //
    // What the caller is *told* is not that decision, though, and this is where
    // it parts company with the REST route: there the path named the repository,
    // so a `403` teaches nothing the caller did not already supply, while here
    // the caller supplied an opaque integer and any answer other than the one an
    // absent id gets confirms the integer hit a row. So the refusal is folded
    // into [`job_not_found`] — the same collapse `masked_denial` performs for
    // `/artifacts/{id}`, and for the same reason.
    //
    // Only a *denial* is folded. A check that could not run stays what it was,
    // or a database outage would answer `404` and send the caller off looking
    // for a job that is very much there.
    if let Err(error) =
        crate::api::repo_access::check_read_for(&state, &repository, Some(user_id)).await
    {
        return if crate::api::repo_access::is_access_denial(&error) {
            job_not_found().into_response()
        } else {
            error.into_response()
        };
    }

    let upgrade = if let Some(proto) = protocol_echo {
        ws.protocols([proto])
    } else {
        ws
    };

    let repo_id = repository.id;
    upgrade
        .on_upgrade(move |socket| {
            handle_job_log_connection(socket, state, job_id, repo_id, session)
        })
        .into_response()
}

/// Handle a job log WebSocket connection.
async fn handle_job_log_connection(
    socket: WebSocket,
    state: AppState,
    job_id: i64,
    repo_id: i64,
    session: WsSessionUser,
) {
    let user_id = session.user_id;
    let hub = state.notification_hub.clone();
    let (mut sender, mut receiver) = socket.split();
    let _ws_guard = WsConnGuard::new();
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

    let mut recheck = session_recheck_interval(&state);

    loop {
        tokio::select! {
            _ = recheck.tick() => {
                if !job_log_access_still_stands(&state, repo_id, session).await {
                    tracing::warn!(
                        job_id,
                        repo_id,
                        user_id,
                        "closing a job-log WebSocket: the reader may no longer read this repository"
                    );
                    if sender
                        .send(Message::Text(
                            serde_json::json!({"error": "access revoked"}).to_string().into(),
                        ))
                        .await
                        .is_err()
                    {
                        // Client is already gone; the close below is a formality.
                    }
                    if sender.close().await.is_err() {
                        // Client already disconnected.
                    }
                    break;
                }
            },
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
