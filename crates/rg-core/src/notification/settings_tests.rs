use super::*;
use rg_db::entities::notification_delivery as delivery;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

const TOKEN: &str = "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi";
const KEY: &str = "notification-fixture-key";

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
        "notice",
        "notice@example.invalid",
        "fixture",
        "Notice",
    )
    .await
    .unwrap();
    (db, user)
}

fn context() -> SaveContext<'static> {
    SaveContext {
        encryption_secret: KEY,
        smtp_available: true,
        metadata: Metadata {
            actor: Some("Alice".into()),
            ip: Some("198.51.100.9".into()),
            device: Some("Fixture Browser".into()),
            ..Default::default()
        },
    }
}

fn configure() -> Patch {
    Patch {
        telegram: Some(TelegramChange::Set {
            token: Some(TOKEN.into()),
            chat_id: -1001234,
            message_thread_id: Some(17),
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn defaults_and_mixed_patch_are_authorized_before_any_persistent_change() {
    let (db, user) = fixture().await;
    let patch = Patch {
        email: [(Event::Mention, false)].into(),
        locale: Some(Locale::Ru),
        ..Default::default()
    };
    let error = save(&db, user.id, &patch, &context(), |current, sensitive| {
        assert_eq!(current.id, user.id);
        assert!(sensitive, "absent-row defaults must count as enabled");
        anyhow::bail!("fixture sudo_required")
    })
    .await
    .err()
    .unwrap();
    assert_eq!(error.to_string(), "fixture sudo_required");
    assert_eq!(
        notification_profile::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        notification_setting::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(delivery::Entity::find().count(&db).await.unwrap(), 0);
    assert!(get(&db, user.id).await.unwrap().events[&Event::Mention].email);
}

#[tokio::test]
async fn every_event_channel_reduction_and_legacy_email_shape_require_proof() {
    let (db, user) = fixture().await;
    let mut initial = configure();
    initial.events = Event::ALL
        .iter()
        .map(|&event| {
            (
                event,
                ChannelPatch {
                    email: Some(true),
                    telegram: Some(true),
                },
            )
        })
        .collect();
    save(&db, user.id, &initial, &context(), |_, sensitive| {
        assert!(sensitive);
        Ok(())
    })
    .await
    .unwrap();
    let before = get(&db, user.id).await.unwrap();
    let jobs = delivery::Entity::find().count(&db).await.unwrap();
    for &event in Event::ALL {
        for telegram in [false, true] {
            let patch = Patch {
                events: [(
                    event,
                    ChannelPatch {
                        email: (!telegram).then_some(false),
                        telegram: telegram.then_some(false),
                    },
                )]
                .into(),
                ..Default::default()
            };
            assert!(save(&db, user.id, &patch, &context(), |_, sensitive| {
                assert!(sensitive);
                anyhow::bail!("proof needed")
            })
            .await
            .is_err());
        }
        // This is also the unchanged six-key legacy wire shape's path.
        let patch = Patch {
            email: [(event, false)].into(),
            ..Default::default()
        };
        assert!(save(&db, user.id, &patch, &context(), |_, sensitive| {
            assert!(sensitive);
            anyhow::bail!("proof needed")
        })
        .await
        .is_err());
    }
    let after = get(&db, user.id).await.unwrap();
    assert_eq!(before.events, after.events);
    assert_eq!(before.profile, after.profile);
    assert_eq!(delivery::Entity::find().count(&db).await.unwrap(), jobs);
}

#[tokio::test]
async fn every_destination_field_is_guarded_and_no_op_retains_confirmation() {
    let (db, user) = fixture().await;
    assert!(
        save(&db, user.id, &configure(), &context(), |_, sensitive| {
            assert!(sensitive);
            anyhow::bail!("proof needed")
        })
        .await
        .is_err()
    );
    save(&db, user.id, &configure(), &context(), |_, _| Ok(()))
        .await
        .unwrap();
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    for change in [
        TelegramChange::Set {
            token: Some(TOKEN.into()),
            chat_id: -1001234,
            message_thread_id: Some(17),
        },
        TelegramChange::Set {
            token: None,
            chat_id: -1005678,
            message_thread_id: Some(17),
        },
        TelegramChange::Set {
            token: None,
            chat_id: -1001234,
            message_thread_id: None,
        },
        TelegramChange::Set {
            token: None,
            chat_id: -1001234,
            message_thread_id: Some(18),
        },
        TelegramChange::Remove,
    ] {
        assert!(save(
            &db,
            user.id,
            &Patch {
                telegram: Some(change),
                ..Default::default()
            },
            &context(),
            |_, sensitive| {
                assert!(sensitive);
                anyhow::bail!("proof needed")
            }
        )
        .await
        .is_err());
    }
    for change in [
        TelegramChange::Keep,
        TelegramChange::Set {
            token: None,
            chat_id: -1001234,
            message_thread_id: Some(17),
        },
    ] {
        let result = save(
            &db,
            user.id,
            &Patch {
                telegram: Some(change),
                locale: Some(Locale::Ru),
                events: [(
                    Event::CiTriggered,
                    ChannelPatch {
                        email: Some(true),
                        telegram: Some(true),
                    },
                )]
                .into(),
                ..Default::default()
            },
            &context(),
            |_, sensitive| {
                assert!(!sensitive);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.profile.generation, 1);
        assert!(result.profile.telegram_confirmed);
    }
    let replaced = save(&db, user.id, &configure(), &context(), |_, sensitive| {
        assert!(sensitive);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(replaced.profile.generation, 2);
    assert!(!replaced.profile.telegram_confirmed);
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    assert!(!get(&db, user.id).await.unwrap().profile.telegram_confirmed);
}

#[tokio::test]
async fn previous_destination_is_encrypted_minimal_rekeyed_and_erased_on_completion() {
    let (db, user) = fixture().await;
    crate::auth::key_check::ensure_encryption_key_check(&db, KEY)
        .await
        .unwrap();
    save(&db, user.id, &configure(), &context(), |_, _| Ok(()))
        .await
        .unwrap();
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    let patch = Patch {
        telegram: Some(TelegramChange::Remove),
        locale: Some(Locale::ZhCn),
        ..Default::default()
    };
    save(&db, user.id, &patch, &context(), |_, sensitive| {
        assert!(sensitive);
        Ok(())
    })
    .await
    .unwrap();
    let old = delivery::Entity::find()
        .filter(delivery::Column::PreviousDestinationEncrypted.is_not_null())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.channel, "telegram");
    assert_eq!(old.prior_locale.as_deref(), Some("en"));
    for forbidden in [
        TOKEN,
        "198.51.100.9",
        "Fixture Browser",
        "Alice",
        "-1001234",
    ] {
        assert!(!old.metadata.contains(forbidden));
    }
    assert!(!format!("{old:?}").contains(TOKEN));
    let before = crate::auth::encryption::decrypt(
        old.previous_destination_encrypted.as_deref().unwrap(),
        &crate::auth::encryption::derive_key(KEY),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&before).unwrap()["chat_id"],
        -1001234
    );
    assert!(!before.contains(TOKEN));
    let report = crate::auth::rekey::rekey(&db, KEY, "replacement-fixture-key", false)
        .await
        .unwrap();
    assert!(report.columns.iter().any(|column| column.column
        == "notification_deliveries.previous_token_encrypted"
        && column.rewritten == 1));
    let rotated = delivery::Entity::find_by_id(old.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::auth::encryption::decrypt(
            rotated.previous_token_encrypted.as_deref().unwrap(),
            &crate::auth::encryption::derive_key("replacement-fixture-key")
        )
        .unwrap(),
        TOKEN
    );
    let held = queue::claim(&db, old.id).await.unwrap().unwrap();
    queue::finish(&db, &held, "sent", None, None).await.unwrap();
    let done = delivery::Entity::find_by_id(old.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert!(
        done.previous_token_encrypted.is_none() && done.previous_destination_encrypted.is_none()
    );
    assert_eq!(done.metadata, "{}");
    assert!(done.destination_hash.is_empty());
}

#[tokio::test]
async fn queue_deduplicates_claims_and_recovers_only_expired_leases() {
    let (db, user) = fixture().await;
    let event = queue::QueueEvent::security(
        user.id,
        Event::TokenCreated,
        Some("fixture-key-name".into()),
    );
    queue::enqueue(&db, &event).await.unwrap();
    queue::enqueue(&db, &event).await.unwrap();
    let rows = queue::event_rows(&db, user.id, &event.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    let (a, b) = tokio::join!(queue::claim(&db, rows[0].id), queue::claim(&db, rows[0].id));
    let held = match (a.unwrap(), b.unwrap()) {
        (Some(held), None) | (None, Some(held)) => held,
        other => panic!("duplicate lease: {other:?}"),
    };
    let mut expired: delivery::ActiveModel = held.clone().into();
    expired.lease_until = Set(Some(chrono::Utc::now() - chrono::Duration::seconds(1)));
    expired.update(&db).await.unwrap();
    let recovered = queue::claim(&db, held.id).await.unwrap().unwrap();
    assert_ne!(held.lease_token, recovered.lease_token);
    queue::finish(&db, &held, "sent", None, None).await.unwrap();
    assert_eq!(
        delivery::Entity::find_by_id(held.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .status,
        "inflight"
    );
    queue::finish(&db, &recovered, "failed", Some("fixture_failure"), None)
        .await
        .unwrap();
    assert!(queue::claim(&db, held.id).await.unwrap().is_none());
}

#[tokio::test]
async fn expiry_and_account_cancellation_erase_queued_snapshots() {
    let (db, user) = fixture().await;
    save(&db, user.id, &configure(), &context(), |_, _| Ok(()))
        .await
        .unwrap();
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    save(
        &db,
        user.id,
        &Patch {
            telegram: Some(TelegramChange::Remove),
            ..Default::default()
        },
        &context(),
        |_, _| Ok(()),
    )
    .await
    .unwrap();
    let row = delivery::Entity::find()
        .filter(delivery::Column::PreviousDestinationEncrypted.is_not_null())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut expired: delivery::ActiveModel = row.clone().into();
    expired.expires_at = Set(chrono::Utc::now() - chrono::Duration::seconds(1));
    expired.update(&db).await.unwrap();
    queue::cleanup(&db).await.unwrap();
    let row = delivery::Entity::find_by_id(row.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.previous_destination_encrypted.is_none() && row.previous_token_encrypted.is_none());
    let event = queue::QueueEvent::security(user.id, Event::NewDevice, None);
    queue::enqueue(&db, &event).await.unwrap();
    queue::cancel_account(&db, user.id).await.unwrap();
    assert!(queue::event_rows(&db, user.id, &event.id)
        .await
        .unwrap()
        .iter()
        .all(|row| row.status == "cancelled" && row.metadata == "{}"));
}

#[tokio::test]
async fn waiting_writer_classifies_committed_settings_and_current_account() {
    let (db, user) = fixture().await;
    let off = Patch {
        email: [(Event::Mention, false)].into(),
        ..Default::default()
    };
    save(&db, user.id, &off, &context(), |_, _| Ok(()))
        .await
        .unwrap();
    let txn = db.begin().await.unwrap();
    let current = queue::lock_account(&txn, user.id).await.unwrap().unwrap();
    let mut prefs = legacy::get(&txn, user.id).await.unwrap();
    prefs.email_mention = true;
    legacy::put(&txn, prefs).await.unwrap();
    let mut rotated: user::ActiveModel = current.into();
    rotated.session_version = Set(user.session_version + 1);
    rotated.totp_secret = Set(Some("rotated-factor-fixture".into()));
    rotated.update(&txn).await.unwrap();
    // A writer which started before this commit must block before reading,
    // then classify true→false and see the new session generation.
    let ctx = context();
    let waiting = save(&db, user.id, &off, &ctx, |fresh, sensitive| {
        assert!(sensitive);
        assert_eq!(fresh.session_version, user.session_version + 1);
        assert_eq!(fresh.totp_secret.as_deref(), Some("rotated-factor-fixture"));
        anyhow::bail!("fixture session_revoked")
    });
    tokio::pin!(waiting);
    match tokio::time::timeout(std::time::Duration::from_millis(25), &mut waiting).await {
        Err(_) => {}
        Ok(Ok(_)) => panic!("waiting writer unexpectedly completed before commit"),
        Ok(Err(error)) => panic!("waiting writer failed before commit: {error:#}"),
    }
    txn.commit().await.unwrap();
    assert_eq!(
        waiting.await.err().unwrap().to_string(),
        "fixture session_revoked"
    );
    assert!(get(&db, user.id).await.unwrap().events[&Event::Mention].email);
}

#[tokio::test]
async fn durable_warning_insert_failure_rolls_back_the_entire_authorized_patch() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let (db, user) = fixture().await;
    save(&db, user.id, &configure(), &context(), |_, _| Ok(()))
        .await
        .unwrap();
    queue::confirm_telegram(&db, user.id, 1).await.unwrap();
    let before = get(&db, user.id).await.unwrap();
    let jobs = delivery::Entity::find().count(&db).await.unwrap();
    let patch = Patch {
        telegram: Some(TelegramChange::Remove),
        email: [(Event::Mention, false)].into(),
        locale: Some(Locale::ZhCn),
        ..Default::default()
    };
    // First fail the warning itself, then fail the profile write AFTER the
    // warning insert. Neither ordering may leave half the transaction behind.
    for (operation, table) in [
        ("INSERT", "notification_deliveries"),
        ("UPDATE", "notification_profiles"),
    ] {
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "CREATE TRIGGER reject_notice BEFORE {operation} ON {table} \
                     BEGIN SELECT RAISE(ABORT, 'fixture write unavailable'); END"
            ),
        ))
        .await
        .unwrap();
        assert!(save(&db, user.id, &patch, &context(), |_, sensitive| {
            assert!(sensitive);
            Ok(())
        })
        .await
        .is_err());
        let after = get(&db, user.id).await.unwrap();
        assert_eq!(after.profile, before.profile);
        assert_eq!(after.legacy, before.legacy);
        assert_eq!(after.events, before.events);
        assert_eq!(delivery::Entity::find().count(&db).await.unwrap(), jobs);
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "DROP TRIGGER reject_notice".to_owned(),
        ))
        .await
        .unwrap();
    }
}
