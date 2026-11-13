use super::*;
use rg_db::{notification::Channels, ops::notification_setting_ops};
use sea_orm::{ActiveModelTrait, ColumnTrait, QueryFilter, Set};
use std::sync::Mutex;

const TOKEN: &str = "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi";
const KEY: &str = "dispatch-fixture-key";

#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<(String, String)>>,
    fail: Option<Failure>,
}
impl Sender for Recorder {
    fn send<'a>(
        &'a self,
        target: &'a Target,
        message: &'a template::Message,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), Failure>> + Send + 'a>> {
        Box::pin(async move {
            let address = match target {
                Target::Email(address) => format!("email:{address}"),
                Target::Telegram(destination) => {
                    let (token, chat, topic) = destination.fixture_parts();
                    assert_eq!(token, TOKEN);
                    format!("telegram:{chat}:{topic:?}")
                }
            };
            self.sent
                .lock()
                .unwrap()
                .push((address, message.text.clone()));
            self.fail.map_or(Ok(()), Err)
        })
    }
}
fn config(smtp: bool) -> Config<'static> {
    Config {
        encryption_key: KEY,
        base_url: Some("https://forge.invalid"),
        smtp_available: smtp,
    }
}
async fn fixture() -> (DatabaseConnection, user::Model) {
    let db = rg_db::connect_with_pool(
        "sqlite::memory:",
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        rg_db::DEFAULT_MAX_CONNECTIONS,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    let user = rg_db::ops::user_ops::create_user(
        &db,
        "dispatch",
        "old@example.invalid",
        "fixture",
        "Dispatch",
    )
    .await
    .unwrap();
    settings::save(
        &db,
        user.id,
        &settings::Patch {
            telegram: Some(settings::TelegramChange::Set {
                token: Some(TOKEN.into()),
                chat_id: -1001234,
                message_thread_id: Some(42),
            }),
            ..Default::default()
        },
        &settings::SaveContext {
            encryption_secret: KEY,
            smtp_available: false,
            metadata: Metadata::default(),
        },
        |_, _| Ok(()),
    )
    .await
    .unwrap();
    let mut profile = queue::profile(&db, user.id).await.unwrap();
    profile.locale = "ru".into();
    profile.choices = serde_json::to_string(
        &Event::ALL
            .iter()
            .map(|e| {
                (
                    *e,
                    Channels {
                        email: true,
                        telegram: true,
                    },
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    queue::put_profile(&db, profile).await.unwrap();
    (db, user)
}
async fn enqueue(
    db: &DatabaseConnection,
    user_id: i64,
    event: Event,
) -> Vec<notification_delivery::Model> {
    let mut event = queue::QueueEvent::security(user_id, event, Some("Alice <literal>".into()));
    event.metadata.ip = Some("198.51.100.3".into());
    queue::enqueue(db, &event).await.unwrap();
    queue::event_rows(db, user_id, &event.id).await.unwrap()
}

#[tokio::test]
async fn smtp_disabled_does_not_prevent_telegram_and_latest_saved_language_wins() {
    let (db, user) = fixture().await;
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    let mut profile = queue::profile(&db, user.id).await.unwrap();
    profile.locale = "zh-CN".into();
    queue::put_profile(&db, profile).await.unwrap();
    let out = Recorder::default();
    pass(&db, &config(false), &out).await.unwrap();
    {
        let sent = out.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "telegram:-1001234:Some(42)");
        assert!(sent[0].1.contains("新设备登录") && sent[0].1.contains("Alice <literal>"));
    }
    for row in rows {
        let row = notification_delivery::Entity::find_by_id(row.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.status,
            if row.channel == "email" {
                "unavailable"
            } else {
                "sent"
            }
        );
        assert_eq!(row.metadata, "{}");
    }
    assert!(
        queue::profile(&db, user.id)
            .await
            .unwrap()
            .telegram_confirmed
    );
}

#[tokio::test]
async fn ordinary_jobs_are_cancelled_for_changed_recipient_or_preference() {
    let (db, user) = fixture().await;
    let rows = enqueue(&db, user.id, Event::Mention).await;
    let mut prefs = notification_setting_ops::get(&db, user.id).await.unwrap();
    prefs.email_mention = false;
    notification_setting_ops::put(&db, prefs).await.unwrap();
    let mut profile = queue::profile(&db, user.id).await.unwrap();
    profile.generation += 1;
    profile.chat_id = Some(-1009999);
    queue::put_profile(&db, profile).await.unwrap();
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    assert!(out.sent.lock().unwrap().is_empty());
    for row in rows {
        assert_eq!(
            notification_delivery::Entity::find_by_id(row.id)
                .one(&db)
                .await
                .unwrap()
                .unwrap()
                .status,
            "cancelled"
        );
    }
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    let mut changed: user::ActiveModel = user.into();
    changed.email = Set("new@example.invalid".into());
    changed.update(&db).await.unwrap();
    let email = rows.iter().find(|r| r.channel == "email").unwrap();
    deliver_one(&db, email.id, &config(true), &out)
        .await
        .unwrap();
    assert!(out.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn old_confirmed_destination_survives_mute_locale_change_and_logout_but_is_minimal() {
    let (db, user) = fixture().await;
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    settings::save(
        &db,
        user.id,
        &settings::Patch {
            telegram: Some(settings::TelegramChange::Remove),
            locale: Some(Locale::ZhCn),
            ..Default::default()
        },
        &settings::SaveContext {
            encryption_secret: KEY,
            smtp_available: false,
            metadata: Metadata {
                actor: Some("Private actor".into()),
                ip: Some("198.51.100.99".into()),
                device: Some("Private UA".into()),
                ..Default::default()
            },
        },
        |_, _| Ok(()),
    )
    .await
    .unwrap();
    let mut logout: user::ActiveModel = user.clone().into();
    logout.session_version = Set(user.session_version + 1);
    logout.update(&db).await.unwrap();
    let out = Recorder::default();
    pass(&db, &config(false), &out).await.unwrap();
    {
        let sent = out.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "telegram:-1001234:Some(42)");
        assert!(sent[0].1.contains("Настройки уведомлений изменены"));
        for excluded in [
            TOKEN,
            "Private actor",
            "198.51.100.99",
            "Private UA",
            "-1001234",
        ] {
            assert!(!sent[0].1.contains(excluded));
        }
        assert!(sent[0]
            .1
            .contains("https://forge.invalid/settings/notifications"));
    }
    let rows = notification_delivery::Entity::find()
        .all(&db)
        .await
        .unwrap();
    assert!(rows.iter().all(
        |r| r.previous_destination_encrypted.is_none() && r.previous_token_encrypted.is_none()
    ));
}

#[tokio::test]
async fn retry_after_respects_backoff_fixed_expiry_and_six_attempt_limit() {
    let (db, user) = fixture().await;
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    let telegram = rows.iter().find(|r| r.channel == "telegram").unwrap();
    let error = Failure::from(telegram::Error::RateLimited { retry_after: 1234 });
    let out = Recorder {
        fail: Some(error),
        ..Default::default()
    };
    let before = chrono::Utc::now();
    deliver_one(&db, telegram.id, &config(false), &out)
        .await
        .unwrap();
    let held = notification_delivery::Entity::find_by_id(telegram.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held.status, "queued");
    assert_eq!(held.attempts, 1);
    assert!(held.next_attempt_at >= before + chrono::Duration::seconds(1234));
    assert_eq!(held.expires_at, telegram.expires_at);
    let mut synthetic = held.clone();
    for seconds in [u32::MAX as u64, u64::MAX] {
        assert!(retry_at(
            &synthetic,
            Failure {
                code: "fixture",
                retry_after: Some(Duration::from_secs(seconds))
            },
            before
        )
        .is_none());
    }
    let retry = retry_at(&synthetic, Failure::from(telegram::Error::Network), before).unwrap();
    assert!(retry >= before + chrono::Duration::seconds(60));
    synthetic.attempts = 6;
    assert!(retry_at(&synthetic, error, before).is_none());
    for error in [
        telegram::Error::Unauthorized,
        telegram::Error::Forbidden,
        telegram::Error::InvalidDestination,
    ] {
        assert!(retry_at(&held, Failure::from(error), before).is_none());
    }
    let mut due: notification_delivery::ActiveModel = held.into();
    due.next_attempt_at = Set(before);
    due.update(&db).await.unwrap();
    let permanent = Recorder {
        fail: Some(Failure::from(telegram::Error::Unauthorized)),
        ..Default::default()
    };
    deliver_one(&db, telegram.id, &config(false), &permanent)
        .await
        .unwrap();
    let done = notification_delivery::Entity::find_by_id(telegram.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "failed");
    assert_eq!(done.metadata, "{}");
}

#[tokio::test]
async fn removed_accounts_and_missing_private_repositories_never_send() {
    let (db, user) = fixture().await;
    let mut event =
        queue::QueueEvent::security(user.id, Event::DeployKeyAdded, Some("fixture".into()));
    event.repo_id = Some(9999);
    event.admin_only = true;
    queue::enqueue(&db, &event).await.unwrap();
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    assert!(out.sent.lock().unwrap().is_empty());
    assert!(queue::event_rows(&db, user.id, &event.id)
        .await
        .unwrap()
        .iter()
        .all(|r| r.last_error.as_deref() == Some("repository_unavailable")));
    enqueue(&db, user.id, Event::NewDevice).await;
    let mut retired: user::ActiveModel = user.into();
    retired.is_active = Set(false);
    retired.update(&db).await.unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    assert!(out.sent.lock().unwrap().is_empty());
    assert!(notification_delivery::Entity::find()
        .filter(notification_delivery::Column::Status.eq("queued"))
        .all(&db)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn real_thread_fanout_captures_both_channels_and_cancels_removed_or_folded_sources() {
    use super::super::thread::{self, Reason, Subject, SubjectKind, ThreadEvent};
    let (db, user) = fixture().await;
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(user.id),
            name: Set("notices".into()),
            is_private: Set(true),
            default_branch: Set("main".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let reasons = [
        Reason::ReviewRequested,
        Reason::Mention,
        Reason::Assigned,
        Reason::CiFailed,
        Reason::Participating,
    ];
    for (i, reason) in reasons.into_iter().enumerate() {
        let subject = Subject {
            kind: SubjectKind::Issue,
            id: i as i64 + 100,
            number: i as i64 + 1,
            repo_id: repo.id,
            title: format!("literal fixture {i}"),
        };
        thread::deliver(
            &db,
            &ThreadEvent::new(subject, None, "fixture action").to(user.id, reason),
        )
        .await
        .unwrap();
    }
    let original = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::EventId.starts_with("thread:"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(original.len(), 10);
    for reason in reasons {
        assert_eq!(
            original
                .iter()
                .filter(|row| row.event_key == reason.as_str())
                .count(),
            2
        );
    }
    let removed = rg_db::entities::notification::Entity::find()
        .filter(rg_db::entities::notification::Column::SubjectId.eq(100))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    rg_db::entities::notification::Entity::delete_by_id(removed.id)
        .exec(&db)
        .await
        .unwrap();
    // A folded thread replaces its pending text/version. Its earlier queued
    // payload cannot survive deletion or race the stronger latest reason.
    thread::deliver(
        &db,
        &ThreadEvent::new(
            Subject {
                kind: SubjectKind::Issue,
                id: 101,
                number: 2,
                repo_id: repo.id,
                title: "updated literal".into(),
            },
            None,
            "fixture action",
        )
        .to(user.id, Reason::Mention),
    )
    .await
    .unwrap();
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    {
        let sent = out.sent.lock().unwrap();
        assert_eq!(sent.len(), 8);
        assert!(!sent
            .iter()
            .any(|(_, text)| text.contains("literal fixture 0")
                || text.contains("literal fixture 1")));
        assert_eq!(
            sent.iter()
                .filter(|(target, _)| target.starts_with("telegram:"))
                .count(),
            4
        );
    }
    let cancelled = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::LastError.eq("thread_removed_or_superseded"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(cancelled.len(), 4);
    assert!(cancelled.iter().all(|row| row.metadata == "{}"));
}

#[tokio::test]
async fn legacy_pending_drain_never_redirects_unknown_payload_or_duplicates_durable_jobs() {
    use rg_db::{entities::notification, ops::notification_ops};
    let (db, user) = fixture().await;
    let note = notification_ops::ThreadNotification {
        user_id: user.id,
        repo_id: 0,
        subject_type: "issue",
        subject_id: 55,
        reason: "mention",
        title: "legacy private title",
        body: None,
        link: "/old/private/issues/1",
        email: true,
    };
    notification_ops::create_for_subjects(&db, &[note])
        .await
        .unwrap();
    let note = notification::Entity::find()
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(super::super::mail::drain_pending(&db).await.unwrap(), 1);
    assert_eq!(super::super::mail::drain_pending(&db).await.unwrap(), 0);
    let jobs = queue::event_rows(&db, user.id, &queue::thread_event_id(&note))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].last_error.as_deref(),
        Some("legacy_destination_unknown")
    );
    assert_eq!(jobs[0].metadata, "{}");
    assert!(jobs[0].previous_destination_encrypted.is_none());
    assert_eq!(
        notification::Entity::find_by_id(note.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .title,
        "legacy private title"
    );
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    assert!(out.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn revoked_repository_and_instance_grants_cancel_pending_delivery() {
    use rg_db::{
        entities::{repo_collaborator, repository},
        ops::{repo_collaborator_ops, repo_ops, user_ops},
    };
    let (db, user) = fixture().await;
    let owner = user_ops::create_user(&db, "other-owner", "owner@example.invalid", "fixture", "")
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let repo = repo_ops::create(
        &db,
        repository::ActiveModel {
            owner_id: Set(owner.id),
            name: Set("private-grants".into()),
            is_private: Set(true),
            default_branch: Set("main".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let grant = repo_collaborator_ops::create(
        &db,
        repo_collaborator::ActiveModel {
            repo_id: Set(repo.id),
            user_id: Set(user.id),
            permission: Set("admin".into()),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut admin =
        queue::QueueEvent::security(user.id, Event::DeployKeyAdded, Some("fixture".into()));
    admin.repo_id = Some(repo.id);
    admin.admin_only = true;
    queue::enqueue(&db, &admin).await.unwrap();
    repo_collaborator_ops::update_permission(&db, grant.id, "read".into())
        .await
        .unwrap()
        .unwrap();
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    let rows = queue::event_rows(&db, user.id, &admin.id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status == "cancelled"
        && row.last_error.as_deref() == Some("repository_access_removed")));
    let mut reader =
        queue::QueueEvent::security(user.id, Event::Mention, Some("private title".into()));
    reader.repo_id = Some(repo.id);
    queue::enqueue(&db, &reader).await.unwrap();
    repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, user.id)
        .await
        .unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    let rows = queue::event_rows(&db, user.id, &reader.id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.status == "cancelled" && row.metadata == "{}"));
    let mut instance: user::ActiveModel = user.clone().into();
    instance.is_admin = Set(true);
    instance.update(&db).await.unwrap();
    let mut runner =
        queue::QueueEvent::security(user.id, Event::RunnerTokenCreated, Some("runner".into()));
    runner.admin_only = true;
    queue::enqueue(&db, &runner).await.unwrap();
    let mut instance: user::ActiveModel = user.clone().into();
    instance.is_admin = Set(false);
    instance.update(&db).await.unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    let rows = queue::event_rows(&db, user.id, &runner.id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status == "cancelled"
        && row.last_error.as_deref() == Some("administrator_access_removed")));
    let sent = out.sent.lock().unwrap();
    // Three access mutations now notify the owner (email) and affected user
    // (email + Telegram). The revoked content/credential notices still cancel.
    assert_eq!(sent.len(), 9);
    assert!(sent
        .iter()
        .all(|(_, text)| text.contains("Repository access changed")
            || text.contains("Доступ к репозиторию изменён")));
    assert!(sent.iter().all(|(_, text)| !text.contains("private title")
        && !text.contains("fixture")
        && !text.contains("runner")));
}

#[tokio::test]
async fn organization_owner_copies_recheck_role_but_personal_revocation_notice_survives() {
    use rg_db::ops::{org_ops, user_ops};
    let (db, user) = fixture().await;
    let other = user_ops::create_user(&db, "next-owner", "next@example.invalid", "fixture", "Next")
        .await
        .unwrap();
    let org = org_ops::create_org(&db, "owner-copy", None, None, user.id, "private")
        .await
        .unwrap();
    org_ops::add_org_member(&db, org.id, other.id, "owner")
        .await
        .unwrap();
    org_ops::create_team(&db, org.id, "operators", None, "admin")
        .await
        .unwrap();
    org_ops::remove_org_member(&db, org.id, user.id)
        .await
        .unwrap();
    let pending = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user.id))
        .all(&db)
        .await
        .unwrap();
    let team: Vec<i64> = pending
        .iter()
        .filter(|row| row.metadata.contains("team_created"))
        .map(|row| row.id)
        .collect();
    let removal: Vec<i64> = pending
        .iter()
        .filter(|row| row.metadata.contains("org_member_removed"))
        .map(|row| row.id)
        .collect();
    assert_eq!(team.len(), 2);
    assert_eq!(removal.len(), 2);
    let out = Recorder::default();
    // Drain, as the dispatcher does: this recipient has more class-1 rows than
    // one page gives a single recipient, which is the S10 limit, not this
    // test's subject.
    drain(&db, &config(true), &out, &|| false).await.unwrap();
    for id in team {
        let row = notification_delivery::Entity::find_by_id(id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "cancelled");
        assert_eq!(
            row.last_error.as_deref(),
            Some("organization_access_removed")
        );
    }
    for id in removal {
        assert_eq!(
            notification_delivery::Entity::find_by_id(id)
                .one(&db)
                .await
                .unwrap()
                .unwrap()
                .status,
            "sent"
        );
    }
}

async fn status_of(db: &DatabaseConnection, rows: &[notification_delivery::Model]) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        out.push(
            notification_delivery::Entity::find_by_id(row.id)
                .one(db)
                .await
                .unwrap()
                .unwrap()
                .status,
        );
    }
    out
}

async fn second_user(db: &DatabaseConnection, name: &str) -> user::Model {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "x", name)
        .await
        .unwrap()
}

/// S10: a security notification queued behind a page's worth of activity is
/// delivered by the next pass, not after the activity backlog.
#[tokio::test]
async fn a_security_row_is_delivered_ahead_of_queued_activity() {
    let (db, user) = fixture().await;
    for _ in 0..30 {
        enqueue(&db, user.id, Event::Participating).await;
    }
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    pass(&db, &config(true), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(status_of(&db, &rows).await, vec!["sent", "sent"]);
}

/// One recipient's backlog in a class takes at most `PER_RECIPIENT` places of
/// a page; somebody else's row queued later is still in it.
#[tokio::test]
async fn one_recipient_cannot_hold_a_security_class() {
    let (db, user) = fixture().await;
    for _ in 0..50 {
        enqueue(&db, user.id, Event::NewDevice).await;
    }
    let other = second_user(&db, "bystander").await;
    let rows = enqueue(&db, other.id, Event::NewDevice).await;
    let out = Recorder::default();
    pass(&db, &config(true), &out).await.unwrap();
    assert_eq!(status_of(&db, &rows).await, vec!["sent"]);
    let flooder_sent = out
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|(target, _)| !target.contains("bystander"))
        .count();
    assert!(
        flooder_sent <= queue::PER_RECIPIENT as usize,
        "{flooder_sent}"
    );
}

/// V1: rows another member can cause (organization membership changes) do
/// not delay the victim's own account warnings.
#[tokio::test]
async fn shared_resource_floods_do_not_delay_account_warnings() {
    let (db, user) = fixture().await;
    for _ in 0..100 {
        enqueue(&db, user.id, Event::OrganizationAccessChanged).await;
    }
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    pass(&db, &config(true), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(status_of(&db, &rows).await, vec!["sent", "sent"]);
}

/// The dispatcher keeps passing while passes settle rows instead of sleeping
/// after each page; a row held by another worker does not keep it spinning.
#[tokio::test]
async fn draining_delivers_more_than_a_page_and_stops_on_held_rows() {
    let (db, user) = fixture().await;
    let mut rows = Vec::new();
    for _ in 0..60 {
        rows.extend(enqueue(&db, user.id, Event::Participating).await);
    }
    let held = rows.pop().unwrap();
    let mut lease: notification_delivery::ActiveModel = held.clone().into();
    lease.status = Set("inflight".into());
    lease.lease_token = Set(Some("another-worker".into()));
    lease.lease_until = Set(Some(chrono::Utc::now() + chrono::Duration::minutes(5)));
    lease.update(&db).await.unwrap();

    let settled = drain(&db, &config(true), &Recorder::default(), &|| false)
        .await
        .unwrap();
    assert_eq!(settled, rows.len());
    assert!(status_of(&db, &rows).await.iter().all(|s| s == "sent"));
    assert_eq!(status_of(&db, &[held]).await, vec!["inflight"]);
}

fn thread_subject(repo_id: i64) -> super::super::thread::Subject {
    super::super::thread::Subject {
        kind: super::super::thread::SubjectKind::Issue,
        id: 700,
        number: 7,
        repo_id,
        title: "folded subject".into(),
    }
}

async fn thread_repo(db: &DatabaseConnection, owner: &user::Model) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(owner.id),
            name: Set("folds".into()),
            is_private: Set(true),
            default_branch: Set("main".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

async fn queued_thread_rows(db: &DatabaseConnection) -> Vec<notification_delivery::Model> {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::EventId.starts_with("thread:"))
        .filter(notification_delivery::Column::Status.eq("queued"))
        .all(db)
        .await
        .unwrap()
}

/// A fold supersedes the queued rows of earlier versions when it is
/// enqueued: three events on one thread leave one version per channel.
#[tokio::test]
async fn folding_a_thread_cancels_its_queued_earlier_versions() {
    use super::super::thread::{self, Reason, ThreadEvent};
    let (db, user) = fixture().await;
    let repo_id = thread_repo(&db, &user).await;
    for action in ["first action", "second action", "third action"] {
        thread::deliver(
            &db,
            &ThreadEvent::new(thread_subject(repo_id), None, action)
                .to(user.id, Reason::Participating),
        )
        .await
        .unwrap();
    }
    assert!(queued_thread_rows(&db).await.len() <= 2);
    let superseded = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::LastError.eq("thread_removed_or_superseded"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(superseded.len(), 4);
    assert!(superseded
        .iter()
        .all(|row| row.metadata == "{}" && row.status == "cancelled"));
}

/// A version already held by a worker is not touched at enqueue; the
/// dispatcher stops it as superseded once its lease runs out.
#[tokio::test]
async fn an_in_flight_version_is_left_to_the_dispatcher() {
    use super::super::thread::{self, Reason, ThreadEvent};
    let (db, user) = fixture().await;
    let repo_id = thread_repo(&db, &user).await;
    thread::deliver(
        &db,
        &ThreadEvent::new(thread_subject(repo_id), None, "first action")
            .to(user.id, Reason::Participating),
    )
    .await
    .unwrap();
    let first = queued_thread_rows(&db).await;
    for row in &first {
        let mut held: notification_delivery::ActiveModel = row.clone().into();
        held.status = Set("inflight".into());
        held.lease_token = Set(Some("another-worker".into()));
        held.lease_until = Set(Some(chrono::Utc::now() + chrono::Duration::minutes(5)));
        held.update(&db).await.unwrap();
    }
    thread::deliver(
        &db,
        &ThreadEvent::new(thread_subject(repo_id), None, "second action")
            .to(user.id, Reason::Participating),
    )
    .await
    .unwrap();
    assert!(status_of(&db, &first).await.iter().all(|s| s == "inflight"));

    for row in &first {
        let mut expired: notification_delivery::ActiveModel = row.clone().into();
        expired.status = Set("inflight".into());
        expired.lease_token = Set(Some("another-worker".into()));
        expired.lease_until = Set(Some(chrono::Utc::now() - chrono::Duration::minutes(1)));
        expired.update(&db).await.unwrap();
    }
    pass(&db, &config(true), &Recorder::default())
        .await
        .unwrap();
    for row in &first {
        let stopped = notification_delivery::Entity::find_by_id(row.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.status, "cancelled");
        assert_eq!(
            stopped.last_error.as_deref(),
            Some("thread_removed_or_superseded")
        );
    }
}

/// Rendered into the warning's text, so the test finds it whatever the
/// recipient's language (the fixture saves `ru`).
const WARNING_MARKER: &str = "v3-warning-device";

/// Records sends and, on the third one, enqueues a security event — a
/// warning that arrives while a page of activity is being sent.
struct EnqueueOnThird {
    db: DatabaseConnection,
    user_id: i64,
    sent: Mutex<Vec<String>>,
}
impl Sender for EnqueueOnThird {
    fn send<'a>(
        &'a self,
        _target: &'a Target,
        message: &'a template::Message,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), Failure>> + Send + 'a>> {
        Box::pin(async move {
            let count = {
                let mut sent = self.sent.lock().unwrap();
                sent.push(message.text.clone());
                sent.len()
            };
            if count == 3 {
                let mut event = queue::QueueEvent::security(self.user_id, Event::NewDevice, None);
                event.metadata.device = Some(WARNING_MARKER.into());
                queue::enqueue(&self.db, &event).await.unwrap();
            }
            Ok(())
        })
    }
}

/// V3: rows of a page go out one by one, so a warning arriving mid-page waits
/// for at most one page of activity, not for a page sized for security.
#[tokio::test]
async fn a_warning_arriving_mid_page_waits_for_at_most_one_activity_page() {
    let (db, user) = fixture().await;
    for _ in 0..25 {
        enqueue(&db, user.id, Event::Participating).await;
    }
    let out = EnqueueOnThird {
        db: db.clone(),
        user_id: user.id,
        sent: Mutex::new(Vec::new()),
    };
    pass(&db, &config(true), &out).await.unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    let sent = out.sent.lock().unwrap();
    let first_warning = sent
        .iter()
        .position(|text| text.contains(WARNING_MARKER))
        .expect("the warning was delivered by the second pass");
    // The bound is the design's, written out: comparing with the constant
    // would follow it if someone raised it back to a full page.
    assert!(
        first_warning <= 10,
        "the warning waited behind {first_warning} activity sends"
    );
}

/// L28: rows of an account whose retirement has begun are neither sent nor
/// cancelled; an aborted retirement delivers them.
#[tokio::test]
async fn a_retiring_account_keeps_its_rows_and_an_abort_delivers_them() {
    let (db, user) = fixture().await;
    let rows = enqueue(&db, user.id, Event::NewDevice).await;
    assert!(rg_db::ops::user_ops::begin_user_retirement(&db, user.id)
        .await
        .unwrap());
    let out = Recorder::default();
    assert_eq!(pass(&db, &config(true), &out).await.unwrap(), 0);
    assert!(out.sent.lock().unwrap().is_empty());
    assert_eq!(status_of(&db, &rows).await, vec!["queued", "queued"]);

    rg_db::ops::user_ops::abort_user_retirement(&db, user.id)
        .await
        .unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    assert_eq!(status_of(&db, &rows).await, vec!["sent", "sent"]);
}

/// A retirement that begins between claim and send hands the row back with
/// the attempt the claim counted, so retirements started and aborted around a
/// warning cannot spend its six attempts.
#[tokio::test]
async fn a_row_claimed_before_retirement_is_handed_back_with_its_attempt() {
    let (db, user) = fixture().await;
    let row = enqueue(&db, user.id, Event::NewDevice).await.remove(0);
    let held = queue::claim(&db, row.id).await.unwrap().expect("claimed");
    assert_eq!(held.attempts, row.attempts + 1);
    assert!(rg_db::ops::user_ops::begin_user_retirement(&db, user.id)
        .await
        .unwrap());

    let before = chrono::Utc::now();
    assert!(settle(&db, held, &config(true), &Recorder::default())
        .await
        .unwrap());
    let back = notification_delivery::Entity::find_by_id(row.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.status, "queued");
    assert_eq!(back.attempts, row.attempts);
    assert!(back.lease_token.is_none());
    assert!(back.next_attempt_at >= before + chrono::Duration::seconds(55));
    assert!(back.next_attempt_at <= chrono::Utc::now() + chrono::Duration::seconds(65));
}

/// The other branches stay terminal: no account, a bot, a deactivated account.
#[tokio::test]
async fn missing_bot_and_deactivated_accounts_are_cancelled() {
    let (db, user) = fixture().await;
    let out = Recorder::default();

    let deactivated = enqueue(&db, user.id, Event::NewDevice).await;
    let mut account: user::ActiveModel = user::Entity::find_by_id(user.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap()
        .into();
    account.is_active = Set(false);
    account.update(&db).await.unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    assert_eq!(
        status_of(&db, &deactivated).await,
        vec!["cancelled", "cancelled"]
    );

    let owner = second_user(&db, "bot-owner").await;
    let bot = second_user(&db, "a-bot").await;
    let to_bot = enqueue(&db, bot.id, Event::NewDevice).await;
    let mut as_bot: user::ActiveModel = bot.clone().into();
    as_bot.bot_owner_id = Set(Some(owner.id));
    as_bot.update(&db).await.unwrap();
    pass(&db, &config(true), &out).await.unwrap();
    assert_eq!(status_of(&db, &to_bot).await, vec!["cancelled"]);

    // An account row gone between claim and send (a race with the final
    // delete, whose cascade then removes the row): stop it, there is nobody to
    // tell. The held copy points at an id no account has; the outcome is
    // written to the real row through its lease.
    let gone = second_user(&db, "gone").await;
    let orphan = enqueue(&db, gone.id, Event::NewDevice).await;
    let mut held = queue::claim(&db, orphan[0].id)
        .await
        .unwrap()
        .expect("claimed");
    held.user_id = i64::MAX;
    assert!(settle(&db, held, &config(true), &out).await.unwrap());
    let stopped = notification_delivery::Entity::find_by_id(orphan[0].id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, "cancelled");
    assert_eq!(stopped.last_error.as_deref(), Some("account_unavailable"));
    assert!(out.sent.lock().unwrap().is_empty());
}

/// A completed deletion removes the account's rows, warnings included.
#[tokio::test]
async fn a_completed_deletion_removes_the_rows_with_the_account() {
    use sea_orm::ConnectionTrait;
    let (db, _) = fixture().await;
    let leaving = second_user(&db, "leaving").await;
    let rows = enqueue(&db, leaving.id, Event::NewDevice).await;
    db.execute_unprepared(&format!("DELETE FROM users WHERE id = {}", leaving.id))
        .await
        .unwrap();
    assert!(notification_delivery::Entity::find_by_id(rows[0].id)
        .one(&db)
        .await
        .unwrap()
        .is_none());
}

async fn kind_rows(
    db: &DatabaseConnection,
    user_id: i64,
    key: &str,
) -> Vec<notification_delivery::Model> {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user_id))
        .filter(notification_delivery::Column::EventKey.eq(key))
        .filter(notification_delivery::Column::Channel.eq("email"))
        .all(db)
        .await
        .unwrap()
}

/// S10 bound, keep-oldest: the sixth warning of a kind to the same recipient
/// and destination is recorded as coalesced; the five already waiting keep
/// their payload.
#[tokio::test]
async fn a_full_kind_drops_the_new_row_and_keeps_the_waiting_ones() {
    let (db, user) = fixture().await;
    for _ in 0..6 {
        enqueue(&db, user.id, Event::NewDevice).await;
    }
    let rows = kind_rows(&db, user.id, "new_device").await;
    let queued: Vec<_> = rows.iter().filter(|r| r.status == "queued").collect();
    let dropped: Vec<_> = rows
        .iter()
        .filter(|r| r.last_error.as_deref() == Some("coalesced"))
        .collect();
    assert_eq!(queued.len(), queue::QUEUED_PER_KIND as usize);
    assert_eq!(dropped.len(), 1);
    assert!(queued.iter().all(|r| r.metadata != "{}"));
    assert_eq!(dropped[0].status, "cancelled");
    assert_eq!(dropped[0].metadata, "{}");
    assert!(dropped[0].id > queued.iter().map(|r| r.id).max().unwrap());
}

/// Somebody else's flood of a shared-resource event cannot push out the
/// recipient's waiting warning of that kind.
#[tokio::test]
async fn a_flood_never_displaces_a_waiting_warning() {
    let (db, user) = fixture().await;
    let real = enqueue(&db, user.id, Event::OrganizationAccessChanged).await;
    for _ in 0..20 {
        enqueue(&db, user.id, Event::OrganizationAccessChanged).await;
    }
    let first = notification_delivery::Entity::find_by_id(
        real.iter().find(|r| r.channel == "email").unwrap().id,
    )
    .one(&db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.status, "queued");
    assert_eq!(
        first.metadata,
        real.iter().find(|r| r.channel == "email").unwrap().metadata
    );
    let queued = kind_rows(&db, user.id, "organization_access_changed")
        .await
        .into_iter()
        .filter(|r| r.status == "queued")
        .count();
    assert_eq!(queued, queue::QUEUED_PER_KIND as usize);
}

/// Delivering one of the five frees a place; other kinds, channels and
/// activity are not bounded by it.
#[tokio::test]
async fn the_bound_is_per_kind_and_frees_with_delivery() {
    let (db, user) = fixture().await;
    for _ in 0..5 {
        enqueue(&db, user.id, Event::NewDevice).await;
    }
    let other_kind = enqueue(&db, user.id, Event::PasswordChanged).await;
    assert!(status_of(&db, &other_kind)
        .await
        .iter()
        .all(|s| s == "queued"));
    let activity: Vec<_> = {
        let mut rows = Vec::new();
        for _ in 0..8 {
            rows.extend(enqueue(&db, user.id, Event::Participating).await);
        }
        rows
    };
    assert!(status_of(&db, &activity)
        .await
        .iter()
        .all(|s| s == "queued"));

    let waiting = kind_rows(&db, user.id, "new_device").await;
    deliver_one(&db, waiting[0].id, &config(true), &Recorder::default())
        .await
        .unwrap();
    let next = enqueue(&db, user.id, Event::NewDevice).await;
    let email = next.iter().find(|r| r.channel == "email").unwrap();
    assert_eq!(
        status_of(&db, std::slice::from_ref(email)).await,
        vec!["queued"]
    );
}

/// Changing the Telegram destination again and again cannot push out the
/// warning owed to the first destination: each previous destination is its
/// own kind.
#[tokio::test]
async fn repeated_destination_changes_keep_the_first_destinations_warning() {
    let (db, user) = fixture().await;
    let ctx = settings::SaveContext {
        encryption_secret: KEY,
        smtp_available: false,
        metadata: Metadata::default(),
    };
    let generation = queue::profile(&db, user.id).await.unwrap().generation;
    queue::confirm_telegram(&db, user.id, generation)
        .await
        .unwrap();
    for chat in 1..=7 {
        settings::save(
            &db,
            user.id,
            &settings::Patch {
                telegram: Some(settings::TelegramChange::Set {
                    token: Some(TOKEN.into()),
                    chat_id: -1_002_000 - chat,
                    message_thread_id: None,
                }),
                ..Default::default()
            },
            &ctx,
            |_, _| Ok(()),
        )
        .await
        .unwrap();
        let generation = queue::profile(&db, user.id).await.unwrap().generation;
        queue::confirm_telegram(&db, user.id, generation)
            .await
            .unwrap();
    }
    let warnings: Vec<_> = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user.id))
        .filter(notification_delivery::Column::Channel.eq("telegram"))
        .filter(notification_delivery::Column::PreviousDestinationEncrypted.is_not_null())
        .all(&db)
        .await
        .unwrap();
    assert!(warnings.len() >= 7, "{}", warnings.len());
    assert!(warnings.iter().all(|r| r.status == "queued"));
    let first = warnings.iter().min_by_key(|r| r.id).unwrap();
    let opened = crate::auth::encryption::decrypt(
        first.previous_destination_encrypted.as_deref().unwrap(),
        &crate::auth::encryption::derive_key(KEY),
    )
    .unwrap();
    assert!(opened.contains("-1001234"), "{opened}");
}

async fn queued_of(
    db: &DatabaseConnection,
    user_id: i64,
    key: &str,
) -> Vec<notification_delivery::Model> {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user_id))
        .filter(notification_delivery::Column::EventKey.eq(key))
        .filter(notification_delivery::Column::Status.eq("queued"))
        .all(db)
        .await
        .unwrap()
}

/// Kinds are unbounded in number (each repository, each destination is one),
/// so one event also has a per-recipient budget: past it a new row is
/// coalesced whatever its kind, and the oldest rows keep their payload.
#[tokio::test]
async fn one_event_has_a_budget_across_its_kinds() {
    let (db, user) = fixture().await;
    let mut first = None;
    for i in 0..60 {
        let mut event = queue::QueueEvent::security(user.id, Event::RepositoryAccessChanged, None);
        event.repo_id = Some(10_000 + i);
        queue::enqueue(&db, &event).await.unwrap();
        if first.is_none() {
            first = Some(queue::event_rows(&db, user.id, &event.id).await.unwrap());
        }
    }
    let queued = queued_of(&db, user.id, "repository_access_changed").await;
    assert_eq!(queued.len(), queue::QUEUED_PER_EVENT as usize);
    let coalesced = sea_orm::PaginatorTrait::count(
        notification_delivery::Entity::find()
            .filter(notification_delivery::Column::UserId.eq(user.id))
            .filter(notification_delivery::Column::LastError.eq("coalesced")),
        &db,
    )
    .await
    .unwrap();
    assert!(coalesced > 0);
    for row in first.unwrap() {
        let kept = notification_delivery::Entity::find_by_id(row.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.status, "queued");
        assert_eq!(kept.metadata, row.metadata);
    }

    // A full budget of one event leaves another event untouched.
    let other = enqueue(&db, user.id, Event::EmailChanged).await;
    assert!(status_of(&db, &other).await.iter().all(|s| s == "queued"));
}

/// Changing the destination in a loop makes a new kind each time; the event
/// budget still bounds the queue, and the warning owed to the first
/// destination — the oldest — is kept.
#[tokio::test]
async fn a_settings_change_loop_is_bounded_and_keeps_the_first_warning() {
    let (db, user) = fixture().await;
    let ctx = settings::SaveContext {
        encryption_secret: KEY,
        smtp_available: false,
        metadata: Metadata::default(),
    };
    let generation = queue::profile(&db, user.id).await.unwrap().generation;
    queue::confirm_telegram(&db, user.id, generation)
        .await
        .unwrap();
    for chat in 1..=60 {
        settings::save(
            &db,
            user.id,
            &settings::Patch {
                telegram: Some(settings::TelegramChange::Set {
                    token: Some(TOKEN.into()),
                    chat_id: -1_003_000 - chat,
                    message_thread_id: None,
                }),
                ..Default::default()
            },
            &ctx,
            |_, _| Ok(()),
        )
        .await
        .unwrap();
        let generation = queue::profile(&db, user.id).await.unwrap().generation;
        queue::confirm_telegram(&db, user.id, generation)
            .await
            .unwrap();
    }
    let queued = queued_of(&db, user.id, "notification_settings_changed").await;
    assert!(
        queued.len() <= queue::QUEUED_PER_EVENT as usize,
        "{}",
        queued.len()
    );
    let first = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user.id))
        .filter(notification_delivery::Column::Channel.eq("telegram"))
        .filter(notification_delivery::Column::PreviousDestinationEncrypted.is_not_null())
        .all(&db)
        .await
        .unwrap()
        .into_iter()
        .min_by_key(|r| r.id)
        .expect("the warning to the first destination");
    assert_eq!(first.status, "queued");
    let opened = crate::auth::encryption::decrypt(
        first.previous_destination_encrypted.as_deref().unwrap(),
        &crate::auth::encryption::derive_key(KEY),
    )
    .unwrap();
    assert!(opened.contains("-1001234"), "{opened}");
}
