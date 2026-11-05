//! Portable migration and queue state smoke. Point the existing test URL at a
//! dedicated disposable database to exercise PostgreSQL/MySQL as well.
use rg_db::{
    entities::{known_browser, notification_delivery, notification_profile},
    notification::Event,
    ops::{notification_delivery_ops as queue, user_ops},
};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set, TransactionTrait};

#[tokio::test]
async fn notification_schema_roundtrips_generations_snapshots_leases_and_cleanup() {
    let url =
        std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL").unwrap_or_else(|_| "sqlite::memory:".into());
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    let name = format!("notice-{}", uuid::Uuid::new_v4().simple());
    let user = user_ops::create_user(
        &db,
        &name,
        &format!("{name}@example.invalid"),
        "fixture",
        "",
    )
    .await
    .unwrap();
    let tx = db.begin().await.unwrap();
    queue::lock_account(&tx, user.id).await.unwrap().unwrap();
    let mut profile = notification_profile::Model::defaults(user.id);
    profile.locale = "zh-CN".into();
    profile.telegram_token_encrypted = Some("opaque-fixture-ciphertext".into());
    profile.chat_id = Some(-1001234567890);
    profile.message_thread_id = Some(2147483647);
    profile.generation = 9;
    profile.choices = r#"{"new_device":{"email":true,"telegram":true}}"#.into();
    queue::put_profile(&tx, profile).await.unwrap();
    let mut event = queue::QueueEvent::security(
        user.id,
        Event::NewDevice,
        Some("Русский 中文 fixture".into()),
    );
    event.id = format!("schema-{}", uuid::Uuid::new_v4());
    queue::enqueue(&tx, &event).await.unwrap();
    queue::enqueue(&tx, &event).await.unwrap();
    // Thread fanout uses the bulk path; the same dedup contract must hold.
    queue::enqueue_many(&tx, std::slice::from_ref(&event))
        .await
        .unwrap();
    let now = chrono::Utc::now();
    known_browser::Entity::insert(known_browser::ActiveModel {
        id: sea_orm::NotSet,
        user_id: Set(user.id),
        witness_hash: Set(rg_db::token_digest("fixture witness")),
        created_at: Set(now),
        expires_at: Set(now + chrono::Duration::days(180)),
    })
    .exec(&tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Second-precision timestamps cannot hide a stronger folded notification.
    let note = rg_db::entities::notification::Model {
        id: 42,
        user_id: user.id,
        event_type: "issue".into(),
        title: "fixture title".into(),
        body: None,
        repo_id: None,
        is_read: false,
        created_at: now,
        updated_at: Some(now),
        reason: Some("mention".into()),
        subject_type: Some("issue".into()),
        subject_id: Some(1),
        link: Some("/fixture/repo/issues/1".into()),
        email_pending: true,
    };
    let mut stronger = note.clone();
    stronger.reason = Some("review_requested".into());
    assert_ne!(
        queue::thread_event_id(&note),
        queue::thread_event_id(&stronger)
    );
    stronger = note.clone();
    stronger.title = "changed title".into();
    assert_ne!(
        queue::thread_event_id(&note),
        queue::thread_event_id(&stronger)
    );
    let saved = queue::profile(&db, user.id).await.unwrap();
    assert_eq!(saved.generation, 9);
    assert_eq!(saved.chat_id, Some(-1001234567890));
    assert_eq!(saved.message_thread_id, Some(2147483647));
    let rows = queue::event_rows(&db, user.id, &event.id).await.unwrap();
    assert_eq!(rows.len(), 2, "unique event/recipient/channel dedup");
    for row in rows {
        assert!(row.metadata.contains("Русский 中文"));
        let held = queue::claim(&db, row.id).await.unwrap().unwrap();
        assert!(queue::claim(&db, row.id).await.unwrap().is_none());
        queue::finish(&db, &held, "sent", None, None).await.unwrap();
        let done = notification_delivery::Entity::find_by_id(row.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.metadata, "{}");
        assert!(done.destination_hash.is_empty());
        assert!(done.completed_at.is_some());
    }
    queue::confirm_telegram(&db, user.id, 8).await.unwrap();
    assert!(
        !queue::profile(&db, user.id)
            .await
            .unwrap()
            .telegram_confirmed
    );
    queue::confirm_telegram(&db, user.id, 9).await.unwrap();
    assert!(
        queue::profile(&db, user.id)
            .await
            .unwrap()
            .telegram_confirmed
    );
    assert_eq!(
        known_browser::Entity::find()
            .filter(known_browser::Column::UserId.eq(user.id))
            .count(&db)
            .await
            .unwrap(),
        1
    );
    user_ops::begin_user_retirement(&db, user.id).await.unwrap();
    assert!(queue::lock_account(&db, user.id).await.unwrap().is_none());
}
