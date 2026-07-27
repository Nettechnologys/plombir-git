//! Post-push hooks — the work that must happen after a `receive-pack` has
//! stored the pack, regardless of which transport carried the push.
//!
//! This lives in `rg-core` rather than next to one transport on purpose. It
//! used to be private to `rg-http`'s Smart-HTTP handler, so a push over SSH
//! silently ran *none* of it: no CI pipeline, no webhooks, no open-PR head-SHA
//! refresh, no auto-merge / merge-queue evaluation. Half the users (SSH is the
//! default once a key is registered) lived without automation and nothing in
//! the logs said so. Both transports now call [`post_push_hooks`].
//!
//! The one piece that cannot move down here is the real-time WebSocket fan-out:
//! the hub is an `rg-http` type, and `rg-core` must not depend on the HTTP
//! layer. [`PushNotifier`] is the seam — `rg_http::ws::NotificationHub`
//! implements it, and a caller without a hub passes `None`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::ci::CiTrigger;
use crate::email::SmtpConfig;
use rg_git::protocol::receive_pack::RefUpdate;

/// Sink for the real-time notifications a push produces (`ci_triggered`,
/// `push`), implemented by the HTTP layer's WebSocket hub.
///
/// Deliberately fire-and-forget and non-async: the hub's own helper already
/// detaches the send through `task_tracker::delivery_tracker()`, and the hook
/// path must never block on a socket fan-out.
pub trait PushNotifier: Send + Sync {
    /// Push `event_type` + `data` to the given user's channel.
    fn notify(&self, user_id: i64, event_type: &str, data: serde_json::Value);
}

/// Borrowed parameters for one post-push hook run.
pub struct PostPushParams<'a> {
    pub db: &'a DatabaseConnection,
    /// Path to the bare repository that received the push.
    pub repo_path: &'a Path,
    /// Root under which all repositories live (merge-queue / auto-merge need it).
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    /// The account that pushed, when the transport authenticated one.
    ///
    /// Only the watch fan-out needs it: it names the pusher in the notification
    /// and is how the pusher is kept off their own recipient list. `None` (an
    /// unauthenticated push on an open-access server) simply means nobody is
    /// excluded.
    pub pusher_id: Option<i64>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub allow_host_runner: bool,
    pub jwt_secret: &'a str,
    /// Real-time notification sink. `None` = no WebSocket hub in this process.
    pub notifier: Option<&'a dyn PushNotifier>,
    pub smtp_config: &'a Option<SmtpConfig>,
    pub ci_engine: &'a dyn CiTrigger,
    pub external_url: Option<&'a str>,
}

impl PostPushParams<'_> {
    /// Fan a real-time event out to `user_id`, or drop it when the caller has
    /// no notification hub.
    fn notify(&self, user_id: i64, event_type: &str, data: serde_json::Value) {
        if let Some(notifier) = self.notifier {
            notifier.notify(user_id, event_type, data);
        }
    }
}

/// Owned, cheaply-clonable form of everything [`post_push_hooks`] needs beyond
/// the per-push arguments.
///
/// A transport builds this once at startup and calls [`PostPushContext::run`]
/// per accepted push, instead of threading a dozen fields through its own
/// config. `None` anywhere a transport holds an `Option<PostPushContext>` means
/// "this deployment has no post-push automation wired" — a legitimate state for
/// an open-access server with no database.
#[derive(Clone)]
pub struct PostPushContext {
    pub repo_root: PathBuf,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub allow_host_runner: bool,
    pub jwt_secret: String,
    pub smtp_config: Option<SmtpConfig>,
    pub ci_engine: Arc<dyn CiTrigger + Send + Sync>,
    pub external_url: Option<String>,
    pub notifier: Option<Arc<dyn PushNotifier>>,
}

impl PostPushContext {
    /// Run every post-push hook for one accepted push.
    ///
    /// Call it from a **tracked** detached task
    /// (`rg_core::task_tracker::delivery_tracker().spawn(...)`), never from a
    /// bare `tokio::spawn`: the client already has its success response, so a
    /// SIGTERM in the next few seconds would sever the work at its first await
    /// with no trace that it was owed.
    pub async fn run(
        &self,
        db: &DatabaseConnection,
        repo_path: &Path,
        owner: &str,
        repo_name: &str,
        pusher_id: Option<i64>,
        ref_updates: &[RefUpdate],
    ) {
        post_push_hooks(
            &PostPushParams {
                db,
                repo_path,
                repo_root: &self.repo_root,
                owner,
                repo_name,
                pusher_id,
                docker_enabled: self.docker_enabled,
                external_runners: self.external_runners,
                allow_host_runner: self.allow_host_runner,
                jwt_secret: &self.jwt_secret,
                notifier: self.notifier.as_deref(),
                smtp_config: &self.smtp_config,
                ci_engine: &*self.ci_engine,
                external_url: self.external_url.as_deref(),
            },
            ref_updates,
        )
        .await;
    }
}

/// Post-push hook: trigger CI pipeline and webhook for push events.
pub async fn post_push_hooks(params: &PostPushParams<'_>, ref_updates: &[RefUpdate]) {
    // Find repo_id from DB
    let repo_model =
        crate::repo::service::find_repo_by_owner_name(params.db, params.owner, params.repo_name)
            .await;

    let (repo_id, repo_owner_id) = match repo_model {
        Ok(Some(r)) => (r.id, r.owner_id),
        _ => {
            tracing::warn!(owner = %params.owner, repo = %params.repo_name, "Post-push: repo not found in DB, skipping hooks");
            return;
        }
    };

    // Resolved once, not per ref: the watch fan-out below needs the pusher's
    // username, and a tag push can carry dozens of updates.
    let pusher_name = match params.pusher_id {
        Some(pusher_id) => rg_db::ops::user_ops::find_by_id(params.db, pusher_id)
            .await
            .ok()
            .flatten()
            .map(|user| user.username),
        None => None,
    };

    for update in ref_updates {
        if update.status != "ok" {
            continue;
        }

        tracing::info!(
            refname = %update.refname,
            new_sha = %update.new_sha,
            "Post-push: triggering hooks"
        );

        // 0. PR head-SHA refresh + auto-merge/merge-queue + protected-branch audit
        if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
            post_push_branch_maintenance(params, repo_id, branch_name, update).await;
        }

        // 1. Trigger CI pipeline if .forgekeep-ci.yml exists
        trigger_ci_for_push(params, repo_id, repo_owner_id, update).await;

        // 2-3. Push + branch/tag webhooks and the real-time notification
        trigger_push_webhooks(params, repo_id, repo_owner_id, update).await;

        // 4. Watch fan-out.
        //
        // The real-time notification above goes to `repo_owner_id` alone, so
        // until card_dc66742badc5 a "Watch" subscription produced nothing for a
        // push: `notify_watchers_push` existed with no caller anywhere in the
        // tree. This is that caller. Read access is re-checked per recipient
        // inside `notification::notify_watchers`, so no gate is needed here.
        if let Err(error) = crate::repo::service::notify_watchers_push(
            params.db,
            repo_id,
            params.repo_name,
            pusher_name.as_deref().unwrap_or_default(),
            &update.refname,
        )
        .await
        {
            tracing::warn!(error = %format!("{error:#}"), "failed to notify watchers about push");
        }
    }
}

/// Section 0 of the post-push hook (branch updates only): refresh open-PR head
/// SHAs, run the auto-merge / merge-queue evaluations for the new commit, and
/// emit the protected-branch acceptance audit log.
async fn post_push_branch_maintenance(
    params: &PostPushParams<'_>,
    repo_id: i64,
    branch_name: &str,
    update: &RefUpdate,
) {
    if !update.new_sha.chars().all(|character| character == '0') {
        match rg_db::ops::pull_request_ops::update_open_head_sha(
            params.db,
            repo_id,
            branch_name,
            &update.new_sha,
        )
        .await
        {
            Ok(_) => {
                if let Err(error) = crate::pull_request::try_auto_merges_for_head_commit(
                    params.db,
                    params.repo_root,
                    repo_id,
                    &update.new_sha,
                )
                .await
                {
                    tracing::warn!(error = %format!("{error:#}"), "auto-merge evaluation after push failed");
                }
                if let Err(error) =
                    crate::pull_request::merge_queue::process_for_head_commit_with_ci(
                        params.db,
                        params.repo_root,
                        repo_id,
                        &update.new_sha,
                        &crate::pull_request::merge_queue::MergeQueueCi {
                            trigger: params.ci_engine,
                            docker_enabled: params.docker_enabled,
                            external_runners: params.external_runners,
                            allow_host_runner: params.allow_host_runner,
                            jwt_secret: Some(params.jwt_secret),
                            external_url: params.external_url,
                        },
                    )
                    .await
                {
                    tracing::warn!(error = %format!("{error:#}"), "merge queue evaluation after push failed");
                }
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "failed to refresh PR head SHA after push")
            }
        }
    }
    match rg_db::ops::protected_branch_ops::find_by_repo_and_branch(params.db, repo_id, branch_name)
        .await
    {
        Ok(Some(_protection)) => {
            tracing::info!(
                branch = %branch_name,
                "Post-push: protected branch update accepted by pre-receive rules"
            );
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "Failed to check branch protection");
        }
        _ => {}
    }
}

/// Section 1 of the post-push hook: trigger a CI pipeline when a
/// `.forgekeep-ci.yml` is present at the pushed commit, then fan out the
/// real-time owner notification and the optional SMTP email.
async fn trigger_ci_for_push(
    params: &PostPushParams<'_>,
    repo_id: i64,
    repo_owner_id: i64,
    update: &RefUpdate,
) {
    if !params
        .ci_engine
        .has_ci_config(params.repo_path, &update.new_sha)
    {
        return;
    }
    let pipeline_id = match params
        .ci_engine
        .trigger_pipeline(crate::ci::TriggerPipelineParams {
            db: params.db,
            repo_path: params.repo_path,
            repo_id,
            commit_sha: &update.new_sha,
            ref_name: &update.refname,
            trigger_type: "push",
            triggered_by: None,
            docker_enabled: params.docker_enabled,
            external_runners: params.external_runners,
            allow_host_runner: params.allow_host_runner,
            jwt_secret: Some(params.jwt_secret),
            external_url: params.external_url,
        })
        .await
    {
        Ok(pipeline_id) => pipeline_id,
        Err(e) => {
            // `{:#}` keeps the whole anyhow chain: the outer context names the
            // workflow file, the cause carries the actual parse/validation reason.
            tracing::warn!("Failed to trigger CI pipeline: {:#}", e);
            return;
        }
    };

    tracing::info!(pipeline_id, "CI pipeline triggered");

    // Push real-time notification to repo owner
    params.notify(
        repo_owner_id,
        "ci_triggered",
        serde_json::json!({
            "pipeline_id": pipeline_id,
            "repo": format!("{}/{}", params.owner, params.repo_name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );

    // Send email notification if SMTP is configured
    if let Some(smtp) = params.smtp_config {
        if let Ok(Some(owner_user)) =
            rg_db::ops::user_ops::find_by_id(params.db, repo_owner_id).await
        {
            let subject = format!(
                "[ForgeKeep] CI pipeline #{} triggered for {}/{}",
                pipeline_id, params.owner, params.repo_name
            );
            let body = format!(
                "A CI pipeline has been triggered for repository {}/{} on branch {}.<br/><br/>Commit: {}<br/>Pipeline ID: {}",
                params.owner, params.repo_name, update.refname, update.new_sha, pipeline_id
            );
            if let Err(e) =
                crate::email::send_html_notification(smtp, &owner_user.email, &subject, &body, None)
                    .await
            {
                tracing::warn!(error = %format!("{e:#}"), "Failed to send CI notification email");
            }
        }
    }
}

/// Sections 2–3 of the post-push hook: fire the generic `push` webhook, the
/// branch/tag create/delete webhooks, and the real-time push notification.
async fn trigger_push_webhooks(
    params: &PostPushParams<'_>,
    repo_id: i64,
    repo_owner_id: i64,
    update: &RefUpdate,
) {
    // 2. Trigger push webhook
    let payload = serde_json::json!({
        "ref": update.refname,
        "before": update.old_sha,
        "after": update.new_sha,
        "repository": {
            "owner": params.owner,
            "name": params.repo_name,
        },
    });

    if let Err(e) =
        crate::webhook::service::trigger_event(params.db, repo_id, "push", &payload).await
    {
        tracing::warn!(error = %format!("{e:#}"), "Failed to trigger push webhook");
    }

    // 3. Trigger branch/tag-specific webhooks
    if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
        if update.old_sha.is_empty() || update.old_sha == "0000000000000000000000000000000000000000"
        {
            // New branch created
            if let Err(e) =
                crate::webhook::service::trigger_branch_created(params.db, repo_id, branch_name)
                    .await
            {
                tracing::warn!(
                    "Failed to trigger branch.created webhook for {}: {e}",
                    branch_name
                );
            }
        } else if update.new_sha.is_empty()
            || update.new_sha == "0000000000000000000000000000000000000000"
        {
            // Branch deleted
            if let Err(e) =
                crate::webhook::service::trigger_branch_deleted(params.db, repo_id, branch_name)
                    .await
            {
                tracing::warn!(
                    "Failed to trigger branch.deleted webhook for {}: {e}",
                    branch_name
                );
            }
        }
    } else if let Some(tag_name) = update.refname.strip_prefix("refs/tags/") {
        if update.old_sha.is_empty() || update.old_sha == "0000000000000000000000000000000000000000"
        {
            // New tag created
            if let Err(e) =
                crate::webhook::service::trigger_tag_created(params.db, repo_id, tag_name).await
            {
                tracing::warn!(
                    "Failed to trigger tag.created webhook for {}: {e}",
                    tag_name
                );
            }
        } else if update.new_sha.is_empty()
            || update.new_sha == "0000000000000000000000000000000000000000"
        {
            // Tag deleted
            if let Err(e) =
                crate::webhook::service::trigger_tag_deleted(params.db, repo_id, tag_name).await
            {
                tracing::warn!(
                    "Failed to trigger tag.deleted webhook for {}: {e}",
                    tag_name
                );
            }
        }
    }

    // Push real-time notification for push event
    params.notify(
        repo_owner_id,
        "push",
        serde_json::json!({
            "repo": format!("{}/{}", params.owner, params.repo_name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );
}
