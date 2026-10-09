//! Code review service — submit reviews, add inline comments, approve / request changes.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait,
};
use std::collections::{BTreeMap, HashSet};

use rg_db::entities::pr_review::{self, Model as PrReview};
use rg_db::entities::pull_request;
use rg_db::entities::review_comment::{self, Model as ReviewComment};
use rg_db::ops::{pr_review_ops, pull_request_ops, review_comment_ops};

/// The largest file that [`apply_suggestions`] holds in memory to splice a
/// suggested range into.
///
/// The apply path reads the target file whole through `git show`, decodes it to
/// UTF-8, and then splits it into a `Vec<String>` so the range can be replaced
/// line by line. That last step is a memory amplifier: each `String` header is
/// 24 bytes on top of its content bytes, so a newline-dense file could
/// multiply the resident set another ~24× before the splice returns. The
/// ceiling is therefore spent against the size in the tree BEFORE `git show`
/// reads a byte — the same rule the neighbouring readers of a committed blob
/// follow (`crate::issue_template::try_read_text_blob`,
/// `crate::review::codeowners::load_codeowners`, `rg-http::api::repo_content`).
/// 1 MiB matches the `MAX_EDITABLE_SIZE` the web editor enforces, so a file
/// the reviewer could not open to write the suggestion cannot ambush the
/// server on the way back.
const MAX_SUGGESTION_TARGET_BYTES: u64 = 1024 * 1024;

// ── Review actions ────────────────────────────────────────────────────

/// Review action types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    /// Submit a comment without explicit approval/rejection
    Comment,
    /// Approve the PR
    Approve,
    /// Request changes before merging
    RequestChanges,
    /// Dismiss a previous review
    Dismiss,
}

async fn create_review_with_event<C: ConnectionTrait>(
    db: &C,
    pr: &pull_request::Model,
    repo_id: i64,
    reviewer_id: i64,
    action: ReviewAction,
    body: Option<String>,
    commit_id: Option<String>,
) -> Result<PrReview> {
    let model = pr_review::ActiveModel {
        id: sea_orm::NotSet,
        pr_id: Set(pr.id),
        repo_id: Set(repo_id),
        reviewer_id: Set(reviewer_id),
        action: Set(action.as_str().to_string()),
        body: Set(body),
        commit_id: Set(commit_id.or_else(|| pr.head_sha.clone())),
        created_at: Set(Utc::now()),
        dismissed_at: Set(None),
        dismissed_by: Set(None),
    };

    let review = pr_review_ops::create(db, model).await?;
    rg_db::ops::pr_event_ops::record(
        db,
        repo_id,
        pr.id,
        Some(reviewer_id),
        &format!("review_{}", review.action),
        review.body.clone(),
        serde_json::json!({"review_id": review.id, "commit_id": review.commit_id}),
    )
    .await?;
    Ok(review)
}

impl ReviewAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Dismiss => "dismiss",
        }
    }

    pub fn parse_action(s: &str) -> Result<Self> {
        match s {
            "comment" => Ok(Self::Comment),
            "approve" => Ok(Self::Approve),
            "request_changes" => Ok(Self::RequestChanges),
            "dismiss" => Ok(Self::Dismiss),
            _ => Err(crate::error::invalid_request(format!(
                "invalid review action: {s}"
            ))),
        }
    }
}

// ── Submit a review ───────────────────────────────────────────────────

/// Submit a review on a pull request.
pub async fn submit_review(
    db: &DatabaseConnection,
    repo_id: i64,
    pr_number: i64,
    reviewer_id: i64,
    action: ReviewAction,
    body: Option<String>,
    commit_id: Option<String>,
) -> Result<PrReview> {
    // Validate PR exists
    let pr = pull_request_ops::find_by_repo_and_number(db, repo_id, pr_number)
        .await?
        .ok_or_else(|| crate::error::not_found("pull request"))?;

    if pr.state != "open" {
        // A closed PR is a *state* problem, not a malformed request: the same
        // call succeeds once the PR reopens, which is what a 409 says and a 400
        // does not.
        return Err(crate::error::conflict(format!(
            "cannot review a PR that is not open (current: {})",
            pr.state
        )));
    }

    // A dismissal names the review it withdraws; submitting one here would
    // create a row that withdraws nothing — the inert shape card_dc0f5d58e5f4
    // removed. Say where the operation actually lives instead of accepting it
    // and doing nothing.
    if matches!(action, ReviewAction::Dismiss) {
        return Err(crate::error::invalid_request(
            "a review cannot be submitted as a dismissal; dismiss a specific \
             review through POST .../pulls/{number}/reviews/{id}/dismiss",
        ));
    }

    let verb = match action {
        ReviewAction::Approve => "approved these changes",
        ReviewAction::RequestChanges => "requested changes",
        ReviewAction::Comment | ReviewAction::Dismiss => "reviewed",
    };
    let transaction = db.begin().await.context("db: begin review submission")?;
    let review = create_review_with_event(
        &transaction,
        &pr,
        repo_id,
        reviewer_id,
        action,
        body,
        commit_id,
    )
    .await?;
    transaction
        .commit()
        .await
        .context("db: commit review submission")?;

    let mut event = crate::notification::thread::ThreadEvent::new(
        crate::pull_request::service::pr_subject(&pr),
        Some(reviewer_id),
        verb,
    )
    .to_subscribers()
    .actor_subscribes("commented");
    if let Some(body) = review.body.as_deref() {
        event = event.mentions_in(body);
    }
    crate::notification::thread::spawn(db, event);
    Ok(review)
}

/// Tell `reviewer_id` that their review of pull request `pr_id` was requested
/// — by `requested_by_id` directly or through CODEOWNERS. Detached; the
/// request itself is already committed.
pub fn notify_review_requested(
    db: &DatabaseConnection,
    pr_id: i64,
    reviewer_id: i64,
    requested_by_id: i64,
) {
    let db = db.clone();
    crate::task_tracker::delivery_tracker().spawn(async move {
        let pr = match pull_request_ops::find_by_id(&db, pr_id).await {
            Ok(Some(pr)) => pr,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(
                    pr_id,
                    reviewer_id,
                    error = %format!("{error:#}"),
                    "review request notification skipped: pull request lookup failed"
                );
                return;
            }
        };
        let event = crate::notification::thread::ThreadEvent::new(
            crate::pull_request::service::pr_subject(&pr),
            Some(requested_by_id),
            "requested a review",
        )
        .to(
            reviewer_id,
            crate::notification::thread::Reason::ReviewRequested,
        );
        if let Err(error) = crate::notification::thread::deliver(&db, &event).await {
            tracing::warn!(
                pr_id,
                reviewer_id,
                error = %format!("{error:#}"),
                "review request notification was not delivered"
            );
        }
    });
}

/// List all reviews for a PR.
pub async fn list_reviews(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
) -> Result<Vec<PrReview>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let pr = pull_request_ops::find_by_repo_and_number(db, repo.id, pr_number)
        .await?
        .ok_or_else(|| crate::error::not_found("pull request"))?;

    pr_review_ops::list_by_pr(db, pr.id).await
}

/// Get a single review by ID.
pub async fn get_review(db: &DatabaseConnection, review_id: i64) -> Result<PrReview> {
    pr_review_ops::find_by_id(db, review_id)
        .await?
        .ok_or_else(|| crate::error::not_found("review"))
}

/// Withdraw a review.
///
/// The dismissal is stamped on the review being dismissed, not written as a
/// second `pr_reviews` row under the dismissor's name. That older shape was the
/// whole bug (card_dc0f5d58e5f4): `current_approvers` folds only
/// `approve` / `request_changes` into its per-reviewer verdict map, so a
/// `"dismiss"` row displaced nobody — and carrying the dismissor's
/// `reviewer_id`, it would have displaced the wrong person had it been folded
/// in. The maintainer got a 200 and a timeline entry while the pull request
/// stayed mergeable on the approval they had just withdrawn.
///
/// The timeline entry is still written — the event log is where "who did what,
/// when" belongs — but the merge gate now reads the stamp, so the two cannot
/// disagree.
pub async fn dismiss_review(
    db: &DatabaseConnection,
    review_id: i64,
    dismissor_id: i64,
    message: String,
) -> Result<PrReview> {
    let dismissed_at = Utc::now();
    let txn = db.begin().await.context("db: begin review dismissal")?;
    let Some(review) =
        pr_review_ops::mark_dismissed(&txn, review_id, dismissor_id, dismissed_at).await?
    else {
        return Err(crate::error::not_found("review"));
    };
    rg_db::ops::pr_event_ops::record(
        &txn,
        review.repo_id,
        review.pr_id,
        Some(dismissor_id),
        "review_dismiss",
        Some(message),
        serde_json::json!({
            "review_id": review.id,
            "commit_id": review.commit_id,
            "dismissed_action": review.action,
            "reviewer_id": review.reviewer_id,
        }),
    )
    .await?;
    txn.commit().await.context("db: commit review dismissal")?;
    Ok(review)
}

// ── Inline Review Comments ────────────────────────────────────────────

/// Create an inline review comment on a specific diff line.
#[allow(clippy::too_many_arguments)]
pub async fn create_review_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    pr_number: i64,
    review_id: Option<i64>,
    author_id: i64,
    path: String,
    line: Option<i64>,
    start_line: Option<i64>,
    side: Option<String>,
    start_side: Option<String>,
    body: String,
    suggestion: Option<String>,
    commit_id: Option<String>,
    reply_to_id: Option<i64>,
) -> Result<ReviewComment> {
    // Validate PR
    let pr = pull_request_ops::find_by_repo_and_number(db, repo_id, pr_number)
        .await?
        .ok_or_else(|| crate::error::not_found("pull request"))?;

    // Validate an explicitly named review before opening the write transaction.
    // With no id the comment endpoint creates its implicit `comment` review in
    // the same transaction as the comment and both timeline rows below.
    let review = if let Some(review_id) = review_id {
        let review = pr_review_ops::find_by_id(db, review_id)
            .await?
            .ok_or_else(|| crate::error::not_found("review"))?;
        // Both ids below are instance-wide primary keys, so a row belonging to
        // another pull request must read as absent rather than as a bad request:
        // the two answers together tell an id-walking caller which ids exist.
        if review.repo_id != repo_id || review.pr_id != pr.id {
            return Err(crate::error::not_found("review"));
        }
        Some(review)
    } else {
        None
    };

    // Validate reply_to if specified
    if let Some(rtid) = reply_to_id {
        let parent = review_comment_ops::find_by_id(db, rtid)
            .await?
            .ok_or_else(|| crate::error::not_found("parent comment"))?;
        if parent.pr_id != pr.id {
            return Err(crate::error::not_found("parent comment"));
        }
    }

    if body.trim().is_empty() {
        return Err(crate::error::invalid_request(
            "comment body cannot be empty",
        ));
    }
    let suggestion = suggestion.map(|value| value.replace("\r\n", "\n"));
    if suggestion.is_some() {
        // One pattern owns both the rule and the line it yields. Spelled as a
        // composite `if` followed by `line.unwrap()`, the two could drift apart
        // — and a suggestion payload is caller input, so the drift would be a
        // panic where a 400 belongs.
        let (None, Some(end), Some("RIGHT")) = (reply_to_id, line, side.as_deref()) else {
            return Err(crate::error::invalid_request(
                "suggestions require a top-level RIGHT-side line comment",
            ));
        };
        let start = start_line.unwrap_or(end);
        if start < 1 || start > end || start_side.as_deref().unwrap_or("RIGHT") != "RIGHT" {
            return Err(crate::error::invalid_request(
                "suggestion range must be an ordered RIGHT-side range",
            ));
        }
    }

    let transaction = db
        .begin()
        .await
        .context("db: begin review comment creation")?;
    let review = match review {
        Some(review) => review,
        None => {
            create_review_with_event(
                &transaction,
                &pr,
                repo_id,
                author_id,
                ReviewAction::Comment,
                None,
                commit_id.clone(),
            )
            .await?
        }
    };
    let model = review_comment::ActiveModel {
        id: sea_orm::NotSet,
        review_id: Set(review.id),
        pr_id: Set(pr.id),
        author_id: Set(author_id),
        path: Set(path),
        position: Set(None), // Deprecated, use line instead
        line: Set(line),
        start_line: Set(start_line),
        side: Set(side),
        start_side: Set(start_side),
        body: Set(body),
        suggestion: Set(suggestion),
        suggestion_applied_at: Set(None),
        suggestion_applied_by_id: Set(None),
        suggestion_commit_sha: Set(None),
        commit_id: Set(commit_id.or_else(|| review.commit_id.clone())),
        reply_to_id: Set(reply_to_id),
        resolved_at: Set(None),
        resolved_by_id: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
    };

    let comment = review_comment_ops::create(&transaction, model).await?;
    let event_type = if comment.reply_to_id.is_some() {
        "review_reply"
    } else if comment.suggestion.is_some() {
        "code_suggestion"
    } else {
        "review_comment"
    };
    rg_db::ops::pr_event_ops::record(
        &transaction,
        repo_id,
        pr.id,
        Some(author_id),
        event_type,
        Some(comment.body.clone()),
        serde_json::json!({
            "comment_id": comment.id,
            "path": comment.path,
            "start_line": comment.start_line,
            "line": comment.line,
            "side": comment.side,
            "reply_to_id": comment.reply_to_id
        }),
    )
    .await?;
    transaction
        .commit()
        .await
        .context("db: commit review comment creation")?;
    crate::notification::thread::spawn(
        db,
        crate::notification::thread::ThreadEvent::new(
            crate::pull_request::service::pr_subject(&pr),
            Some(author_id),
            "commented on the changes",
        )
        .mentions_in(comment.body.clone())
        .to_subscribers()
        .actor_subscribes("commented"),
    );
    Ok(comment)
}

#[derive(Debug, serde::Serialize)]
pub struct AppliedSuggestion {
    pub comment: ReviewComment,
    pub commit_sha: String,
}

#[derive(Debug, serde::Serialize)]
pub struct AppliedSuggestions {
    pub comments: Vec<ReviewComment>,
    pub commit_sha: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_suggestion(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    source_repo: &rg_db::entities::repository::Model,
    source_namespace: &str,
    pr: &rg_db::entities::pull_request::Model,
    comment_id: i64,
    actor: &rg_db::entities::user::Model,
) -> Result<AppliedSuggestion> {
    let mut applied = apply_suggestions(
        db,
        repo_root,
        source_repo,
        source_namespace,
        pr,
        &[comment_id],
        actor,
    )
    .await?;
    let comment = applied.comments.remove(0);
    Ok(AppliedSuggestion {
        comment,
        commit_sha: applied.commit_sha,
    })
}

#[derive(Debug)]
struct ValidatedSuggestion {
    comment: ReviewComment,
    start_line: i64,
    end_line: i64,
    replacement: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_suggestions(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    source_repo: &rg_db::entities::repository::Model,
    source_namespace: &str,
    pr: &rg_db::entities::pull_request::Model,
    comment_ids: &[i64],
    actor: &rg_db::entities::user::Model,
) -> Result<AppliedSuggestions> {
    if comment_ids.is_empty() || comment_ids.len() > 100 {
        return Err(crate::error::invalid_request(
            "between 1 and 100 suggestions must be selected",
        ));
    }
    let head_sha = pr.head_sha.as_deref().ok_or_else(|| {
        crate::error::conflict("pull request has no head commit to apply suggestions to")
    })?;
    let mut unique_ids = HashSet::new();
    let mut suggestions_by_path: BTreeMap<String, Vec<ValidatedSuggestion>> = BTreeMap::new();
    for &comment_id in comment_ids {
        if !unique_ids.insert(comment_id) {
            return Err(crate::error::invalid_request(format!(
                "duplicate suggestion comment #{comment_id}"
            )));
        }
        let comment = review_comment_ops::find_by_id(db, comment_id)
            .await?
            .ok_or_else(|| crate::error::not_found("review comment"))?;
        // Belonging to another pull request is indistinguishable from not
        // existing, and must answer the same way: `comment_ids` comes from the
        // request body, so a `400`/`404` split here would enumerate every
        // review comment on the instance from a pull request of one's own.
        if comment.pr_id != pr.id {
            return Err(crate::error::not_found("review comment"));
        }
        // A reply is a different complaint: the id names a comment of *this*
        // pull request, it simply cannot carry a suggestion.
        if comment.reply_to_id.is_some() {
            return Err(crate::error::invalid_request(
                "a reply cannot carry a suggestion",
            ));
        }
        let replacement = comment.suggestion.clone().ok_or_else(|| {
            crate::error::invalid_request("comment does not contain a suggestion")
        })?;
        // The next two are *state*, not shape: the same request succeeds again
        // once the suggestion is un-applied or the head moves back, so a 409
        // tells the client what to wait for instead of blaming the payload.
        if comment.suggestion_applied_at.is_some() {
            return Err(crate::error::conflict(
                "suggestion has already been applied",
            ));
        }
        if comment.commit_id.as_deref() != Some(head_sha) {
            return Err(crate::error::conflict(
                "suggestion is outdated because the pull request head has changed",
            ));
        }
        let end_line = comment.line.context("suggestion line is missing")?;
        let start_line = comment.start_line.unwrap_or(end_line);
        if start_line < 1
            || start_line > end_line
            || comment.side.as_deref() != Some("RIGHT")
            || comment.start_side.as_deref().unwrap_or("RIGHT") != "RIGHT"
        {
            return Err(crate::error::invalid_request(
                "suggestion must target a valid RIGHT-side range",
            ));
        }
        suggestions_by_path
            .entry(comment.path.clone())
            .or_default()
            .push(ValidatedSuggestion {
                comment,
                start_line,
                end_line,
                replacement,
            });
    }

    let push_policy = crate::branch_protection::server_side::ServerSideCommitPolicy::load(
        db,
        source_repo.id,
        &pr.head_branch,
        actor.id,
    )
    .await?;
    let repo_path = repo_root.join(format!("{source_namespace}/{}.git", source_repo.name));
    let head_sha_for_git = head_sha.to_string();
    let head_branch_for_git = pr.head_branch.clone();
    let source_namespace_for_git = source_namespace.to_string();
    let source_repo_name_for_git = source_repo.name.clone();
    let actor_username_for_git = actor.username.clone();
    let actor_email_for_git = actor.email.clone();
    let repo_root_for_git = repo_root.to_path_buf();
    let suggestion_count = comment_ids.len();
    let (suggestions_by_path, commit_sha) = crate::blocking::run_blocking_git(
        "preparing and committing review suggestions",
        move || {
            let git = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let mut file_updates = Vec::with_capacity(suggestions_by_path.len());
            for (path, suggestions) in &mut suggestions_by_path {
                suggestions.sort_by_key(|suggestion| suggestion.start_line);
                for pair in suggestions.windows(2) {
                    if pair[0].end_line >= pair[1].start_line {
                        return Err(crate::error::invalid_request(format!(
                            "suggestion ranges overlap in {path}"
                        )));
                    }
                }

                let object = format!("{head_sha_for_git}:{path}");
                // Charge the ceiling against the size in the tree BEFORE the read.
                // `git show` has no cap of its own, neither does the gateway that
                // collects its output, and the `Vec<String>` split below allocates a
                // 24-byte header per line on top of the content bytes. A file measured
                // only once it is decoded costs exactly the memory the ceiling was
                // declared to save; the size is chosen by whoever can push to the head
                // branch, and this endpoint is reachable by the PR author over their
                // own PR. An absent path is left to `git show` to report — pinning to
                // a commit id means the file the size came from and the file the
                // bytes come from are the same one, so the check does not race the
                // read.
                if let Some(size) =
                    crate::committed_blob::blob_size(git, &repo_path, &head_sha_for_git, path)?
                {
                    if size > MAX_SUGGESTION_TARGET_BYTES {
                        return Err(crate::error::invalid_request(format!(
                            "suggestion target {path} is larger than the \
                             {MAX_SUGGESTION_TARGET_BYTES}-byte limit"
                        )));
                    }
                }
                let content_output = git.run(&["show", &object], Some(&repo_path))?;
                content_output.ensure_success()?;
                let content = String::from_utf8(content_output.stdout)
                    .context("suggestions cannot be applied to a non-UTF-8 file")?;
                let sha_output = git.run(&["rev-parse", &object], Some(&repo_path))?;
                sha_output.ensure_success()?;
                let blob_sha = sha_output.stdout_str().trim().to_string();
                let had_trailing_newline = content.ends_with('\n');
                let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();

                for suggestion in suggestions.iter().rev() {
                    let start_index = (suggestion.start_line - 1) as usize;
                    let end_index = suggestion.end_line as usize;
                    if start_index >= lines.len() || end_index > lines.len() {
                        return Err(crate::error::conflict(format!(
                            "suggestion range {}-{} is outside {}",
                            suggestion.start_line, suggestion.end_line, path
                        )));
                    }
                    let replacement = suggestion
                        .replacement
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    if lines[start_index..end_index] == replacement {
                        return Err(crate::error::conflict(format!(
                            "suggestion #{} does not change {path}",
                            suggestion.comment.id
                        )));
                    }
                    lines.splice(start_index..end_index, replacement);
                }
                let mut updated_content = lines.join("\n");
                if had_trailing_newline {
                    updated_content.push('\n');
                }
                file_updates.push(crate::repo::service::FileUpdate {
                    path: path.clone(),
                    content: updated_content,
                    expected_blob_sha: blob_sha,
                });
            }

            let commit_sha = crate::repo::service::update_files_in_commit(
                &source_namespace_for_git,
                &source_repo_name_for_git,
                &head_branch_for_git,
                &head_sha_for_git,
                &file_updates,
                &format!("Apply {suggestion_count} review suggestion(s)"),
                &actor_username_for_git,
                &actor_email_for_git,
                &repo_root_for_git,
                &push_policy,
            )?;
            Ok((suggestions_by_path, commit_sha))
        },
    )
    .await?;

    // ── Point of no return ────────────────────────────────────────────
    //
    // The commit is now in `refs/heads/<head_branch>`: the branch has moved
    // exactly as it moves over `git push`, and nothing below can take that
    // back. Every step from here on is therefore best-effort with a loud log,
    // because an `Err` would not undo the commit — it would only cost the
    // caller the right to run `after_suggestions_applied`, and with it the CI
    // pipeline, the `push` webhook, the watch fan-out and the auto-merge /
    // merge-queue evaluation the new head can unblock. That is the whole set
    // card_e324a9281789 was written to deliver; a locked database must not be
    // able to withdraw it while answering the author `5xx: not applied`.
    if let Err(error) = pull_request_ops::advance_open_head_sha(
        db,
        source_repo.id,
        &pr.head_branch,
        head_sha,
        &commit_sha,
    )
    .await
    {
        // A zero row count is *not* an error and is deliberately not logged:
        // the compare-and-swap missing means a concurrent push already moved
        // the row past the SHA this update was prepared against, which is the
        // guard doing its job (sol_98910499eed2).
        tracing::error!(
            pr_id = pr.id,
            repo_id = source_repo.id,
            branch = %pr.head_branch,
            commit_sha = %commit_sha,
            error = %error,
            "suggestion commit is on the head branch but the pull request's head SHA \
             could not be advanced — the row now lags the branch until the next push"
        );
    }

    let now = Utc::now();
    // What each comment looks like once applied. This is both the source of
    // the `Set(...)` values below and the fallback the response is built from
    // when the write does not land: the commit exists either way, so the
    // caller is owed the applied suggestion — un-persisted markers are a
    // bookkeeping loss, not a reason to claim nothing happened.
    let mut comments = Vec::with_capacity(comment_ids.len());
    let mut updates = Vec::with_capacity(comment_ids.len());
    for suggestions in suggestions_by_path.values() {
        for suggestion in suggestions {
            // Built from the *stored* model, whose columns are `Unchanged`, so
            // the statement carries exactly the four columns set here.
            let mut active: review_comment::ActiveModel = suggestion.comment.clone().into();
            active.suggestion_applied_at = Set(Some(now));
            active.suggestion_applied_by_id = Set(Some(actor.id));
            active.suggestion_commit_sha = Set(Some(commit_sha.clone()));
            active.updated_at = Set(now);
            updates.push(active);

            let mut applied = suggestion.comment.clone();
            applied.suggestion_applied_at = Some(now);
            applied.suggestion_applied_by_id = Some(actor.id);
            applied.suggestion_commit_sha = Some(commit_sha.clone());
            applied.updated_at = now;
            comments.push(applied);
        }
    }
    match mark_suggestions_applied(db, updates).await {
        Ok(written) => comments = written,
        Err(error) => tracing::error!(
            pr_id = pr.id,
            commit_sha = %commit_sha,
            comment_ids = ?comment_ids,
            error = %error,
            "suggestion commit is on the head branch but the review comments could not be \
             marked as applied — the markers stay unset until somebody re-applies"
        ),
    }
    for comment in &comments {
        if let Err(error) = rg_db::ops::pr_event_ops::record(
            db,
            pr.repo_id,
            pr.id,
            Some(actor.id),
            "suggestion_applied",
            None,
            serde_json::json!({
                "comment_id": comment.id,
                "commit_sha": commit_sha
            }),
        )
        .await
        {
            tracing::error!(
                pr_id = pr.id,
                comment_id = comment.id,
                commit_sha = %commit_sha,
                error = %error,
                "suggestion commit is on the head branch but its timeline entry could not \
                 be recorded"
            );
        }
    }
    comments.sort_by_key(|comment| {
        comment_ids
            .iter()
            .position(|id| *id == comment.id)
            .unwrap_or(usize::MAX)
    });
    Ok(AppliedSuggestions {
        comments,
        commit_sha,
    })
}

/// Persist the applied-suggestion markers as one transaction.
///
/// Split out so the caller past the point of no return can treat the whole
/// write as a single fallible step: a partial set of markers is what the
/// transaction exists to prevent, and the caller has a snapshot to answer with
/// when it fails.
async fn mark_suggestions_applied(
    db: &DatabaseConnection,
    updates: Vec<review_comment::ActiveModel>,
) -> Result<Vec<ReviewComment>> {
    let transaction = db.begin().await?;
    let mut written = Vec::with_capacity(updates.len());
    for active in updates {
        written.push(active.update(&transaction).await?);
    }
    transaction.commit().await?;
    Ok(written)
}

/// Resolve or reopen the top-level thread containing a review comment.
pub async fn set_thread_resolved(
    db: &DatabaseConnection,
    pr_id: i64,
    comment_id: i64,
    actor_id: i64,
    resolved: bool,
) -> Result<ReviewComment> {
    let comment = get_thread_root(db, pr_id, comment_id).await?;

    let mut active: review_comment::ActiveModel = comment.into();
    active.resolved_at = Set(resolved.then(Utc::now));
    active.resolved_by_id = Set(resolved.then_some(actor_id));
    active.updated_at = Set(Utc::now());
    let transaction = db
        .begin()
        .await
        .context("db: begin review thread resolution")?;
    let updated = review_comment_ops::update(&transaction, active).await?;
    rg_db::ops::pr_event_ops::record(
        &transaction,
        // The repository is recovered from the PR to keep the event scoped.
        pull_request::Entity::find_by_id(pr_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| crate::error::not_found("pull request"))?
            .repo_id,
        pr_id,
        Some(actor_id),
        if resolved {
            "thread_resolved"
        } else {
            "thread_reopened"
        },
        None,
        serde_json::json!({"comment_id": updated.id}),
    )
    .await?;
    transaction
        .commit()
        .await
        .context("db: commit review thread resolution")?;
    Ok(updated)
}

/// Find and validate the top-level comment for a review thread.
pub async fn get_thread_root(
    db: &DatabaseConnection,
    pr_id: i64,
    comment_id: i64,
) -> Result<ReviewComment> {
    let mut comment = review_comment_ops::find_by_id(db, comment_id)
        .await?
        .ok_or_else(|| crate::error::not_found("review comment"))?;
    // A comment id is an instance-wide primary key, so "belongs to another pull
    // request" is the same answer as "there is no such comment" — and it has to
    // be *said* the same way. A `400` here answered `404` for an id that does
    // not exist and `400` for one that does, which is an existence oracle over
    // every review comment on the instance, reachable from any repository the
    // caller can read.
    if comment.pr_id != pr_id {
        return Err(crate::error::not_found("review comment"));
    }

    // Resolution is stored on the root. Replies currently form a shallow
    // tree, but walking makes this safe if clients reply to another reply.
    let mut hops = 0;
    while let Some(parent_id) = comment.reply_to_id {
        comment = review_comment_ops::find_by_id(db, parent_id)
            .await?
            .ok_or_else(|| crate::error::not_found("review thread root"))?;
        if comment.pr_id != pr_id {
            return Err(crate::error::not_found("review thread root"));
        }
        hops += 1;
        if hops > 100 {
            // Not the caller's doing — a reply chain this deep means the stored
            // tree is broken, so it stays a 5xx.
            bail!("review thread nesting is invalid");
        }
    }

    Ok(comment)
}

/// List all review comments for a PR.
pub async fn list_review_comments(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
) -> Result<Vec<ReviewComment>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let pr = pull_request_ops::find_by_repo_and_number(db, repo.id, pr_number)
        .await?
        .ok_or_else(|| crate::error::not_found("pull request"))?;

    review_comment_ops::list_by_pr(db, pr.id).await
}

// `check_approval_status` used to live here: "does this PR have enough
// approvals", judged against a `required_approvals` handed in by the *caller*
// and so detached from the branch-protection rule where that number actually
// lives. It had no caller; the live answer is `branch_protection::service::
// check_merge_allowed`, which reads the number off the rule
// (card_ab36709fa0c7).

// ── Helpers ───────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}

#[cfg(test)]
mod suggestion_target_size_guard {
    //! card_746256cf6ab4: `apply_suggestions` reads the file a review comment
    //! targets whole through `git show`, decodes it to UTF-8, and then splits
    //! it into a `Vec<String>` before splicing the range in. Neither `git show`
    //! nor the gateway that collects its output caps the bytes it hands back,
    //! and the line split adds a ~24-byte `String` header on top of each line's
    //! own bytes — a newline-dense file could multiply the resident set another
    //! ~24× before the splice returns. The ceiling therefore has to be spent
    //! against the size in the tree BEFORE `git show`. Behaviour tests cannot
    //! see whether the same message came from a ceiling spent before the read
    //! or after it (a `git show` that fills memory answers the same way as a
    //! ceiling that refused to look), so the ordering is asserted where it
    //! lives.
    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    #[test]
    fn the_named_ceiling_stays_beside_the_read() {
        let code = rust_source::production_rust_code_only(include_str!("service.rs"));
        assert!(
            code.contains("const MAX_SUGGESTION_TARGET_BYTES"),
            "the review-suggestion apply path no longer names its ceiling: nothing bounds the \
             file it is about to read into memory and split into a `Vec<String>` line by line"
        );
    }

    #[test]
    fn the_ceiling_is_spent_before_git_show_reads_the_target_file() {
        let code = rust_source::production_rust_code_only(include_str!("service.rs"));
        let start = code
            .find("pub async fn apply_suggestions(")
            .expect("`apply_suggestions` must still be the entry point");
        let body = &code[start..];
        let end = body[1..]
            .find("\nasync fn ")
            .or_else(|| body[1..].find("\nfn "))
            .or_else(|| body[1..].find("\npub "))
            .map(|offset| offset + 1)
            .unwrap_or(body.len());
        let body = &body[..end];

        let charge = body.find("blob_size(").expect(
            "`apply_suggestions` no longer calls `blob_size`: nothing bounds the blob it is \
             about to read into memory. The check that this test defends against a mutation of \
             therefore never fires.",
        );
        let ceiling = body.find("MAX_SUGGESTION_TARGET_BYTES").expect(
            "`apply_suggestions` no longer names `MAX_SUGGESTION_TARGET_BYTES`: nothing bounds \
             the blob it is about to read into memory",
        );
        // String literals are blanked in the code-only view, so `"show"` cannot
        // be found; the anchor is the first `git.run(` in the function body.
        // In this loop that first call IS the `git show <sha>:<path>` — the
        // suggestion-target read; the `rev-parse` `git.run` sits below it and
        // does not itself allocate the blob.
        let read = body.find("git.run(").expect(
            "`apply_suggestions` no longer runs git — the anchor this ordering is asserted \
             against has moved, so the assertion below proves nothing",
        );
        assert!(
            charge < ceiling && ceiling < read,
            "`apply_suggestions` compares against `MAX_SUGGESTION_TARGET_BYTES` only after \
             `git.run(&[\"show\", …])` has collected the blob: a 5 GiB file committed at the \
             suggestion's path is then materialised in full and refused afterwards"
        );
    }
}
