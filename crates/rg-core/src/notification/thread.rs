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

    let recipients: Vec<(i64, Reason)> = recipients.into_iter().collect();
    let text = DeliveryText {
        title: &title,
        link: &link,
        actor: actor_name.as_deref(),
    };
    let mut delivered = Delivered::default();
    for page in recipients.chunks(THREAD_FANOUT_PAGE) {
        deliver_page(db, &repo, event, page, &text, &mut delivered).await?;
    }
    if !delivered.mailed.is_empty() {
        super::mail::wake();
    }
    Ok(delivered)
}

/// How many recipients one page of [`deliver`] handles.
///
/// A page costs the same handful of statements however many people are on it —
/// one user query, one read check, one settings query, one unread lookup, and
/// one transaction holding a few grouped updates and one insert. It used to be
/// five queries and a write transaction of its own per recipient, so a comment
/// on a thread with a hundred subscribers took SQLite's write lock a hundred
/// times (card_ae49267ffd93). Fourteen bound values per inserted row keep a
/// page well inside every backend's parameter ceiling.
const THREAD_FANOUT_PAGE: usize = 500;

/// What every recipient of one event reads, whatever their reason.
struct DeliveryText<'a> {
    title: &'a str,
    link: &'a str,
    actor: Option<&'a str>,
}

/// Rows that one grouped update folds the event into: everyone in a group is
/// told the same thing for the same reason, and mailed or not alike.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FoldGroup {
    reason: Reason,
    /// The stored reason is weaker and is replaced.
    stronger: bool,
    email: bool,
}

/// Deliver `event` to one page of its recipients, in recipient order.
async fn deliver_page(
    db: &DatabaseConnection,
    repo: &rg_db::entities::repository::Model,
    event: &ThreadEvent,
    page: &[(i64, Reason)],
    text: &DeliveryText<'_>,
    delivered: &mut Delivered,
) -> Result<()> {
    use sea_orm::TransactionTrait;

    let subject = &event.subject;
    let subject_type = subject.kind.as_str();
    let ids: Vec<i64> = page.iter().map(|&(user_id, _)| user_id).collect();
    let users = match rg_db::ops::user_ops::find_by_ids(db, &ids).await {
        Ok(users) => users,
        Err(error) => {
            // The event has committed and stays done; this page is what is lost,
            // and the log says how much of it.
            tracing::warn!(
                repo_id = subject.repo_id,
                subject_type,
                subject_id = subject.id,
                skipped = ids.len(),
                error = %format!("{error:#}"),
                "thread notification recipients skipped: user lookup failed"
            );
            return Ok(());
        }
    };
    let users: std::collections::HashMap<i64, rg_db::entities::user::Model> = users
        .into_iter()
        .filter(|user| user.is_active && user.deleted_at.is_none())
        .map(|user| (user.id, user))
        .collect();
    let active: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|user_id| users.contains_key(user_id))
        .collect();
    let readers = match crate::repo::service::readers_among(db, repo, &active).await {
        Ok(readers) => readers,
        Err(error) => {
            // Fail closed, as the watch fan-out does: an unanswered read check
            // must not become a delivered notification.
            tracing::warn!(
                repo_id = subject.repo_id,
                subject_type,
                subject_id = subject.id,
                skipped = active.len(),
                error = %format!("{error:#}"),
                "thread notification recipients skipped: read check failed"
            );
            return Ok(());
        }
    };
    let recipients: Vec<(&rg_db::entities::user::Model, Reason)> = page
        .iter()
        .filter(|(user_id, _)| readers.contains(user_id))
        .filter_map(|(user_id, reason)| users.get(user_id).map(|user| (user, *reason)))
        .collect();
    if recipients.is_empty() {
        return Ok(());
    }
    let recipient_ids: Vec<i64> = recipients.iter().map(|(user, _)| user.id).collect();
    let settings = notification_setting_ops::get_many(db, &recipient_ids).await?;
    let unread = notification_ops::find_unread_for_subject_among(
        db,
        &recipient_ids,
        subject_type,
        subject.id,
    )
    .await?;

    let bodies: BTreeMap<Reason, String> = recipients
        .iter()
        .map(|&(_, reason)| (reason, describe(event, reason, text.actor)))
        .collect();
    let note = |user_id: i64, reason: Reason, email: bool| notification_ops::ThreadNotification {
        user_id,
        repo_id: subject.repo_id,
        subject_type,
        subject_id: subject.id,
        reason: reason.as_str(),
        title: text.title,
        body: bodies.get(&reason).map(String::as_str),
        link: text.link,
        email,
    };

    let mut folds: BTreeMap<FoldGroup, Vec<i64>> = BTreeMap::new();
    let mut inserts = Vec::new();
    let mut outcome = Vec::with_capacity(recipients.len());
    for &(user, reason) in &recipients {
        let email = settings
            .get(&user.id)
            .is_some_and(|settings| reason.mailed(settings))
            && !user.is_bot()
            && !user.email.trim().is_empty();
        match unread.get(&user.id) {
            Some(existing) => {
                let stronger = existing
                    .reason
                    .as_deref()
                    .and_then(Reason::parse)
                    .is_none_or(|held| reason > held);
                folds
                    .entry(FoldGroup {
                        reason,
                        stronger,
                        email,
                    })
                    .or_default()
                    .push(existing.id);
            }
            None => inserts.push(note(user.id, reason, email)),
        }
        outcome.push((user.id, reason, email));
    }

    let transaction = db.begin().await?;
    for (group, row_ids) in &folds {
        let folded_note = note(0, group.reason, group.email);
        let folded = notification_ops::fold_many(
            &transaction,
            row_ids,
            &folded_note,
            group.stronger.then_some(group.reason.as_str()),
        )
        .await?;
        if usize::try_from(folded).unwrap_or(usize::MAX) < row_ids.len() {
            // Read between the lookup and the update: that recipient's row is
            // done with, and this event needs a row of its own.
            for user_id in notification_ops::read_among(&transaction, row_ids).await? {
                inserts.push(note(user_id, group.reason, group.email));
            }
        }
    }
    notification_ops::create_for_subjects(&transaction, &inserts).await?;
    transaction.commit().await?;

    for (user_id, reason, email) in outcome {
        delivered.notified.push((user_id, reason));
        if email {
            delivered.mailed.push(user_id);
        }
    }
    Ok(())
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

    struct ThreadCost {
        statements: usize,
        notified: usize,
        mailed: usize,
        rows: usize,
        folded_titles: usize,
    }

    /// Statements one comment on an issue of a private repository sends to its
    /// `subscribers` (collaborators, every other one holding an unread row about
    /// the issue already, the first one declining mail), plus one subscriber who lost
    /// access.
    async fn thread_cost(subscribers: usize) -> ThreadCost {
        use super::{deliver, Reason, Subject, SubjectKind, ThreadEvent};
        use rg_db::ops::notification_ops::{self, ThreadNotification};
        use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        const SUBJECT_ID: i64 = 77;
        let mut db = crate::test_support::migrated_memory_database().await;
        let owner = rg_db::ops::user_ops::create_user(&db, "thr-owner", "thr-owner@x.test", "", "")
            .await
            .expect("create owner");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner.id),
                name: Set("threaded".to_string()),
                is_private: Set(true),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create repo");
        let subscribe = |user_id: i64| {
            let db = db.clone();
            async move {
                rg_db::ops::thread_subscription_ops::set(
                    &db, user_id, repo.id, "issue", SUBJECT_ID, true,
                )
                .await
                .expect("subscribe");
            }
        };
        for index in 0..subscribers {
            let user = rg_db::ops::user_ops::create_user(
                &db,
                &format!("thr-{index}"),
                &format!("thr-{index}@x.test"),
                "",
                "",
            )
            .await
            .expect("create subscriber");
            rg_db::ops::repo_collaborator_ops::create(
                &db,
                rg_db::entities::repo_collaborator::ActiveModel {
                    repo_id: Set(repo.id),
                    user_id: Set(user.id),
                    permission: Set("read".to_string()),
                    created_at: Set(now),
                    ..Default::default()
                },
            )
            .await
            .expect("add collaborator");
            subscribe(user.id).await;
            if index % 2 == 1 {
                notification_ops::create_for_subjects(
                    &db,
                    &[ThreadNotification {
                        user_id: user.id,
                        repo_id: repo.id,
                        subject_type: "issue",
                        subject_id: SUBJECT_ID,
                        reason: "participating",
                        title: "an earlier event",
                        body: None,
                        link: "/thr-owner/threaded/issues/1",
                        email: false,
                    }],
                )
                .await
                .expect("seed an unread row");
            }
            if index == 0 {
                let mut settings =
                    rg_db::entities::notification_setting::Model::defaults_for(user.id);
                settings.email_participating = false;
                rg_db::ops::notification_setting_ops::put(&db, settings)
                    .await
                    .expect("decline mail");
            }
        }
        let outsider = rg_db::ops::user_ops::create_user(&db, "thr-out", "thr-out@x.test", "", "")
            .await
            .expect("create outsider");
        subscribe(outsider.id).await;

        let statements = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&statements);
        db.set_metric_callback(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let event = ThreadEvent::new(
            Subject {
                kind: SubjectKind::Issue,
                id: SUBJECT_ID,
                number: 1,
                repo_id: repo.id,
                title: "Busy thread".to_string(),
            },
            Some(owner.id),
            "commented",
        )
        .to_subscribers();
        let delivered = deliver(&db, &event).await.expect("deliver");
        let statements = statements.load(Ordering::SeqCst);

        assert!(delivered
            .notified
            .iter()
            .all(|&(user_id, reason)| user_id != outsider.id && reason == Reason::Participating));
        let rows = rg_db::entities::notification::Entity::find()
            .count(&db)
            .await
            .expect("count notifications") as usize;
        let folded_titles = rg_db::entities::notification::Entity::find()
            .filter(rg_db::entities::notification::Column::Title.contains("Busy thread"))
            .count(&db)
            .await
            .expect("count updated rows") as usize;
        ThreadCost {
            statements,
            notified: delivered.notified.len(),
            mailed: delivered.mailed.len(),
            rows,
            folded_titles,
        }
    }

    /// card_ae49267ffd93: every recipient of a thread event used to cost a user
    /// lookup, a read check, a settings read, an unread lookup and its own write
    /// — a comment on a thread with a hundred subscribers took the SQLite write
    /// lock a hundred times. A page is now a fixed handful of statements, and
    /// the outcome is the same: everyone who may read is told once, an unread
    /// row is folded into rather than doubled, mail follows the settings, and
    /// the subscriber who lost access hears nothing.
    #[tokio::test]
    async fn a_thread_event_costs_the_same_statements_for_three_subscribers_or_forty() {
        let few = thread_cost(3).await;
        let many = thread_cost(40).await;
        for (subscribers, cost) in [(3, &few), (40, &many)] {
            assert_eq!(cost.notified, subscribers);
            assert_eq!(
                cost.rows, subscribers,
                "an unread row was doubled, not folded"
            );
            assert_eq!(
                cost.folded_titles, subscribers,
                "a row does not carry the event"
            );
            assert_eq!(
                cost.mailed,
                subscribers - 1,
                "mail does not follow the settings"
            );
        }
        assert_eq!(
            few.statements, many.statements,
            "an event for 3 subscribers sent {} statements, for 40 sent {}",
            few.statements, many.statements
        );
    }
}
