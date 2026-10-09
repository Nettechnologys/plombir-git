//! Notifications about one issue or one pull request (card_349c2b6a0d7c).
//!
//! A repository's watchers hear about everything that happens in it; that is
//! [`super::notify_watchers`]. This module is the other half: the people an
//! event is *addressed to* — a reviewer asked to approve, an assignee, someone
//! `@mentioned`, the author of a pull request whose CI just failed — and the
//! people following the thread because they took part in it.
//!
//! Every event goes through [`deliver`]:
//!
//! 1. The actor, and everyone the event addresses, become subscribers of the
//!    thread unless they already have a row — an explicit unsubscribe stays.
//! 2. Recipients are collected with the strongest [`Reason`] each one has, the
//!    actor is dropped, and every one of them is read-checked against the
//!    repository: a mention of someone who cannot read a private repository
//!    tells them nothing.
//! 3. A recipient with an unread notification about the same subject gets that
//!    row updated and moved to the top instead of a second one — ten comments
//!    in a busy thread are one line in the inbox.
//! 4. A row whose reason the recipient chose to be mailed about is marked
//!    `email_pending`; [`super::mail`] sends what piled up per person.
//!
//! A notification never carries the text of a comment, only who did what. A
//! comment that is deleted — a pasted secret is the usual reason — therefore
//! leaves nothing behind in anyone's inbox; an issue that is deleted takes its
//! notifications and subscriptions with it ([`forget_subject`]).

use std::collections::BTreeMap;

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::entities::notification_setting::Model as Settings;
use rg_db::ops::{notification_ops, notification_setting_ops, thread_subscription_ops};

/// What a thread is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubjectKind {
    Issue,
    PullRequest,
}

impl SubjectKind {
    /// The value stored in `subject_type`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull_request",
        }
    }

    fn path_segment(self) -> &'static str {
        match self {
            Self::Issue => "issues",
            Self::PullRequest => "pulls",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull request",
        }
    }
}

/// The issue or pull request an event happened in.
#[derive(Clone, Debug)]
pub struct Subject {
    pub kind: SubjectKind,
    /// The row id — what `subject_id` and the subscriptions key on.
    pub id: i64,
    /// The repository-local number, for the title and the link.
    pub number: i64,
    pub repo_id: i64,
    pub title: String,
}

/// Why somebody is told — ordered from the weakest to the strongest, so the
/// strongest of several wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// They follow the thread: they opened it, commented, or subscribed.
    Participating,
    Mention,
    CiFailed,
    Assigned,
    ReviewRequested,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Participating => "participating",
            Self::Mention => "mention",
            Self::CiFailed => "ci_failed",
            Self::Assigned => "assigned",
            Self::ReviewRequested => "review_requested",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "participating" => Self::Participating,
            "mention" => Self::Mention,
            "ci_failed" => Self::CiFailed,
            "assigned" => Self::Assigned,
            "review_requested" => Self::ReviewRequested,
            _ => return None,
        })
    }

    /// Whether `settings` ask for this reason by mail too.
    pub fn mailed(self, settings: &Settings) -> bool {
        match self {
            Self::Participating => settings.email_participating,
            Self::Mention => settings.email_mention,
            Self::CiFailed => settings.email_ci_failed,
            Self::Assigned => settings.email_assigned,
            Self::ReviewRequested => settings.email_review_requested,
        }
    }

    /// Why the thread subscription that this reason creates exists.
    fn subscription_reason(self) -> &'static str {
        match self {
            Self::Participating => "commented",
            Self::Mention => "mention",
            Self::CiFailed => "author",
            Self::Assigned => "assigned",
            Self::ReviewRequested => "review_requested",
        }
    }
}

/// One thing that happened in a thread.
#[derive(Clone, Debug)]
pub struct ThreadEvent {
    pub subject: Subject,
    /// Who did it; `None` for the server itself (a CI run).
    pub actor_id: Option<i64>,
    /// What the actor did, as the subscribers read it after the actor's name:
    /// `commented`, `opened this issue`, `approved`. For an event without an
    /// actor it is the whole sentence.
    pub action: String,
    /// Text whose `@names` are mentions — a body or a comment. Never stored.
    pub mention_text: Option<String>,
    /// Accounts the event is addressed to, with why.
    pub direct: Vec<(i64, Reason)>,
    /// Whether the thread's subscribers hear about it.
    pub to_subscribers: bool,
    /// Subscribe the actor, and why — `author`, `commented`.
    pub actor_subscribes_as: Option<&'static str>,
}

impl ThreadEvent {
    pub fn new(subject: Subject, actor_id: Option<i64>, action: impl Into<String>) -> Self {
        Self {
            subject,
            actor_id,
            action: action.into(),
            mention_text: None,
            direct: Vec::new(),
            to_subscribers: false,
            actor_subscribes_as: None,
        }
    }

    pub fn mentions_in(mut self, text: impl Into<String>) -> Self {
        self.mention_text = Some(text.into());
        self
    }

    pub fn to(mut self, user_id: i64, reason: Reason) -> Self {
        self.direct.push((user_id, reason));
        self
    }

    pub fn to_subscribers(mut self) -> Self {
        self.to_subscribers = true;
        self
    }

    pub fn actor_subscribes(mut self, reason: &'static str) -> Self {
        self.actor_subscribes_as = Some(reason);
        self
    }
}

/// How many distinct `@names` one text may notify. A comment that pastes a
/// member list is not a reason to walk the user table a thousand times.
const MENTION_LIMIT: usize = 50;

/// The `@names` in `text`, in order, without repeats.
///
/// A name is what an account name may be — ASCII letters, digits, `-` and `_`
/// — preceded by the start of the text or by something that cannot be part of
/// a name or an address, so `alice@example.com` mentions nobody. Fenced code
/// blocks and inline code are skipped: a pasted log or a decorator is not a
/// mention.
pub fn mentioned_usernames(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut in_code = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '`' {
                in_code = !in_code;
                i += 1;
                continue;
            }
            let boundary = i == 0 || {
                let before = chars[i - 1];
                !(before.is_alphanumeric() || matches!(before, '-' | '_' | '.' | '@' | '/'))
            };
            if c == '@' && !in_code && boundary {
                let start = i + 1;
                let mut end = start;
                while end < chars.len()
                    && (chars[end].is_ascii_alphanumeric() || matches!(chars[end], '-' | '_'))
                {
                    end += 1;
                }
                // `@org/team` names a team, which this does not expand.
                let team = chars.get(end) == Some(&'/');
                if end > start && !team {
                    let name: String = chars[start..end].iter().collect();
                    if !names.contains(&name) {
                        names.push(name);
                        if names.len() == MENTION_LIMIT {
                            return names;
                        }
                    }
                }
                i = end.max(i + 1);
                continue;
            }
            i += 1;
        }
    }
    names
}

/// Deliver `event` off the request path, through the delivery tracker that
/// graceful shutdown drains. A failure is logged with the subject; the
/// operation that caused the event has already committed and stays done.
pub fn spawn(db: &DatabaseConnection, event: ThreadEvent) {
    let db = db.clone();
    crate::task_tracker::delivery_tracker().spawn(async move {
        if let Err(error) = deliver(&db, &event).await {
            tracing::warn!(
                repo_id = event.subject.repo_id,
                subject_type = event.subject.kind.as_str(),
                subject_id = event.subject.id,
                error = %format!("{error:#}"),
                "thread notification was not delivered"
            );
        }
    });
}

/// Who heard about an event — for tests and logs.
#[derive(Debug, Default)]
pub struct Delivered {
    pub notified: Vec<(i64, Reason)>,
    pub mailed: Vec<i64>,
}

/// Deliver `event` now. See the module note for the order of things.
pub async fn deliver(db: &DatabaseConnection, event: &ThreadEvent) -> Result<Delivered> {
    let subject = &event.subject;
    let subject_type = subject.kind.as_str();
    let Some(repo) = rg_db::ops::repo_ops::find_by_id(db, subject.repo_id).await? else {
        return Ok(Delivered::default());
    };
    let owner = repository_owner_name(db, &repo).await?;
    let link = format!(
        "/{owner}/{}/{}/{}",
        repo.name,
        subject.kind.path_segment(),
        subject.number
    );
    let title = format!(
        "{owner}/{} #{}: {}",
        repo.name, subject.number, subject.title
    );
    let actor_name = match event.actor_id {
        Some(actor_id) => {
            super::best_effort_user_by_id(db, actor_id, subject.repo_id, subject_type, "actor")
                .await
                .map(|user| user.username)
        }
        None => None,
    };

    if let (Some(actor_id), Some(reason)) = (event.actor_id, event.actor_subscribes_as) {
        thread_subscription_ops::subscribe_if_absent(
            db,
            actor_id,
            subject.repo_id,
            subject_type,
            subject.id,
            reason,
        )
        .await?;
    }

    let mut recipients: BTreeMap<i64, Reason> = BTreeMap::new();
    let mut add = |user_id: i64, reason: Reason| {
        let entry = recipients.entry(user_id).or_insert(reason);
        if reason > *entry {
            *entry = reason;
        }
    };
    for &(user_id, reason) in &event.direct {
        add(user_id, reason);
    }
    if let Some(text) = event.mention_text.as_deref() {
        for name in mentioned_usernames(text) {
            match rg_db::ops::user_ops::find_by_username(db, &name).await? {
                Some(user) if user.is_active && user.deleted_at.is_none() => {
                    add(user.id, Reason::Mention)
                }
                _ => {}
            }
        }
    }
    // Addressed people follow the thread from here on — the reviewer sees the
    // replies to their review, the assignee the discussion of their task.
    for (&user_id, &reason) in recipients.iter() {
        if Some(user_id) == event.actor_id {
            continue;
        }
        thread_subscription_ops::subscribe_if_absent(
            db,
            user_id,
            subject.repo_id,
            subject_type,
            subject.id,
            reason.subscription_reason(),
        )
        .await?;
    }
    if event.to_subscribers {
        for user_id in
            thread_subscription_ops::list_subscribers(db, subject_type, subject.id).await?
        {
            recipients.entry(user_id).or_insert(Reason::Participating);
        }
    }
    if let Some(actor_id) = event.actor_id {
        recipients.remove(&actor_id);
    }

    let mut delivered = Delivered::default();
    for (user_id, reason) in recipients {
        let Some(user) =
            super::best_effort_user_by_id(db, user_id, subject.repo_id, subject_type, "recipient")
                .await
        else {
            continue;
        };
        if !user.is_active || user.deleted_at.is_some() {
            continue;
        }
        match crate::repo::service::can_read_repo(db, &repo, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                // Fail closed, as the watch fan-out does: an unanswered read
                // check must not become a delivered notification.
                tracing::warn!(
                    user_id,
                    repo_id = subject.repo_id,
                    error = %format!("{error:#}"),
                    "thread notification recipient skipped: read check failed"
                );
                continue;
            }
        }
        let body = describe(event, reason, actor_name.as_deref());
        let settings = notification_setting_ops::get(db, user_id).await?;
        let email = reason.mailed(&settings) && !user.is_bot() && !user.email.trim().is_empty();
        let note = notification_ops::ThreadNotification {
            user_id,
            repo_id: subject.repo_id,
            subject_type,
            subject_id: subject.id,
            reason: reason.as_str(),
            title: &title,
            body: Some(&body),
            link: &link,
            email,
        };
        let folded =
            match notification_ops::find_unread_for_subject(db, user_id, subject_type, subject.id)
                .await?
            {
                Some(existing) => {
                    let stronger = existing
                        .reason
                        .as_deref()
                        .and_then(Reason::parse)
                        .is_none_or(|held| reason > held);
                    notification_ops::fold_into(
                        db,
                        existing.id,
                        &note,
                        stronger.then_some(reason.as_str()),
                    )
                    .await?
                }
                None => false,
            };
        if !folded {
            notification_ops::create_for_subject(db, &note).await?;
        }
        delivered.notified.push((user_id, reason));
        if email {
            delivered.mailed.push(user_id);
        }
    }
    if !delivered.mailed.is_empty() {
        super::mail::wake();
    }
    Ok(delivered)
}

/// The line a recipient reads under the title.
fn describe(event: &ThreadEvent, reason: Reason, actor: Option<&str>) -> String {
    let noun = event.subject.kind.noun();
    match (reason, actor) {
        (Reason::ReviewRequested, Some(actor)) => format!("{actor} requested your review"),
        (Reason::ReviewRequested, None) => "Your review was requested".to_string(),
        (Reason::Assigned, Some(actor)) => format!("{actor} assigned this {noun} to you"),
        (Reason::Assigned, None) => format!("This {noun} was assigned to you"),
        (Reason::Mention, Some(actor)) => format!("{actor} mentioned you"),
        (Reason::Mention, None) => "You were mentioned".to_string(),
        (_, Some(actor)) => format!("{actor} {}", event.action),
        (_, None) => event.action.clone(),
    }
}

/// The name a repository's URL starts with: its organization's, or its owning
/// account's.
pub(crate) async fn repository_owner_name(
    db: &DatabaseConnection,
    repo: &rg_db::entities::repository::Model,
) -> Result<String> {
    if let Some(org_id) = repo.org_id {
        if let Some(org) = rg_db::ops::org_ops::get_org(db, org_id).await? {
            return Ok(org.name);
        }
    }
    rg_db::ops::user_ops::find_by_id(db, repo.owner_id)
        .await?
        .map(|user| user.username)
        .ok_or_else(|| anyhow::anyhow!("repository {} has no owner row", repo.id))
}

/// Remove what pointed at a subject that no longer exists: its notifications
/// and its subscriptions.
pub async fn forget_subject(
    db: &DatabaseConnection,
    kind: SubjectKind,
    subject_id: i64,
) -> Result<()> {
    notification_ops::delete_for_subject(db, kind.as_str(), subject_id).await?;
    thread_subscription_ops::delete_for_subject(db, kind.as_str(), subject_id).await?;
    Ok(())
}

/// The caller's subscription to a subject: `Some(true)` subscribed,
/// `Some(false)` explicitly unsubscribed, `None` never decided.
pub async fn subscription(
    db: &DatabaseConnection,
    user_id: i64,
    kind: SubjectKind,
    subject_id: i64,
) -> Result<Option<rg_db::entities::thread_subscription::Model>> {
    thread_subscription_ops::find(db, user_id, kind.as_str(), subject_id).await
}

/// Subscribe to, or unsubscribe from, a subject by choice.
pub async fn set_subscription(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    kind: SubjectKind,
    subject_id: i64,
    subscribed: bool,
) -> Result<rg_db::entities::thread_subscription::Model> {
    thread_subscription_ops::set(db, user_id, repo_id, kind.as_str(), subject_id, subscribed).await
}

/// Tell the authors of the open pull requests a failed pipeline tested that
/// their CI failed. Detached; called where a pipeline is settled `failed` —
/// the internal runner, an external runner's last job, a configuration the
/// engine refused.
pub fn notify_ci_failed(db: &DatabaseConnection, pipeline_id: i64) {
    let db = db.clone();
    crate::task_tracker::delivery_tracker().spawn(async move {
        if let Err(error) = deliver_ci_failed(&db, pipeline_id).await {
            tracing::warn!(
                pipeline_id,
                error = %format!("{error:#}"),
                "CI failure notification was not delivered"
            );
        }
    });
}

/// [`notify_ci_failed`], awaited: how many authors were told.
pub async fn deliver_ci_failed(db: &DatabaseConnection, pipeline_id: i64) -> Result<usize> {
    let Some(pipeline) = rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id).await? else {
        return Ok(0);
    };
    if pipeline.status != "failed" {
        return Ok(0);
    }
    let short_sha: String = pipeline.commit_sha.chars().take(7).collect();
    let mut told = 0;
    for pr in rg_db::ops::pull_request_ops::list_open_for_pipeline_commit(
        db,
        pipeline.repo_id,
        &pipeline.commit_sha,
    )
    .await?
    {
        let event = ThreadEvent::new(
            crate::pull_request::service::pr_subject(&pr),
            None,
            format!("CI pipeline #{pipeline_id} failed on {short_sha}"),
        )
        .to(pr.author_id, Reason::CiFailed);
        told += deliver(db, &event).await?.notified.len();
    }
    Ok(told)
}

#[cfg(test)]
mod tests {
    use super::mentioned_usernames;

    #[test]
    fn mentions_are_names_after_a_boundary_outside_code() {
        let text = "Thanks @alice and @bob-2, cc @alice again.\n\
                    Mail alice@example.com or see `@decorator` and @org/team.\n\
                    ```\n@inside_fence\n```\n\
                    (@carol_x) @";
        assert_eq!(mentioned_usernames(text), vec!["alice", "bob-2", "carol_x"]);
    }

    #[test]
    fn a_long_list_is_capped() {
        let text: String = (0..80).map(|i| format!("@user{i} ")).collect();
        assert_eq!(mentioned_usernames(&text).len(), super::MENTION_LIMIT);
    }
}
