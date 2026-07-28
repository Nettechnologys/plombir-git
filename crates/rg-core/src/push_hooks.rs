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

use std::collections::{HashSet, VecDeque};
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
    /// The watch fan-out names the pusher in the notification and uses this to
    /// keep them off their own recipient list, and the CI pipeline records it as
    /// `triggered_by`. `None` (an unauthenticated push on an open-access server)
    /// means nobody is excluded and the pipeline has no attributed actor.
    pub pusher_id: Option<i64>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub allow_host_runner: bool,
    /// Secret the CI job tokens are signed with. `None` = this caller has none
    /// (a local CLI run); it is passed through as `None` rather than as an empty
    /// secret, which would mint tokens signed with "".
    pub jwt_secret: Option<&'a str>,
    /// Real-time notification sink. `None` = no WebSocket hub in this process.
    pub notifier: Option<&'a dyn PushNotifier>,
    pub smtp_config: &'a Option<SmtpConfig>,
    pub ci_engine: &'a dyn CiTrigger,
    pub external_url: Option<&'a str>,
    pub delivery_tracker: &'a crate::task_tracker::TaskTracker,
}

impl PostPushParams<'_> {
    /// This run's CI wiring, in the borrowed form a pipeline-triggering path
    /// takes (the merge queue, the pull-request trigger).
    fn pipeline_ci(&self) -> crate::pull_request::ci::PipelineCi<'_> {
        crate::pull_request::ci::PipelineCi {
            trigger: self.ci_engine,
            docker_enabled: self.docker_enabled,
            external_runners: self.external_runners,
            allow_host_runner: self.allow_host_runner,
            jwt_secret: self.jwt_secret,
            external_url: self.external_url,
        }
    }

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
    /// See [`PostPushParams::jwt_secret`].
    pub jwt_secret: Option<String>,
    pub smtp_config: Option<SmtpConfig>,
    pub ci_engine: Arc<dyn CiTrigger + Send + Sync>,
    pub external_url: Option<String>,
    pub notifier: Option<Arc<dyn PushNotifier>>,
    /// Tracker used for detached post-push work spawned from this context.
    pub delivery_tracker: crate::task_tracker::TaskTracker,
}

impl PostPushContext {
    /// Run every post-push hook for one accepted push.
    ///
    /// Call it from a **tracked** detached task through this context's
    /// `delivery_tracker`, never from a bare `tokio::spawn`: the client already
    /// has its success response, so a
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
                jwt_secret: self.jwt_secret.as_deref(),
                notifier: self.notifier.as_deref(),
                smtp_config: &self.smtp_config,
                ci_engine: &*self.ci_engine,
                external_url: self.external_url.as_deref(),
                delivery_tracker: &self.delivery_tracker,
            },
            ref_updates,
        )
        .await;
    }

    /// This process's CI wiring, in the borrowed form a pipeline-triggering
    /// path takes (the merge queue, the pull-request trigger).
    pub fn pipeline_ci(&self) -> crate::pull_request::ci::PipelineCi<'_> {
        crate::pull_request::ci::PipelineCi {
            trigger: &*self.ci_engine,
            docker_enabled: self.docker_enabled,
            external_runners: self.external_runners,
            allow_host_runner: self.allow_host_runner,
            jwt_secret: self.jwt_secret.as_deref(),
            external_url: self.external_url.as_deref(),
        }
    }

    /// Evaluate the merges a new head commit unblocks and run the post-push
    /// hooks for every base branch they moved.
    ///
    /// This is the whole job of a caller that just made a commit reachable — a
    /// finished CI pipeline, an applied review suggestion. Doing only the first
    /// half is the defect of card_73a1ec5b32f3: the merge lands, the merge
    /// commit on `main` gets no pipeline, no `push` webhook and no watch
    /// notification, precisely in the flow auto-merge exists for.
    pub async fn evaluate_merges_and_spawn_hooks(
        &self,
        db: &DatabaseConnection,
        source_repo_id: i64,
        commit_sha: &str,
        actor_id: Option<i64>,
    ) {
        let merged = evaluate_merges_for_head_commit(
            db,
            &self.repo_root,
            source_repo_id,
            commit_sha,
            &self.pipeline_ci(),
        )
        .await;
        self.spawn_for_merged_refs(db, actor_id, merged);
    }

    /// Trigger the `pull_request` pipeline for a PR that just became current,
    /// detached through the delivery tracker.
    ///
    /// Detached for the same reason the push hooks are: the caller has already
    /// answered its request, and reading the repository's workflows plus
    /// creating the pipeline rows has no business sitting in that response.
    /// Tracked rather than a bare `tokio::spawn` so a SIGTERM in the next few
    /// seconds does not sever it without trace (card_8d4148774f32).
    pub fn spawn_pull_request_ci(
        &self,
        db: &DatabaseConnection,
        pr: rg_db::entities::pull_request::Model,
        actor_id: Option<i64>,
    ) {
        let context = self.clone();
        let db = db.clone();
        self.delivery_tracker.spawn(async move {
            crate::pull_request::trigger_pull_request_ci_best_effort(
                &db,
                &context.repo_root,
                &pr,
                actor_id,
                &context.pipeline_ci(),
            )
            .await;
        });
    }

    /// Run the post-push hooks for base-branch moves merges just made, detached
    /// through the delivery tracker.
    ///
    /// One hook run per move rather than one for all of them: the moves can
    /// belong to different repositories (a fork PR merges into the upstream),
    /// and a run is scoped to a single repository.
    pub fn spawn_for_merged_refs(
        &self,
        db: &DatabaseConnection,
        actor_id: Option<i64>,
        merged: Vec<crate::pull_request::MergedRef>,
    ) {
        if merged.is_empty() {
            return;
        }
        let context = self.clone();
        let db = db.clone();
        self.delivery_tracker.spawn(async move {
            for merged_ref in merged {
                let repo_path = context
                    .repo_root
                    .join(format!("{}/{}.git", merged_ref.owner, merged_ref.repo_name));
                context
                    .run(
                        &db,
                        &repo_path,
                        &merged_ref.owner,
                        &merged_ref.repo_name,
                        actor_id,
                        std::slice::from_ref(&merged_ref.update),
                    )
                    .await;
            }
        });
    }
}

/// Run both merge evaluations a new head commit can unblock — auto-merge and the
/// merge queue — and report every base-branch move they made.
///
/// The pair was copied byte-for-byte at six call sites (the push hooks below,
/// both CI-completion paths in `rg-ci`, the external runner's `finish_job`, and
/// the two review paths), and five of them dropped the ref moves on the floor,
/// so the most common auto-merge there is — "CI went green, the PR went in" —
/// produced a merge commit on `main` that no automation ever saw
/// (card_73a1ec5b32f3). One helper now, and its return value is the thing a
/// caller must not ignore.
///
/// Best-effort by design: a failed evaluation is logged, never propagated. The
/// caller has already finished the work it was actually asked to do (a CI job, a
/// review), and a merge that could not run must not turn that into an error.
pub async fn evaluate_merges_for_head_commit(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &crate::pull_request::ci::PipelineCi<'_>,
) -> Vec<crate::pull_request::MergedRef> {
    let mut merged_refs = Vec::new();
    match crate::pull_request::try_auto_merges_for_head_commit(
        db,
        repo_root,
        source_repo_id,
        commit_sha,
    )
    .await
    {
        Ok(outcomes) => merged_refs.extend(
            outcomes
                .into_iter()
                .filter_map(|outcome| outcome.merge)
                .filter_map(|merge| merge.base_ref_update),
        ),
        Err(error) => {
            tracing::warn!(
                repo_id = source_repo_id,
                commit_sha,
                error = %format!("{error:#}"),
                "auto-merge evaluation for a new head commit failed"
            )
        }
    }
    match crate::pull_request::merge_queue::process_for_head_commit_with_ci(
        db,
        repo_root,
        source_repo_id,
        commit_sha,
        ci,
    )
    .await
    {
        Ok(results) => {
            merged_refs.extend(results.into_iter().flat_map(|run| run.merged_ref_updates))
        }
        Err(error) => {
            tracing::warn!(
                repo_id = source_repo_id,
                commit_sha,
                error = %format!("{error:#}"),
                "merge queue evaluation for a new head commit failed"
            )
        }
    }
    merged_refs
}

/// How deep a chain of hook-triggered merges may run before it is cut off.
///
/// Each step of a legitimate chain closes a pull request, so a real workflow
/// converges in a couple of hops; anything longer is a cycle somebody built by
/// accident. Eight is far above any sane fan-out and still a hard stop.
const MAX_MERGE_CASCADE_DEPTH: usize = 8;

/// The work list of ref updates one hook run has left to process.
///
/// The hooks can *cause* ref updates: they run auto-merge and the merge queue,
/// and a merge advances the base branch. That move owes the same hooks — but
/// running them by recursion would be a cycle with no visible bound
/// (hook → merge → hook → …) and, in an `async fn`, would not even compile
/// without boxing. So the cycle is a loop here, with both of its bounds written
/// down in one place where a test can hold them:
///
/// * every `(refname, new_sha)` is processed **at most once** per run, and
/// * a chain of merges may not run deeper than [`MAX_MERGE_CASCADE_DEPTH`].
///
/// The first bound is what actually makes an infinite cascade impossible: a
/// merge produces a *new* commit, so a repeat can only come from a cycle. The
/// depth limit is the backstop for a cycle that keeps minting fresh commits.
struct RefUpdateCascade {
    pending: VecDeque<(Arc<HookTarget>, RefUpdate, usize)>,
    seen: HashSet<(i64, String, String)>,
}

/// The repository a queued ref move belongs to.
///
/// A cascade item is a ref move *plus* its repository, not a bare [`RefUpdate`]:
/// the merges the hooks run are evaluated by head commit, and a fork PR's base
/// branch lives in a different repository than the one that received the push.
/// Carrying the identity is what keeps the pipeline, the webhooks and the watch
/// fan-out pointed at the repository whose branch actually moved.
struct HookTarget {
    repo_id: i64,
    owner_id: i64,
    /// Namespace — user or organization name.
    owner: String,
    name: String,
    /// Bare repository the ref lives in.
    path: PathBuf,
}

impl RefUpdateCascade {
    /// Seed the work list with the updates the transport actually received.
    fn new(target: Arc<HookTarget>, initial: &[RefUpdate]) -> Self {
        let mut cascade = Self {
            pending: VecDeque::new(),
            seen: HashSet::new(),
        };
        cascade.extend(target, initial.iter().cloned(), 0);
        cascade
    }

    /// Queue follow-up updates discovered at `depth`, dropping the ones that
    /// break either bound.
    fn extend(
        &mut self,
        target: Arc<HookTarget>,
        updates: impl IntoIterator<Item = RefUpdate>,
        depth: usize,
    ) {
        for update in updates {
            if depth > MAX_MERGE_CASCADE_DEPTH {
                tracing::warn!(
                    refname = %update.refname,
                    new_sha = %update.new_sha,
                    depth,
                    "post-push cascade cut off at the depth limit — a merge chain this long is a loop, not a workflow"
                );
                continue;
            }
            if !self.seen.insert((
                target.repo_id,
                update.refname.clone(),
                update.new_sha.clone(),
            )) {
                tracing::debug!(
                    repo_id = target.repo_id,
                    refname = %update.refname,
                    new_sha = %update.new_sha,
                    "post-push cascade skipped a ref move it had already handled in this run"
                );
                continue;
            }
            self.pending.push_back((target.clone(), update, depth));
        }
    }

    #[allow(clippy::should_implement_trait)]
    fn next(&mut self) -> Option<(Arc<HookTarget>, RefUpdate, usize)> {
        self.pending.pop_front()
    }
}

/// Look up the repository a merge moved a branch in, so its ref move can be
/// processed under its own identity instead of the pusher's repository.
async fn resolve_hook_target(
    db: &DatabaseConnection,
    repo_root: &Path,
    owner: &str,
    name: &str,
) -> Option<HookTarget> {
    match crate::repo::service::find_repo_by_owner_name(db, owner, name).await {
        Ok(Some(repo)) => Some(HookTarget {
            repo_id: repo.id,
            owner_id: repo.owner_id,
            owner: owner.to_string(),
            name: name.to_string(),
            path: repo_root.join(format!("{owner}/{name}.git")),
        }),
        Ok(None) => {
            tracing::warn!(
                owner,
                repo = name,
                "Post-push: repo not found in DB, skipping hooks"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                owner,
                repo = name,
                error = %format!("{error:#}"),
                "Post-push: repo lookup failed, skipping hooks"
            );
            None
        }
    }
}

/// Post-push hook: trigger CI pipeline and webhook for push events.
pub async fn post_push_hooks(params: &PostPushParams<'_>, ref_updates: &[RefUpdate]) {
    // Find repo_id from DB. The pushed repository keeps the path the transport
    // handed us; only repositories the cascade discovers get one derived from
    // the repo root.
    let seed = match crate::repo::service::find_repo_by_owner_name(
        params.db,
        params.owner,
        params.repo_name,
    )
    .await
    {
        Ok(Some(repo)) => HookTarget {
            repo_id: repo.id,
            owner_id: repo.owner_id,
            owner: params.owner.to_string(),
            name: params.repo_name.to_string(),
            path: params.repo_path.to_path_buf(),
        },
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

    let mut cascade = RefUpdateCascade::new(Arc::new(seed), ref_updates);
    while let Some((target, update, depth)) = cascade.next() {
        if update.status != "ok" {
            continue;
        }

        tracing::info!(
            repo_id = target.repo_id,
            refname = %update.refname,
            new_sha = %update.new_sha,
            depth,
            "Post-push: triggering hooks"
        );

        // 0. PR head-SHA refresh + auto-merge/merge-queue + protected-branch audit
        if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
            let merged = post_push_branch_maintenance(params, &target, branch_name, &update).await;
            // A merge the maintenance above performed advanced *another* branch,
            // and that move owes these same hooks. Back into the work list it
            // goes rather than into a recursive call (card_87c4912c51ed) — under
            // the repository whose branch moved, which for a fork PR is not the
            // one that received the push.
            for merged_ref in merged {
                let next = if merged_ref.repo_id == target.repo_id {
                    Some(target.clone())
                } else {
                    resolve_hook_target(
                        params.db,
                        params.repo_root,
                        &merged_ref.owner,
                        &merged_ref.repo_name,
                    )
                    .await
                    .map(Arc::new)
                };
                if let Some(next) = next {
                    cascade.extend(next, [merged_ref.update], depth + 1);
                }
            }
        }

        // 1. Trigger CI pipeline if .forgekeep-ci.yml exists
        trigger_ci_for_push(params, &target, &update).await;

        // 2-3. Push + branch/tag webhooks and the real-time notification
        trigger_push_webhooks(params, &target, &update).await;

        // 4. Watch fan-out.
        //
        // The real-time notification above goes to `repo_owner_id` alone, so
        // until card_dc66742badc5 a "Watch" subscription produced nothing for a
        // push: `notify_watchers_push` existed with no caller anywhere in the
        // tree. This is that caller. Read access is re-checked per recipient
        // inside `notification::notify_watchers`, so no gate is needed here.
        //
        // Onto this run's tracker rather than awaited: the walk is one query
        // pair per subscriber, and a repository with thousands of them would
        // otherwise hold up every later ref in this same push.
        crate::repo::service::notify_watchers_push(
            params.db,
            params.delivery_tracker,
            target.repo_id,
            &target.name,
            pusher_name.as_deref().unwrap_or_default(),
            &update.refname,
        );
    }
}

/// Section 0 of the post-push hook (branch updates only): refresh open-PR head
/// SHAs, run the auto-merge / merge-queue evaluations for the new commit, and
/// emit the protected-branch acceptance audit log.
///
/// Returns the base-branch moves the merges it performed produced, so the caller
/// can run this same hook run over them (card_87c4912c51ed).
async fn post_push_branch_maintenance(
    params: &PostPushParams<'_>,
    target: &HookTarget,
    branch_name: &str,
    update: &RefUpdate,
) -> Vec<crate::pull_request::MergedRef> {
    let mut merged_refs = Vec::new();
    if !update.new_sha.chars().all(|character| character == '0') {
        match rg_db::ops::pull_request_ops::update_open_head_sha(
            params.db,
            target.repo_id,
            branch_name,
            &update.new_sha,
        )
        .await
        {
            Ok(open_prs) => {
                // The `pull_request` half of the event pair a forge emits for a
                // branch that has an open PR on it: the push gets its own `push`
                // pipeline below, and every PR this branch heads has been
                // synchronised and owes a pipeline of its own. Nothing produced
                // that event before card_074d93bfe327, so `on: pull_request`
                // selected workflows that never ran.
                //
                // The condition is the *ref move* this run is processing, not
                // "did the UPDATE above change a row": a server-side path may
                // have advanced the PR itself before handing the move over
                // (`advance_open_head_sha`, the applied-suggestion path), and
                // keying on the row would silently skip exactly those. Repeats
                // are bounded by the cascade's own `seen` set, which drops a
                // `(repo, refname, new_sha)` it has already handled.
                if update.old_sha != update.new_sha {
                    for pr in &open_prs {
                        crate::pull_request::trigger_pull_request_ci_best_effort(
                            params.db,
                            params.repo_root,
                            pr,
                            params.pusher_id,
                            &params.pipeline_ci(),
                        )
                        .await;
                    }
                }
                merged_refs = evaluate_merges_for_head_commit(
                    params.db,
                    params.repo_root,
                    target.repo_id,
                    &update.new_sha,
                    &params.pipeline_ci(),
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "failed to refresh PR head SHA after push")
            }
        }
    }
    match rg_db::ops::protected_branch_ops::find_by_repo_and_branch(
        params.db,
        target.repo_id,
        branch_name,
    )
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
    merged_refs
}

/// Section 1 of the post-push hook: trigger a CI pipeline when a
/// `.forgekeep-ci.yml` is present at the pushed commit, then fan out the
/// real-time owner notification and the optional SMTP email.
async fn trigger_ci_for_push(params: &PostPushParams<'_>, target: &HookTarget, update: &RefUpdate) {
    if !params
        .ci_engine
        .has_ci_config(&target.path, &update.new_sha)
    {
        return;
    }
    let pipeline_id = match params
        .ci_engine
        .trigger_pipeline(crate::ci::TriggerPipelineParams {
            db: params.db,
            repo_path: &target.path,
            repo_id: target.repo_id,
            commit_sha: &update.new_sha,
            ref_name: &update.refname,
            trigger_type: "push",
            // A push targets no branch other than the one it moves.
            base_branch: None,
            // The transport knows who pushed, and every other trigger path
            // records its actor, so a push pipeline had no reason to be the one
            // anonymous row in the table — `triggered_by` was hardcoded `None`
            // even when the push was authenticated.
            triggered_by: params.pusher_id,
            docker_enabled: params.docker_enabled,
            external_runners: params.external_runners,
            allow_host_runner: params.allow_host_runner,
            jwt_secret: params.jwt_secret,
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
        target.owner_id,
        "ci_triggered",
        serde_json::json!({
            "pipeline_id": pipeline_id,
            "repo": format!("{}/{}", target.owner, target.name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );

    // Send email notification if SMTP is configured
    if let Some(smtp) = params.smtp_config {
        if let Ok(Some(owner_user)) =
            rg_db::ops::user_ops::find_by_id(params.db, target.owner_id).await
        {
            let subject = format!(
                "[ForgeKeep] CI pipeline #{} triggered for {}/{}",
                pipeline_id, target.owner, target.name
            );
            let body = format!(
                "A CI pipeline has been triggered for repository {}/{} on branch {}.<br/><br/>Commit: {}<br/>Pipeline ID: {}",
                target.owner, target.name, update.refname, update.new_sha, pipeline_id
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
    target: &HookTarget,
    update: &RefUpdate,
) {
    let repo_id = target.repo_id;
    // 2. Trigger push webhook
    let payload = serde_json::json!({
        "ref": update.refname,
        "before": update.old_sha,
        "after": update.new_sha,
        "repository": {
            "owner": target.owner,
            "name": target.name,
        },
    });

    if let Err(e) = crate::webhook::service::trigger_event_with_tracker(
        params.db,
        repo_id,
        "push",
        &payload,
        params.delivery_tracker,
    )
    .await
    {
        tracing::warn!(error = %format!("{e:#}"), "Failed to trigger push webhook");
    }

    // 3. Trigger branch/tag-specific webhooks
    if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
        if update.old_sha.is_empty() || update.old_sha == "0000000000000000000000000000000000000000"
        {
            // New branch created
            let payload = serde_json::json!({
                "event": "branch.created",
                "ref": branch_name,
                "ref_type": "branch",
            });
            if let Err(e) = crate::webhook::service::trigger_event_with_tracker(
                params.db,
                repo_id,
                "branch.created",
                &payload,
                params.delivery_tracker,
            )
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
            let payload = serde_json::json!({
                "event": "branch.deleted",
                "ref": branch_name,
                "ref_type": "branch",
            });
            if let Err(e) = crate::webhook::service::trigger_event_with_tracker(
                params.db,
                repo_id,
                "branch.deleted",
                &payload,
                params.delivery_tracker,
            )
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
            let payload = serde_json::json!({
                "event": "tag.created",
                "ref": tag_name,
                "ref_type": "tag",
            });
            if let Err(e) = crate::webhook::service::trigger_event_with_tracker(
                params.db,
                repo_id,
                "tag.created",
                &payload,
                params.delivery_tracker,
            )
            .await
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
            let payload = serde_json::json!({
                "event": "tag.deleted",
                "ref": tag_name,
                "ref_type": "tag",
            });
            if let Err(e) = crate::webhook::service::trigger_event_with_tracker(
                params.db,
                repo_id,
                "tag.deleted",
                &payload,
                params.delivery_tracker,
            )
            .await
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
        target.owner_id,
        "push",
        serde_json::json!({
            "repo": format!("{}/{}", target.owner, target.name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(refname: &str, new_sha: &str) -> RefUpdate {
        RefUpdate {
            old_sha: "1".repeat(40),
            new_sha: new_sha.to_string(),
            refname: refname.to_string(),
            status: "ok".to_string(),
            message: String::new(),
        }
    }

    fn target(repo_id: i64) -> Arc<HookTarget> {
        Arc::new(HookTarget {
            repo_id,
            owner_id: 1,
            owner: "owner".to_string(),
            name: format!("repo-{repo_id}"),
            path: PathBuf::from(format!("/repos/owner/repo-{repo_id}.git")),
        })
    }

    /// The hooks run auto-merge, auto-merge moves a branch, and that move is fed
    /// back into the hooks. Modelled here at its worst: *every* update produces
    /// another, forever. The loop must still end — before card_87c4912c51ed this
    /// shape did not exist at all, and the obvious way to write it (recursion)
    /// would have had no bound.
    #[test]
    fn a_self_feeding_cascade_stops_at_the_depth_limit() {
        let mut cascade = RefUpdateCascade::new(target(1), &[moved("refs/heads/main", "commit-0")]);
        let mut processed = 0usize;

        while let Some((target, _update, depth)) = cascade.next() {
            processed += 1;
            assert!(
                processed <= MAX_MERGE_CASCADE_DEPTH + 1,
                "the cascade ran past its own bound — this is the infinite \
                 hook → merge → hook loop the work list exists to prevent"
            );
            // Each round mints a fresh commit, so the seen-set cannot stop it:
            // only the depth limit can.
            cascade.extend(
                target,
                [moved("refs/heads/main", &format!("commit-{}", depth + 1))],
                depth + 1,
            );
        }

        assert_eq!(
            processed,
            MAX_MERGE_CASCADE_DEPTH + 1,
            "the chain must run exactly the allowed depth (the seed plus \
             {MAX_MERGE_CASCADE_DEPTH} follow-ups) and then stop"
        );
    }

    /// A cycle that returns to a ref move already handled is the cheaper half of
    /// the bound, and the one that actually fires: a merge commit is unique, so
    /// seeing the same `(refname, new_sha)` twice means the chain closed a loop.
    #[test]
    fn the_same_ref_move_is_never_processed_twice() {
        let mut cascade =
            RefUpdateCascade::new(target(1), &[moved("refs/heads/main", "merge-commit")]);
        let mut processed = 0usize;

        while let Some((target, update, depth)) = cascade.next() {
            processed += 1;
            assert!(processed <= 2, "a repeated ref move must not be re-run");
            cascade.extend(target, [update], depth + 1);
        }

        assert_eq!(
            processed, 1,
            "the second sighting of the same (refname, new_sha) must be dropped"
        );
    }

    /// The same `(refname, new_sha)` in a *different* repository is a different
    /// ref move: a fork PR merges into the upstream repository, so the moves a
    /// cascade discovers are not all in the repository that received the push,
    /// and deduplicating them by name alone would silently drop the upstream's
    /// hooks (card_73a1ec5b32f3).
    #[test]
    fn the_same_ref_name_in_another_repository_is_not_deduplicated() {
        let mut cascade =
            RefUpdateCascade::new(target(1), &[moved("refs/heads/main", "merge-commit")]);
        cascade.extend(target(2), [moved("refs/heads/main", "merge-commit")], 1);

        let mut repos: Vec<i64> = Vec::new();
        while let Some((target, _update, _depth)) = cascade.next() {
            repos.push(target.repo_id);
        }

        assert_eq!(
            repos,
            vec![1, 2],
            "both repositories owe hooks for their own branch move"
        );
    }

    /// The hook run a merge owes must stay on the *tracked* spawn path, for the
    /// reason the pushed one does: whoever merged already has its response, so a
    /// bare `tokio::spawn` would be severed by a SIGTERM seconds later with no
    /// trace that the work was owed (card_8d4148774f32). `rg-http` guards its own
    /// half of this in `git_http.rs`.
    #[test]
    fn merged_ref_hooks_are_detached_through_the_delivery_tracker() {
        let lines: Vec<&str> = include_str!("push_hooks.rs").lines().collect();
        let helper = lines
            .iter()
            .position(|line| {
                line.trim_start()
                    .starts_with("pub fn spawn_for_merged_refs")
            })
            .expect("the merge hooks must still go through spawn_for_merged_refs");
        let spawn = lines[helper..]
            .iter()
            .position(|line| line.contains("spawn("))
            .expect("spawn_for_merged_refs must detach the hook run");
        let spawn_line = lines[helper + spawn].trim();
        assert!(
            spawn_line.contains(".delivery_tracker.spawn(")
                || spawn_line.contains("delivery_tracker().spawn("),
            "the merge hook run must be spawned through a delivery tracker \
             so the shutdown drain awaits it; found `{spawn_line}`"
        );
    }

    /// The bound must not punish a legitimate push: `git push --tags` carries one
    /// update per tag, all of them at depth 0.
    #[test]
    fn a_wide_push_batch_is_not_mistaken_for_a_cascade() {
        let updates: Vec<RefUpdate> = (0..50)
            .map(|index| moved(&format!("refs/tags/v{index}"), &format!("tag-{index}")))
            .collect();
        let mut cascade = RefUpdateCascade::new(target(1), &updates);

        let mut processed = 0usize;
        while cascade.next().is_some() {
            processed += 1;
        }

        assert_eq!(
            processed, 50,
            "every ref of a wide push must be processed — the depth limit bounds \
             chains of merges, not the size of one push"
        );
    }
}
