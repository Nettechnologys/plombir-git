//! Webhook service — register, trigger, and deliver webhooks.
//!
//! Webhooks allow external services to be notified when events occur in a
//! repository (push, issues, pull_request, etc.). This service handles:
//! - CRUD for webhook registrations
//! - Event dispatch (find matching webhooks and fire HTTP POST)
//! - Delivery recording (status, response, timing)
//!
//! # The signing secret
//!
//! `webhooks.secret_encrypted` holds the key every delivery's
//! `X-Hub-Signature-256` is computed with. The server has to read it back on
//! every dispatch, so it cannot be a digest the way a runner token is — it is
//! AES-256-GCM ciphertext under the instance's at-rest key, like
//! `ci_secrets.encrypted_value` and `mirrors.password_encrypted`, and it is
//! registered in [`crate::auth::encrypted_columns`] so the startup preflight
//! and `plombir-git rotate-encryption-key` cover it without being told twice.
//!
//! Writing it takes the key as a parameter ([`create_webhook`],
//! [`update_webhook`] — both are called straight from a handler that has it).
//! Reading it back cannot: delivery is detached into a background task from
//! every corner of the codebase that raises a repository event, so the
//! dispatcher takes the key from [`crate::auth::at_rest_key`], which the server
//! publishes at startup.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use rg_db::entities::webhook;
use rg_db::entities::webhook_delivery;
use rg_db::ops::webhook_ops;

use crate::auth::{at_rest_key, encryption};

// ── API types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateWebhookRequest {
    /// Where deliveries are POSTed. It is stored and shown as typed, so it may
    /// not carry a credential (`https://user:token@host/hook`) — see
    /// [`reject_url_credentials`].
    pub url: String,
    pub content_type: Option<String>, // "json" (default) or "form"
    pub secret: Option<String>,
    pub active: Option<bool>,
    /// Subscription list; every entry must be one of [`WEBHOOK_EVENTS`]
    /// (e.g. `["push", "issue.opened"]`).
    pub events: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateWebhookRequest {
    /// Replacement target. Same rule as on create: no credential in the URL.
    pub url: Option<String>,
    pub content_type: Option<String>,
    pub secret: Option<String>,
    pub active: Option<bool>,
    pub events: Option<Vec<String>>,
}

// ── The secret at rest ────────────────────────────────────────────────────

/// Seal an operator-supplied signing secret for storage.
///
/// An absent or empty secret is stored as `NULL` rather than as the ciphertext
/// of `""`: "no secret configured" is what both the API (`has_secret`) and the
/// dispatcher read out of the column, and encrypting emptiness would make the
/// row claim a secret that signs nothing.
fn seal_secret(secret: Option<&str>, encryption_key: &str) -> Result<Option<String>> {
    let Some(secret) = secret.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let key = encryption::derive_key(encryption_key);
    encryption::encrypt(secret, &key)
        .map(Some)
        .context("encrypt the webhook signing secret")
}

/// Open the stored secret for a delivery about to be signed.
///
/// Three cases, classified the way [`crate::auth::rekey`] classifies every
/// other at-rest column:
/// * no secret — the delivery goes out unsigned, which is what it always did;
/// * a value that is structurally not our ciphertext — a row written before
///   this column was encrypted, used as the plaintext it is (the startup pass
///   [`seal_legacy_secrets`] normally seals these before any dispatch runs);
/// * our ciphertext — opened with the published at-rest key.
///
/// The last case is the only one that can fail, and it fails *loudly*: an
/// unopenable secret returns an error, which the caller records as a delivery
/// error. Signing with the ciphertext, or dropping the signature and posting
/// anyway, would both hand the receiver a request it cannot tell from a forgery
/// — the one failure mode a webhook must not have.
fn secret_for_delivery(
    stored: Option<&str>,
    encryption_key: Option<&str>,
) -> Result<Option<String>> {
    let Some(stored) = stored.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !encryption::looks_like_ciphertext(stored) {
        tracing::warn!(
            "webhook signing secret is still stored in the clear; it will be sealed on the \
             next start"
        );
        return Ok(Some(stored.to_string()));
    }
    let encryption_key = encryption_key.ok_or_else(|| {
        anyhow!(
            "no at-rest encryption key was published to this process, so the webhook signing \
             secret cannot be opened"
        )
    })?;
    let key = encryption::derive_key(encryption_key);
    encryption::decrypt(stored, &key).map(Some).context(
        "open the webhook signing secret — the at-rest encryption key does not match the \
         stored value",
    )
}

/// Seal every webhook secret still stored in the clear.
///
/// The rename to `secret_encrypted` is a schema change and happens in the
/// migration runner, which has no access to the at-rest key; this is the other
/// half, run by `plombir-git serve` right after the key preflight — the first
/// point in the boot where the migrated schema and the key both exist. Returns
/// how many rows it sealed.
///
/// Idempotent: a value that already is our ciphertext is left alone, so a
/// restart costs one query and rewrites nothing.
pub async fn seal_legacy_secrets(db: &DatabaseConnection, encryption_key: &str) -> Result<usize> {
    use sea_orm::{ActiveModelTrait, Set};

    let mut pages = webhook::Entity::find()
        .filter(webhook::Column::SecretEncrypted.is_not_null())
        .order_by_asc(webhook::Column::Id)
        .paginate(db, 500);

    let key = encryption::derive_key(encryption_key);
    let mut sealed = 0_usize;

    while let Some(hooks) = pages
        .fetch_and_next()
        .await
        .context("read webhook signing secrets")?
    {
        for hook in hooks {
            let Some(stored) = hook.secret_encrypted.as_deref() else {
                continue;
            };
            if stored.is_empty() || encryption::looks_like_ciphertext(stored) {
                continue;
            }
            let ciphertext = encryption::encrypt(stored, &key)
                .context("encrypt a legacy webhook signing secret")?;
            let id = hook.id;
            let mut model: webhook::ActiveModel = hook.into();
            model.secret_encrypted = Set(Some(ciphertext));
            model
                .update(db)
                .await
                .with_context(|| format!("seal the signing secret of webhook {id}"))?;
            sealed += 1;
        }
    }

    if sealed > 0 {
        tracing::info!(count = sealed, "sealed webhook signing secrets at rest");
    }
    Ok(sealed)
}

/// Take the credential out of every webhook URL that still carries one.
///
/// [`reject_url_credentials`] refuses them on the way in, but a hook registered
/// before that check holds `https://user:token@host/hook` in a plaintext
/// column. Run at startup next to [`seal_legacy_secrets`]. Returns how many
/// rows it rewrote.
///
/// Lossy on purpose, and the only honest option available: there is no column
/// to move a webhook credential into, so a delivery that relied on it starts
/// answering `401` — visible on the hook's own delivery list — instead of the
/// secret staying readable in the database. The removal is logged per hook so
/// the operator can act on it.
///
/// A credential in the *query* (`?token=…`) is not touched: it is part of the
/// address the receiver routes on, indistinguishable from any other parameter.
///
/// Idempotent: a URL with no userinfo is left alone.
pub async fn strip_legacy_url_credentials(db: &DatabaseConnection) -> Result<usize> {
    use sea_orm::{ActiveModelTrait, Set};

    let mut pages = webhook::Entity::find()
        .filter(webhook::Column::Url.contains("@"))
        .order_by_asc(webhook::Column::Id)
        .paginate(db, 500);

    let mut stripped = 0_usize;
    while let Some(hooks) = pages
        .fetch_and_next()
        .await
        .context("read webhook target URLs")?
    {
        for hook in hooks {
            let target = crate::net::strip_url_credentials(&hook.url);
            if !target.is_present() {
                // An `@` in the path or query — nothing that authenticates.
                continue;
            }
            let id = hook.id;
            let mut model: webhook::ActiveModel = hook.into();
            model.url = Set(target.url);
            model
                .update(db)
                .await
                .with_context(|| format!("rewrite the target URL of webhook {id}"))?;
            tracing::warn!(
                webhook_id = id,
                "webhook {id} carried a credential in its target URL; it was removed — the \
                 receiver must authenticate deliveries by their signature instead"
            );
            stripped += 1;
        }
    }

    if stripped > 0 {
        tracing::info!(count = stripped, "removed credentials from webhook URLs");
    }
    Ok(stripped)
}

// ── The event vocabulary ──────────────────────────────────────────────────

/// Every event this server ever raises.
///
/// The list existed only as the set of string literals passed to
/// [`trigger_event`], so a subscription was never checked against anything:
/// `{"events":["pull_request.merge"]}` answered `201 Created`, the hook showed
/// up in the settings page looking configured, and it stayed silent forever
/// (card_55c9cddfe9d6). Nothing logged a thing — there is no failure here, only
/// a subscription to an event that does not exist.
///
/// Kept in sync with the trigger sites by
/// `every_event_this_server_raises_is_in_the_canon`, which reads the source
/// rather than trusting this comment, and with the checkbox list in
/// `web/src/routes/[owner]/[repo]/settings/webhooks/+page.svelte`.
pub const WEBHOOK_EVENTS: [&str; 14] = [
    "push",
    "branch.created",
    "branch.deleted",
    "tag.created",
    "tag.deleted",
    "release.created",
    "release.deleted",
    "issue.opened",
    "issue.closed",
    "issue.comment",
    "milestone.closed",
    "pull_request.opened",
    "pull_request.closed",
    "pull_request.merged",
];

/// Whether `event` is a name this server can ever deliver.
pub fn is_known_event(event: &str) -> bool {
    WEBHOOK_EVENTS.contains(&event)
}

/// Validate a subscription list and render it into the stored column.
///
/// Storage is one comma-joined string, which is also why an event name may not
/// contain a comma or be empty: the reader splits on `,`, so either would
/// produce a subscription entry that can never be matched — the same silent
/// dead end by a different route.
fn encode_subscriptions(events: &[String]) -> Result<String> {
    for event in events {
        if !is_known_event(event) {
            return Err(crate::error::invalid_request(format!(
                "unknown webhook event; this server delivers: {}",
                WEBHOOK_EVENTS.join(", ")
            )));
        }
    }
    Ok(events.join(","))
}

/// Whether a stored subscription list covers `event`.
///
/// Membership in the split list, not a substring of the joined one. The column
/// was matched with SQL `LIKE '%<event>%'`, which answered a question nobody
/// asked: a hook subscribed to `issue` received all three `issue.*` events
/// (documented nowhere), and the day an event name became a prefix of another —
/// `push` and `push.forced` — every `push` subscriber would start receiving
/// both. The `LIKE` stays as the *narrowing* filter in SQL; this is what
/// decides (card_55c9cddfe9d6).
pub fn subscription_covers(subscriptions: &str, event: &str) -> bool {
    subscriptions
        .split(',')
        .any(|subscribed| subscribed.trim() == event)
}

// ── CRUD ──────────────────────────────────────────────────────────────────

/// A webhook target may not carry a credential in its userinfo.
///
/// `webhooks.url` is a plaintext column and the hook's *own* secret is the only
/// thing this table encrypts, so `https://user:token@host/hook` would store a
/// usable credential in the clear — and hand it back out through every
/// `GET /hooks` response and every recorded delivery error. Unlike a mirror
/// there is nowhere to move it to: a webhook authenticates by signing its
/// payload, not by logging in. So the URL is refused, with the alternative
/// named.
fn reject_url_credentials(url: &str) -> Result<()> {
    let target = crate::net::split_url_credentials(url).context("invalid webhook URL")?;
    if target.is_present() {
        return Err(crate::error::invalid_request(
            "a webhook URL may not carry a credential in it (`https://user:token@host/hook`): \
             the URL is stored and shown as typed, so the credential would sit in the database \
             in the clear. Authenticate the delivery with the hook's signing secret \
             (`X-Hub-Signature-256`) instead.",
        ));
    }
    Ok(())
}

/// Register a webhook under the instance's explicit transport policy.
pub async fn create_webhook(
    db: &DatabaseConnection,
    repo_id: i64,
    req: &CreateWebhookRequest,
    encryption_key: &str,
    transport_policy: crate::webhook::transport::WebhookTransportPolicy,
) -> Result<webhook::Model> {
    // Reject obviously-internal targets (bad scheme / private IP literal) at
    // registration for immediate feedback, and require an operator exception
    // before plaintext HTTP can carry payloads/signatures. Delivery re-checks
    // both policy and DNS so legacy rows cannot bypass a safer new default.
    transport_policy.validate_url_static(&req.url)?;
    reject_url_credentials(&req.url)?;
    let now = Utc::now();
    let events_str = encode_subscriptions(&req.events)?;
    let model = webhook::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo_id),
        url: sea_orm::Set(req.url.clone()),
        content_type: sea_orm::Set(
            req.content_type
                .clone()
                .unwrap_or_else(|| "json".to_string()),
        ),
        secret_encrypted: sea_orm::Set(seal_secret(req.secret.as_deref(), encryption_key)?),
        active: sea_orm::Set(req.active.unwrap_or(true)),
        events: sea_orm::Set(events_str),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
    };
    webhook_ops::create_webhook(db, model).await
}

/// List all webhooks for a repository.
pub async fn list_webhooks(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<webhook::Model>> {
    webhook_ops::list_by_repo(db, repo_id).await
}

/// Get a webhook by id.
pub async fn get_webhook(db: &DatabaseConnection, id: i64) -> Result<Option<webhook::Model>> {
    webhook_ops::find_by_id(db, id).await
}

/// Update a webhook.
///
/// The secret follows the same write-only rule the API does: `None` keeps the
/// stored ciphertext (the caller cannot read it back to resend it), a value
/// replaces it, and an empty string clears it.
pub async fn update_webhook(
    db: &DatabaseConnection,
    existing: &webhook::Model,
    req: &UpdateWebhookRequest,
    encryption_key: &str,
    transport_policy: crate::webhook::transport::WebhookTransportPolicy,
) -> Result<webhook::Model> {
    update_webhook_after_read(db, existing, req, encryption_key, transport_policy, || {
        std::future::ready(Ok(()))
    })
    .await
}

/// Testable boundary between the handler's scoped read and conditional write.
async fn update_webhook_after_read<F, Fut>(
    db: &DatabaseConnection,
    existing: &webhook::Model,
    req: &UpdateWebhookRequest,
    encryption_key: &str,
    transport_policy: crate::webhook::transport::WebhookTransportPolicy,
    after_read: F,
) -> Result<webhook::Model>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Some(url) = req.url.as_deref() {
        transport_policy.validate_url_static(url)?;
        reject_url_credentials(url)?;
    }
    let secret_encrypted = match req.secret.as_deref() {
        Some(secret) => seal_secret(Some(secret), encryption_key)?,
        None => existing.secret_encrypted.clone(),
    };
    let events = match req.events.as_deref() {
        Some(events) => encode_subscriptions(events)?,
        None => existing.events.clone(),
    };
    after_read().await?;
    webhook_ops::update_webhook(
        db,
        existing.id,
        req.url.clone().unwrap_or_else(|| existing.url.clone()),
        req.content_type
            .clone()
            .unwrap_or_else(|| existing.content_type.clone()),
        secret_encrypted,
        req.active.unwrap_or(existing.active),
        events,
        Utc::now(),
    )
    .await?
    .ok_or_else(|| crate::error::not_found("webhook"))
}

/// Delete a webhook. `false` means the row was already gone — see
/// [`webhook_ops::delete_webhook_by_id`].
pub async fn delete_webhook(db: &DatabaseConnection, id: i64) -> Result<bool> {
    webhook_ops::delete_webhook_by_id(db, id).await
}

// ── Event dispatch ────────────────────────────────────────────────────────

/// Trigger a webhook event: find matching webhooks and deliver payloads.
pub async fn trigger_event(
    db: &DatabaseConnection,
    repo_id: i64,
    event: &str,
    payload: &Value,
) -> Result<()> {
    trigger_event_with_tracker(
        db,
        repo_id,
        event,
        payload,
        crate::task_tracker::delivery_tracker(),
    )
    .await
}

pub(crate) async fn trigger_event_with_tracker(
    db: &DatabaseConnection,
    repo_id: i64,
    event: &str,
    payload: &Value,
    delivery_tracker: &crate::task_tracker::TaskTracker,
) -> Result<()> {
    let hooks = webhook_ops::list_active_by_repo_and_event(db, repo_id, event).await?;
    let payload = serde_json::to_string(payload).context("serialize webhook payload")?;

    // The query narrowed by substring; membership is decided here.
    for hook in hooks
        .into_iter()
        .filter(|hook| subscription_covers(&hook.events, event))
    {
        spawn_delivery(
            db,
            hook,
            event.to_string(),
            payload.clone(),
            delivery_tracker,
        );
    }

    Ok(())
}

/// Spawn one concrete hook delivery.
///
/// Event fan-out calls this once per matching hook. Redelivery deliberately
/// calls it once for the hook named by the recorded delivery: routing the
/// replay back through [`trigger_event`] would send it to every current
/// subscriber and would skip the original hook when it is inactive.
fn spawn_delivery(
    db: &DatabaseConnection,
    hook: webhook::Model,
    event: String,
    payload: String,
    delivery_tracker: &crate::task_tracker::TaskTracker,
) {
    // Spawn delivery in background — don't block the caller. Routed through
    // the shared delivery tracker (not a bare `tokio::spawn`) so graceful
    // shutdown can await the outbound POST + `webhook_delivery` row write
    // instead of severing it mid-flight on SIGTERM.
    let db = db.clone();
    let hook_id = hook.id;
    let url = hook.url;
    let content_type = hook.content_type;
    // Still sealed here. It is opened inside `deliver`, so a secret this
    // instance cannot read surfaces as a recorded delivery error on the hook's
    // own delivery list instead of a hook that quietly stops firing.
    let secret_encrypted = hook.secret_encrypted;

    delivery_tracker.spawn(async move {
        let delivery_id = uuid::Uuid::new_v4().to_string();
        let start = std::time::Instant::now();

        let (status, response_body) = match deliver(
            &url,
            &content_type,
            secret_encrypted.as_deref(),
            &payload,
        )
        .await
        {
            Ok(resp_status) => (Some(resp_status), None::<String>),
            Err(e) => {
                // The reason is persisted on the delivery and shown in the
                // hook's own delivery list, and `reqwest` quotes the URL it
                // failed on. A hook registered before the userinfo check
                // existed still carries a credential there.
                let reason = crate::net::mask_url_credentials(&format!("{e:#}"));
                tracing::warn!(webhook_id = hook_id, error = %reason, "webhook delivery failed");
                (None, Some(format!("delivery error: {reason}")))
            }
        };

        let duration_ms = start.elapsed().as_millis() as i64;

        // Meter the delivery outcome (2xx = success) for the
        // `plombir_git_webhook_deliveries_total` counter. Forwarded through the
        // HTTP-layer observer since the Prometheus recorder lives above us.
        let succeeded = matches!(status, Some(s) if (200..300).contains(&s));
        crate::metrics_hook::record_webhook_delivery(succeeded);

        let delivery_model = webhook_delivery::ActiveModel {
            id: sea_orm::NotSet,
            webhook_id: sea_orm::Set(hook_id),
            event: sea_orm::Set(event),
            delivery_id: sea_orm::Set(delivery_id),
            response_status: sea_orm::Set(status),
            request_payload: sea_orm::Set(Some(payload)),
            response_body: sea_orm::Set(response_body),
            duration_ms: sea_orm::Set(Some(duration_ms)),
            created_at: sea_orm::Set(Utc::now()),
        };

        if let Err(e) = webhook_ops::create_delivery(&db, delivery_model).await {
            tracing::error!(error = %format!("{e:#}"), "failed to record webhook delivery");
        }
    });
}

/// Deliver a webhook payload via HTTP POST.
///
/// `secret_encrypted` is the column as stored. Opening it is the first thing
/// that happens: a hook whose signature cannot be computed must not be posted
/// unsigned, so the failure has to land before the request is built.
async fn deliver(
    url: &str,
    content_type: &str,
    secret_encrypted: Option<&str>,
    payload: &str,
) -> Result<i32> {
    let secret = secret_for_delivery(secret_encrypted, at_rest_key::resolve())?;

    // The policy returns only the webhook-owned client. IP literals are checked
    // here; domain answers are checked inside reqwest's resolver and those exact
    // addresses go to the connector, closing the DNS-rebinding check/use gap.
    // Redirects and proxies are disabled at the same boundary.
    let client = crate::webhook::transport::current()
        .client_for_url(url)
        .context("webhook target rejected by transport/SSRF policy")?;
    let mut builder = client.post(url);

    if content_type == "form" {
        builder = builder
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(payload.to_string());
    } else {
        builder = builder
            .header("Content-Type", "application/json")
            .body(payload.to_string());
    }

    // Sign with HMAC-SHA256 if secret is configured
    if let Some(secret) = secret.as_deref() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).context("HMAC init failed")?;
        mac.update(payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        builder = builder.header("X-Hub-Signature-256", format!("sha256={}", sig));
    }

    let resp = builder.send().await.context("webhook POST failed")?;
    Ok(resp.status().as_u16() as i32)
}

/// List recent deliveries for a webhook.
pub async fn list_deliveries(
    db: &DatabaseConnection,
    webhook_id: i64,
) -> Result<Vec<webhook_delivery::Model>> {
    webhook_ops::list_deliveries_by_webhook(db, webhook_id).await
}

/// Get a delivery by id.
pub async fn get_delivery(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<webhook_delivery::Model>> {
    webhook_ops::find_delivery_by_id(db, id).await
}

/// Redeliver a webhook (re-post the original payload).
pub async fn redeliver(db: &DatabaseConnection, delivery_id: i64) -> Result<()> {
    redeliver_with_tracker(db, delivery_id, crate::task_tracker::delivery_tracker()).await
}

async fn redeliver_with_tracker(
    db: &DatabaseConnection,
    delivery_id: i64,
    delivery_tracker: &crate::task_tracker::TaskTracker,
) -> Result<()> {
    let delivery = webhook_ops::find_delivery_by_id(db, delivery_id)
        .await?
        .ok_or_else(|| crate::error::not_found("webhook delivery"))?;

    let hook = webhook_ops::find_by_id(db, delivery.webhook_id)
        .await?
        .ok_or_else(|| crate::error::not_found("webhook"))?;

    // Redelivery is "send the recorded request again", so a payload we cannot
    // reproduce has to stop the operation. Both fallbacks here used to produce
    // a *different* request instead: a delivery row with no recorded payload
    // became `""`, an unreadable one stayed a string that would not parse, and
    // either way `unwrap_or(Value::Null)` posted a body of `null` to the
    // receiver — under a `200 redelivery triggered`. That is the one failure
    // mode a webhook must not have: not "did not arrive", but "arrived wrong".
    let payload = delivery.request_payload.ok_or_else(|| {
        crate::error::conflict(format!(
            "webhook delivery {delivery_id} has no recorded request payload to resend"
        ))
    })?;
    serde_json::from_str::<Value>(&payload).with_context(|| {
        format!("stored request_payload of webhook delivery {delivery_id} is not valid JSON")
    })?;

    // Replay the exact recorded request body to the exact recorded hook. This
    // is an explicit operator action, so it does not inherit the automatic
    // fan-out's `active` filter; making a hook inactive must not turn a 200
    // redelivery response into a silent no-op.
    spawn_delivery(db, hook, delivery.event, payload, delivery_tracker);
    Ok(())
}

// ── Convenience event helpers ───────────────────────────────────────────

/// Trigger a release.created webhook event.
pub async fn trigger_release_created(
    db: &DatabaseConnection,
    repo_id: i64,
    release: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "release.created",
        "release": release,
    });
    trigger_event(db, repo_id, "release.created", &payload).await
}

/// Trigger a release.deleted webhook event.
pub async fn trigger_release_deleted(
    db: &DatabaseConnection,
    repo_id: i64,
    release: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "release.deleted",
        "release": release,
    });
    trigger_event(db, repo_id, "release.deleted", &payload).await
}

/// Trigger an issue.opened webhook event.
pub async fn trigger_issue_opened(
    db: &DatabaseConnection,
    repo_id: i64,
    issue: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "issue.opened",
        "issue": issue,
    });
    trigger_event(db, repo_id, "issue.opened", &payload).await
}

/// Trigger an issue.closed webhook event.
pub async fn trigger_issue_closed(
    db: &DatabaseConnection,
    repo_id: i64,
    issue: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "issue.closed",
        "issue": issue,
    });
    trigger_event(db, repo_id, "issue.closed", &payload).await
}

/// Trigger an issue.comment webhook event.
pub async fn trigger_issue_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    issue: &Value,
    comment: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "issue.comment",
        "issue": issue,
        "comment": comment,
    });
    trigger_event(db, repo_id, "issue.comment", &payload).await
}

/// Trigger a pull_request.opened webhook event.
pub async fn trigger_pr_opened(db: &DatabaseConnection, repo_id: i64, pr: &Value) -> Result<()> {
    let payload = serde_json::json!({
        "event": "pull_request.opened",
        "pull_request": pr,
    });
    trigger_event(db, repo_id, "pull_request.opened", &payload).await
}

/// Trigger a pull_request.closed webhook event.
pub async fn trigger_pr_closed(db: &DatabaseConnection, repo_id: i64, pr: &Value) -> Result<()> {
    let payload = serde_json::json!({
        "event": "pull_request.closed",
        "pull_request": pr,
    });
    trigger_event(db, repo_id, "pull_request.closed", &payload).await
}

/// Trigger a pull_request.merged webhook event.
pub async fn trigger_pr_merged(db: &DatabaseConnection, repo_id: i64, pr: &Value) -> Result<()> {
    let payload = serde_json::json!({
        "event": "pull_request.merged",
        "pull_request": pr,
    });
    trigger_event(db, repo_id, "pull_request.merged", &payload).await
}

/// Trigger a milestone.closed webhook event.
pub async fn trigger_milestone_closed(
    db: &DatabaseConnection,
    repo_id: i64,
    milestone: &Value,
) -> Result<()> {
    let payload = serde_json::json!({
        "event": "milestone.closed",
        "milestone": milestone,
    });
    trigger_event(db, repo_id, "milestone.closed", &payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database, Set};

    const KEY: &str = "the-instance-at-rest-key";
    const SECRET: &str = "s3cr3t-the-receiver-also-knows";

    async fn update_fixture() -> (DatabaseConnection, webhook::Model) {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.expect("connect test db");
        rg_db::run_migrations(&db).await.expect("migrate test db");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            "webhook-race-owner",
            "webhook-race-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(user.id),
                name: Set("webhook-race-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
                ..Default::default()
            },
        )
        .await
        .expect("create repository");
        let hook = webhook_ops::create_webhook(
            &db,
            webhook::ActiveModel {
                repo_id: Set(repo.id),
                url: Set("https://example.com/original".to_string()),
                content_type: Set("json".to_string()),
                secret_encrypted: Set(None),
                active: Set(true),
                events: Set("push".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create webhook");

        (db, hook)
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_typed_not_found() {
        let (db, hook) = update_fixture().await;
        let hook_id = hook.id;

        let error = update_webhook_after_read(
            &db,
            &hook,
            &UpdateWebhookRequest {
                url: Some("https://example.com/updated".to_string()),
                content_type: Some("form".to_string()),
                secret: Some("replacement".to_string()),
                active: Some(false),
                events: Some(vec!["release.created".to_string()]),
            },
            KEY,
            crate::webhook::transport::WebhookTransportPolicy::default(),
            || async {
                assert!(webhook_ops::delete_webhook_by_id(&db, hook_id)
                    .await
                    .expect("the competing webhook delete succeeds"));
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful webhook update");

        let typed = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the lost race must stay classifiable as HTTP 404");
        assert_eq!(typed.resource, "webhook");
    }

    /// The defect itself: what goes into the column must not be what the
    /// operator typed, and what comes back out must be exactly that.
    #[test]
    fn a_secret_is_sealed_on_the_way_in_and_opened_on_the_way_out() {
        let stored = seal_secret(Some(SECRET), KEY)
            .expect("seal")
            .expect("a secret was supplied");

        assert_ne!(stored, SECRET, "the operator's secret is in the column");
        assert!(encryption::looks_like_ciphertext(&stored));

        let opened = secret_for_delivery(Some(&stored), Some(KEY)).expect("open");
        assert_eq!(opened.as_deref(), Some(SECRET));
    }

    /// Encrypting emptiness would leave a row claiming a secret that signs
    /// nothing — `has_secret` and the dispatcher both read the column as
    /// "configured or not".
    #[test]
    fn an_absent_or_empty_secret_stays_null() {
        assert_eq!(seal_secret(None, KEY).expect("seal"), None);
        assert_eq!(seal_secret(Some(""), KEY).expect("seal"), None);
        assert_eq!(secret_for_delivery(None, Some(KEY)).expect("open"), None);
        assert_eq!(
            secret_for_delivery(Some(""), Some(KEY)).expect("open"),
            None
        );
    }

    /// A row written before the column was encrypted keeps signing until the
    /// startup pass seals it. Reading it as "ciphertext that failed to open"
    /// would take every one of those hooks off the air on upgrade.
    #[test]
    fn a_legacy_plaintext_secret_still_signs() {
        let opened = secret_for_delivery(Some(SECRET), Some(KEY)).expect("open");
        assert_eq!(opened.as_deref(), Some(SECRET));
    }

    /// The one case that must fail rather than improvise: signing with the
    /// ciphertext, or posting unsigned, hands the receiver a request it cannot
    /// tell from a forged one.
    #[test]
    fn a_secret_that_cannot_be_opened_is_an_error_not_a_guess() {
        let stored = seal_secret(Some(SECRET), KEY).expect("seal").expect("some");

        let wrong_key = secret_for_delivery(Some(&stored), Some("a-different-key"));
        assert!(wrong_key.is_err(), "signed with a mis-keyed secret");

        let no_key = secret_for_delivery(Some(&stored), None);
        assert!(no_key.is_err(), "signed without an at-rest key at all");
    }

    /// A redelivery names one persisted delivery, not a repository event. The
    /// old implementation fed that event back through `trigger_event`, which
    /// skipped an inactive original hook and delivered to every other active
    /// subscriber instead.
    #[tokio::test]
    async fn redelivery_targets_only_the_recorded_hook_even_when_it_is_inactive() {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.expect("connect test db");
        rg_db::run_migrations(&db).await.expect("migrate test db");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            "redelivery-owner",
            "redelivery-owner@example.invalid",
            "",
            "Redelivery owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(user.id),
                name: Set("replay".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
                ..Default::default()
            },
        )
        .await
        .expect("create repository");

        let original_hook = webhook_ops::create_webhook(
            &db,
            webhook::ActiveModel {
                repo_id: Set(repo.id),
                url: Set("http://127.0.0.1:1/original".to_string()),
                content_type: Set("json".to_string()),
                secret_encrypted: Set(None),
                active: Set(false),
                events: Set("push".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create inactive original hook");
        let sibling_hook = webhook_ops::create_webhook(
            &db,
            webhook::ActiveModel {
                repo_id: Set(repo.id),
                url: Set("http://127.0.0.1:2/sibling".to_string()),
                content_type: Set("json".to_string()),
                secret_encrypted: Set(None),
                active: Set(true),
                events: Set("push".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create active sibling hook");

        let recorded_payload = "{\"ref\": \"refs/heads/main\", \"forced\": false}";
        let recorded = webhook_ops::create_delivery(
            &db,
            webhook_delivery::ActiveModel {
                webhook_id: Set(original_hook.id),
                event: Set("push".to_string()),
                delivery_id: Set("11111111-1111-1111-1111-111111111111".to_string()),
                response_status: Set(Some(503)),
                request_payload: Set(Some(recorded_payload.to_string())),
                response_body: Set(Some("upstream unavailable".to_string())),
                duration_ms: Set(Some(7)),
                created_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("record original delivery");

        let tracker = crate::task_tracker::TaskTracker::new();
        redeliver_with_tracker(&db, recorded.id, &tracker)
            .await
            .expect("accept explicit redelivery");
        tracker.close();
        tracker.wait().await;

        let original_deliveries = webhook_ops::list_deliveries_by_webhook(&db, original_hook.id)
            .await
            .expect("list original-hook deliveries");
        let sibling_deliveries = webhook_ops::list_deliveries_by_webhook(&db, sibling_hook.id)
            .await
            .expect("list sibling-hook deliveries");

        assert_eq!(
            original_deliveries.len(),
            2,
            "the explicit retry must reach its inactive original hook"
        );
        assert!(
            sibling_deliveries.is_empty(),
            "the retry must not become a fresh repository-wide fan-out"
        );
        let replay = original_deliveries
            .iter()
            .find(|delivery| delivery.id != recorded.id)
            .expect("one new delivery was recorded");
        assert_eq!(
            replay.request_payload.as_deref(),
            Some(recorded_payload),
            "replay must retain the exact recorded request body"
        );
        assert!(
            replay
                .response_body
                .as_deref()
                .is_some_and(|body| body.starts_with("delivery error:")),
            "the deterministic SSRF refusal remains diagnostic"
        );
    }
}

/// The subscription vocabulary: what may be registered, and what a registration
/// then matches (card_55c9cddfe9d6).
#[cfg(test)]
mod event_vocabulary_tests {
    use super::*;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The two functions that raise a webhook event, and the argument position
    /// the event name occupies in both: `trigger_event_with_tracker` only adds
    /// a tracker after the payload, so the name stays the third argument.
    const TRIGGER_FUNCTIONS: [&str; 2] = ["trigger_event", "trigger_event_with_tracker"];
    const EVENT_ARGUMENT: usize = 2;

    /// The events `source` raises, as `file:line` and event name.
    ///
    /// Calls are located on identifier boundaries in the byte-aligned
    /// production code-only view, and the name is read from the *event*
    /// argument specifically — not from whatever is quoted nearby. So a comment
    /// shaped like a call, a normal/raw/byte literal spelling one out, a
    /// `#[cfg(test)]` item raising a test-only name, and a string sitting in an
    /// earlier parameter slot all contribute nothing.
    ///
    /// Still deliberately literal-only: a call that passes a variable is
    /// invisible here, which is the honest limit of reading source. Every
    /// trigger site in this crate spells its event out, and the count assertion
    /// in [`every_event_this_server_raises_is_in_the_canon`] is what notices if
    /// that stops being true.
    fn raised_events(name: &str, source: &str) -> Vec<(String, String)> {
        rust_source::production_call_sites(source, &TRIGGER_FUNCTIONS)
            .into_iter()
            .filter_map(|call| {
                let event = rust_source::nth_string_argument(source, call, EVENT_ARGUMENT)?;
                Some((format!("{name}:{}", call.line), event))
            })
            .collect()
    }

    fn assert_raised_events_are_canonical(raised: &[(String, String)]) {
        let unknown: Vec<_> = raised
            .iter()
            .filter(|(_, event)| !is_known_event(event))
            .collect();
        assert!(
            unknown.is_empty(),
            "these events are raised but cannot be subscribed to: {unknown:?}"
        );
    }

    #[test]
    fn a_subscription_to_an_event_that_does_not_exist_is_refused() {
        let typo = ["pull_request.merge".to_string()];
        let error = encode_subscriptions(&typo).expect_err("a hook that can never fire");
        let message = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "a subscription nobody can deliver is the caller's to fix: {message}"
        );
        assert!(
            message.contains("pull_request.merged"),
            "the refusal has to name what is on offer: {message}"
        );

        // Not the caller's fault, and not refused: an empty list is a hook that
        // is registered and subscribed to nothing, which the API has always
        // allowed.
        assert_eq!(encode_subscriptions(&[]).expect("empty list"), "");
    }

    #[test]
    fn every_offered_event_registers() {
        let all: Vec<String> = WEBHOOK_EVENTS.iter().map(|e| e.to_string()).collect();
        let encoded = encode_subscriptions(&all).expect("the canon must register");
        for event in WEBHOOK_EVENTS {
            assert!(subscription_covers(&encoded, event));
        }
    }

    /// The stored column is one joined string, and it used to be matched with
    /// `LIKE '%<event>%'`. That made a prefix a subscription: `issue` received
    /// all three `issue.*` events, and the first pair of names where one
    /// contains the other would start cross-firing.
    #[test]
    fn a_prefix_of_an_event_name_is_not_a_subscription_to_it() {
        assert!(!subscription_covers("issue", "issue.opened"));
        assert!(!subscription_covers("push", "push.forced"));
        assert!(!subscription_covers("push.forced", "push"));
        assert!(subscription_covers("push,issue.opened", "issue.opened"));
        assert!(subscription_covers("push", "push"));
        assert!(!subscription_covers("", "push"));
    }

    /// The canon is a list in one file and the events are raised from another,
    /// so "keep them in sync" is a promise no comment can keep. This reads the
    /// source: every literal handed to a trigger function has to be a name a
    /// hook is allowed to subscribe to, or the event exists and nobody can ask
    /// for it — which is the same dead subscription seen from the other end.
    #[test]
    fn every_event_this_server_raises_is_in_the_canon() {
        let crate_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut raised: Vec<(String, String)> = Vec::new();
        let mut files = vec![crate_src];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(
                    std::fs::read_dir(&path)
                        .expect("read source directory")
                        .map(|entry| entry.expect("read source entry").path()),
                );
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read source file");
            raised.extend(raised_events(&path.display().to_string(), &source));
        }

        assert!(
            raised.len() >= WEBHOOK_EVENTS.len(),
            "the scan found only {} trigger sites — it stopped seeing the calls it is meant to \
             read, so it can no longer catch anything: {raised:?}",
            raised.len()
        );
        assert_raised_events_are_canonical(&raised);
    }

    /// The other end of the same drift: the settings page renders a checkbox per
    /// event, and a checkbox for a name the server never raises is a
    /// subscription that can only ever be silent — which is the defect this card
    /// is about, arrived at from the UI instead of from the API.
    #[test]
    fn the_settings_page_offers_exactly_the_events_that_exist() {
        let page = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/routes/[owner]/[repo]/settings/webhooks/+page.svelte");
        let source = std::fs::read_to_string(&page)
            .unwrap_or_else(|error| panic!("read {}: {error}", page.display()));
        let (_, rest) = source
            .split_once("const eventOptions = [")
            .expect("the settings page still declares its event list as `const eventOptions`");
        let (list, _) = rest
            .split_once(']')
            .expect("unterminated eventOptions list");

        let offered: Vec<String> = list
            .split(',')
            .filter_map(|entry| {
                let entry = entry.trim().trim_matches('\'').trim_matches('"').trim();
                (!entry.is_empty()).then(|| entry.to_string())
            })
            .collect();

        let mut expected: Vec<String> = WEBHOOK_EVENTS.iter().map(|e| e.to_string()).collect();
        let mut got = offered.clone();
        expected.sort();
        got.sort();
        assert_eq!(
            got, expected,
            "the settings page and the server disagree about which events exist"
        );
    }

    /// What the census reads, and what it must refuse to read.
    ///
    /// The scan it replaced worked on raw lines: any line containing
    /// `trigger_event(` was a call, and the first quoted fragment within the
    /// next six lines was the event. That made a comment, a normal/raw/byte
    /// string literal, or a name quoted in an *earlier* argument enough to
    /// invent a trigger site — and enough to hold the count floor up after the
    /// last real producer was deleted.
    #[test]
    fn the_event_census_reads_call_arguments_rather_than_nearby_quotes() {
        const SAMPLE: &str = r####"
// trigger_event(db, repo_id, "line_comment", &payload);
/* trigger_event(db, repo_id, "block_comment", &payload); */
let normal = "trigger_event(db, repo_id, \"normal_string\", &payload)";
let raw = r#"trigger_event_with_tracker(db, repo_id, "raw_string", &payload, t)"#;
let bytes = b"trigger_event(db, repo_id, \"byte_string\", &payload)";

async fn trigger_event(db: &Db, repo_id: i64, event: &str, payload: &Value) {}

let first = trigger_event(db, repo_id, "push", &payload).await;

trigger_event_with_tracker(
    db,
    repo_id,
    /* the event may sit on a line of its own */ "branch.created",
    &payload,
    tracker,
);

let third = trigger_event(db, json!({"event": "not_the_argument"}), "tag.created", &payload);
let fourth = trigger_event(db, lookup("repo, id"), "milestone.closed", &payload);

#[cfg(test)]
mod early_tests {
    const BRACE_DECOY: &str = "}";
    fn raise() {
        trigger_event(db, repo_id, "early_test", &payload);
    }
}

let generic = trigger_event::<Value>(db, repo_id, r#"issue.opened"#, &payload);

#[cfg(test)]
mod tail_tests {
    fn raise() {
        trigger_event(db, repo_id, "test_tail", &payload);
    }
}
"####;
        let line_of = |needle: &str| {
            SAMPLE
                .lines()
                .position(|line| line.contains(needle))
                .map(|line| line + 1)
                .unwrap_or_else(|| panic!("sample has no line containing `{needle}`"))
        };
        // The multiline call has to be anchored on a line that *opens* with the
        // name: a decoy literal above it spells the same call out inside a raw
        // string, and picking that line up would be the fixture agreeing with
        // the bug it exists to catch.
        let line_opening_with = |needle: &str| {
            SAMPLE
                .lines()
                .position(|line| line.trim_start().starts_with(needle))
                .map(|line| line + 1)
                .unwrap_or_else(|| panic!("sample has no line opening with `{needle}`"))
        };

        assert_eq!(
            raised_events("fixture.rs", SAMPLE),
            vec![
                (
                    format!("fixture.rs:{}", line_of("let first")),
                    "push".into()
                ),
                (
                    format!(
                        "fixture.rs:{}",
                        line_opening_with("trigger_event_with_tracker(")
                    ),
                    "branch.created".into(),
                ),
                (
                    format!("fixture.rs:{}", line_of("let third")),
                    "tag.created".into(),
                ),
                (
                    format!("fixture.rs:{}", line_of("let fourth")),
                    "milestone.closed".into(),
                ),
                (
                    format!("fixture.rs:{}", line_of("let generic")),
                    "issue.opened".into(),
                ),
            ],
            "the census must read the event argument of real calls and nothing else"
        );
    }

    #[test]
    #[should_panic(expected = "invented.event")]
    fn an_event_outside_the_canon_fails_the_census() {
        let raised = raised_events(
            "fixture.rs",
            "fn raise() { trigger_event(db, repo_id, \"invented.event\", &payload); }",
        );
        assert_raised_events_are_canonical(&raised);
    }

    /// The floor has to bite on the real file, not only on a fixture: deleting
    /// the last producer of an event must lose it from the census even when
    /// call-shaped literals and a test-only trigger are there to rescue it.
    #[test]
    fn removing_the_last_real_producer_loses_the_event_despite_decoys() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/webhook/service.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let event = "release.created";
        let live_call = "trigger_event(db, repo_id, \"release.created\"";

        assert!(
            raised_events("service.rs", &source)
                .iter()
                .any(|(_, raised)| raised == event),
            "the production fixture no longer raises the event this mutation removes"
        );

        let mut mutated = source.replacen(live_call, "retired_trigger(db, repo_id, \"x\"", 1);
        assert_ne!(mutated, source, "the production mutation changed nothing");
        mutated.push_str(
            r####"
// trigger_event(db, repo_id, "release.created", &payload);
const EVENT_DECOY: &str = "release.created";
let normal = "trigger_event(db, repo_id, \"release.created\", &payload)";
let raw = r#"trigger_event_with_tracker(db, repo_id, "release.created", &p, t)"#;
let bytes = b"trigger_event(db, repo_id, \"release.created\", &payload)";

#[cfg(test)]
mod decoy_tests {
    fn raise() {
        trigger_event(db, repo_id, "release.created", &payload);
    }
}
"####,
        );

        assert!(
            !raised_events("service.rs", &mutated)
                .iter()
                .any(|(_, raised)| raised == event),
            "a comment, a call-shaped literal or a test-only trigger rescued the removed producer"
        );
    }
}
