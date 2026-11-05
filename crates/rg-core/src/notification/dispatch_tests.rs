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
    let db = rg_db::connect("sqlite::memory:").await.unwrap();
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
    let sent = out.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "telegram:-1001234:Some(42)");
    assert!(sent[0].1.contains("新设备登录") && sent[0].1.contains("Alice <literal>"));
    drop(sent);
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
    drop(sent);
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
