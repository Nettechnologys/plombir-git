//! Real DB mutation hooks, not an event-key-only catalog test.
use rg_db::{
    entities::*,
    notification::{Event, Metadata},
    ops::*,
};
use sea_orm::*;
use std::collections::BTreeMap;

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
    let user = user_ops::create_user(
        &db,
        "producer",
        "producer@example.invalid",
        "old-password-hash",
        "Producer",
    )
    .await
    .unwrap();
    let mut active: user::ActiveModel = user.into();
    active.auth_provider = Set("local".into());
    let user = active.update(&db).await.unwrap();
    (db, user)
}
async fn events(db: &DatabaseConnection, id: i64) -> BTreeMap<String, usize> {
    let rows = notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(id))
        .all(db)
        .await
        .unwrap();
    let mut events = BTreeMap::new();
    for row in rows {
        assert_eq!(row.channel, "email");
        for secret in [
            "token-hash-secret",
            "private-key-material",
            "model-secret-fixture",
            "secret-image-query",
            "factor-secret",
            "backup-secret",
            "old-password-hash",
            "new-password-hash",
            "reset-secret-hash",
        ] {
            assert!(
                !row.metadata.contains(secret),
                "{} leaked fixture secret",
                row.event_key
            );
        }
        *events.entry(row.event_key).or_default() += 1;
    }
    events
}
fn token_model(id: i64, name: &str) -> access_token::ActiveModel {
    access_token::ActiveModel {
        user_id: Set(id),
        name: Set(name.into()),
        token_hash: Set("token-hash-secret".into()),
        scopes: Set("repo".into()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
}

#[tokio::test]
async fn credential_mutations_emit_once_and_bot_events_reach_only_the_human_owner() {
    let (db, user) = fixture().await;
    let token = token_ops::create(&db, token_model(user.id, "laptop"), &[])
        .await
        .unwrap();
    assert!(token_ops::delete_by_id(&db, token.id).await.unwrap());
    assert!(!token_ops::delete_by_id(&db, token.id).await.unwrap());
    let ssh = ssh_key_ops::create(
        &db,
        ssh_key::ActiveModel {
            user_id: Set(user.id),
            title: Set("ssh-name".into()),
            public_key: Set("private-key-material".into()),
            fingerprint: Set("SHA256:safe-fingerprint".into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(ssh_key_ops::delete_by_id(&db, ssh.id).await.unwrap());
    assert!(!ssh_key_ops::delete_by_id(&db, ssh.id).await.unwrap());
    let signing = commit_signing_key_ops::create(
        &db,
        commit_signing_key::ActiveModel {
            user_id: Set(user.id),
            title: Set("sign-name".into()),
            kind: Set("ssh".into()),
            public_key: Set("private-key-material".into()),
            fingerprint: Set("SHA256:signing".into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        !commit_signing_key_ops::delete_by_user(&db, signing.id, user.id + 1)
            .await
            .unwrap()
    );
    assert!(
        commit_signing_key_ops::delete_by_user(&db, signing.id, user.id)
            .await
            .unwrap()
    );
    let passkey = passkey_credential_ops::create(
        &db,
        user.id,
        "credential-id",
        "private-key-material",
        "passkey-name",
        "forge.invalid",
    )
    .await
    .unwrap();
    assert!(
        !passkey_credential_ops::delete(&db, user.id + 1, passkey.id)
            .await
            .unwrap()
    );
    assert!(passkey_credential_ops::delete(&db, user.id, passkey.id)
        .await
        .unwrap());
    assert!(!passkey_credential_ops::delete(&db, user.id, passkey.id)
        .await
        .unwrap());
    let bot = user_ops::create_bot(&db, user.id, "producer-bot", "bot@example.invalid", None)
        .await
        .unwrap();
    let token = token_ops::create(&db, token_model(bot.id, "agent-name"), &[])
        .await
        .unwrap();
    assert!(token_ops::delete_by_id(&db, token.id).await.unwrap());
    let now = chrono::Utc::now();
    let profile = agent_profile_ops::upsert(
        &db,
        agent_profile::Model {
            id: 0,
            bot_id: bot.id,
            enabled: true,
            preset: "custom".into(),
            image: Some("registry.invalid/image?secret-image-query".into()),
            command: Some("private-key-material".into()),
            runner_tags: None,
            model_key_encrypted: Some("model-secret-fixture".into()),
            timeout_secs: 300,
            max_runs_per_day: 5,
            max_iterations: 2,
            created_at: now,
            updated_at: now,
        },
        &[],
    )
    .await
    .unwrap();
    agent_profile_ops::upsert(&db, profile, &[]).await.unwrap(); // effective no-op
    assert!(agent_profile_ops::delete_by_bot(&db, bot.id).await.unwrap());
    assert!(!agent_profile_ops::delete_by_bot(&db, bot.id).await.unwrap());
    assert!(user_ops::delete_by_id(&db, bot.id).await.unwrap());
    assert!(!user_ops::delete_by_id(&db, bot.id).await.unwrap());
    let found = events(&db, user.id).await;
    for event in [
        Event::TokenCreated,
        Event::TokenRevoked,
        Event::SshKeyAdded,
        Event::SshKeyRemoved,
        Event::SigningKeyAdded,
        Event::SigningKeyRemoved,
        Event::PasskeyAdded,
        Event::PasskeyRemoved,
        Event::BotCreated,
        Event::BotDeleted,
        Event::AgentTokenCreated,
        Event::AgentTokenRevoked,
    ] {
        assert_eq!(found[event.key()], 1, "{}", event.key());
    }
    assert_eq!(found[Event::AgentProfileChanged.key()], 2);
    assert_eq!(
        notification_delivery::Entity::find()
            .filter(notification_delivery::Column::UserId.ne(user.id))
            .count(&db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn factor_password_email_and_external_link_producers_follow_successful_commits() {
    let (db, user) = fixture().await;
    let first = user_ops::stage_pending_totp_secret(&db, user.id, "factor-secret-one")
        .await
        .unwrap()
        .unwrap();
    assert!(
        user_ops::complete_mfa_enrolment(&db, &first, &["backup-secret-one".into()])
            .await
            .unwrap()
    );
    assert!(
        !user_ops::complete_mfa_enrolment(&db, &first, &["backup-secret-one".into()])
            .await
            .unwrap()
    );
    let second = user_ops::stage_pending_totp_secret(&db, user.id, "factor-secret-two")
        .await
        .unwrap()
        .unwrap();
    assert!(
        user_ops::complete_mfa_enrolment(&db, &second, &["backup-secret-two".into()])
            .await
            .unwrap()
    );
    mfa_backup_code_ops::reissue_codes(&db, user.id, &["backup-secret-three".into()])
        .await
        .unwrap();
    crate::user::account::reset_mfa(&db, user.id).await.unwrap();
    assert!(crate::user::account::reset_mfa(&db, user.id).await.is_err());
    user_ops::disable_mfa(&db, user.id).await.unwrap(); // no second disabled event
    assert!(
        user_ops::change_password(&db, user.id, "wrong", "new-password-hash")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        user_ops::change_password(&db, user.id, "old-password-hash", "new-password-hash")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        user_ops::set_password_by_administrator(&db, user.id, "administrator-hash")
            .await
            .unwrap()
            .is_some()
    );
    let reset = password_reset_token_ops::create(
        &db,
        user.id,
        "reset-secret-hash",
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .unwrap();
    assert!(password_reset_token_ops::complete_password_reset(
        &db,
        reset.id,
        user.id,
        "new-password-hash"
    )
    .await
    .unwrap()
    .is_some());
    assert!(password_reset_token_ops::complete_password_reset(
        &db,
        reset.id,
        user.id,
        "new-password-hash"
    )
    .await
    .unwrap()
    .is_none());
    user_ops::update_email(&db, user.id, "changed@example.invalid")
        .await
        .unwrap()
        .unwrap();
    user_ops::update_email(&db, user.id, "changed@example.invalid")
        .await
        .unwrap()
        .unwrap();
    let link = oauth_account_ops::link(
        &db,
        user.id,
        "fixture-provider",
        "subject",
        "Provider User",
        "changed@example.invalid",
    )
    .await
    .unwrap()
    .unwrap();
    oauth_account_ops::link(
        &db,
        user.id,
        "fixture-provider",
        "subject",
        "Provider User",
        "changed@example.invalid",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!oauth_account_ops::delete_by_id(&db, link.id, user.id + 1)
        .await
        .unwrap());
    assert!(oauth_account_ops::delete_by_id(&db, link.id, user.id)
        .await
        .unwrap());
    assert!(!oauth_account_ops::delete_by_id(&db, link.id, user.id)
        .await
        .unwrap());
    let found = events(&db, user.id).await;
    for event in [
        Event::MfaEnabled,
        Event::MfaReplaced,
        Event::MfaDisabled,
        Event::BackupCodesRegenerated,
        Event::AdminFactorsReset,
        Event::PasswordChanged,
        Event::AdminPasswordReset,
        Event::PasswordReset,
        Event::EmailChanged,
        Event::ExternalLoginLinked,
        Event::ExternalLoginUnlinked,
    ] {
        assert_eq!(found[event.key()], 1, "{}", event.key());
    }
    let (sso, _) = oauth_account_ops::link_with_new_user(
        &db,
        "sso-first",
        "sso@example.invalid",
        "SSO",
        "fixture-provider",
        "sso-new",
        "SSO",
    )
    .await
    .unwrap();
    assert_eq!(
        events(&db, sso.id).await[Event::ExternalLoginLinked.key()],
        1
    );
}

#[tokio::test]
async fn deploy_and_runner_credential_events_target_only_the_actor_and_keep_scope() {
    let (db, user) = fixture().await;
    let now = chrono::Utc::now();
    let repo = repo_ops::create(
        &db,
        repository::ActiveModel {
            owner_id: Set(user.id),
            name: Set("private-fixture".into()),
            is_private: Set(true),
            default_branch: Set("main".into()),
            stars_count: Set(0),
            forks_count: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    rg_db::notification::scope(
        Some(user.id),
        Metadata {
            actor: Some("Fixture Admin".into()),
            ..Default::default()
        },
        async {
            let key = deploy_key_ops::create(
                &db,
                deploy_key::ActiveModel {
                    repo_id: Set(repo.id),
                    created_by_id: Set(Some(user.id)),
                    title: Set("deployment-name".into()),
                    public_key: Set("private-key-material".into()),
                    fingerprint: Set("SHA256:deploy-safe".into()),
                    read_only: Set(true),
                    created_at: Set(now),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(deploy_key_ops::delete_by_id(&db, key.id).await.unwrap());
            assert!(!deploy_key_ops::delete_by_id(&db, key.id).await.unwrap());
            let (runner, raw) =
                runner_ops::register_runner(&db, repo.id, "runner-name", "[]", None, None, None)
                    .await
                    .unwrap();
            assert!(runner_ops::stop_runner(&db, runner.id).await.unwrap());
            assert!(
                !events(&db, user.id)
                    .await
                    .contains_key(Event::RunnerTokenRevoked.key()),
                "going offline is not credential revocation"
            );
            assert!(runner_ops::deregister_runner(&db, runner.id).await.unwrap());
            assert!(!runner_ops::deregister_runner(&db, runner.id).await.unwrap());
            for row in notification_delivery::Entity::find()
                .all(&db)
                .await
                .unwrap()
            {
                assert!(row.admin_only);
                assert!(!row.metadata.contains(&raw));
                assert_eq!(
                    row.repo_id,
                    if row.event_key.starts_with("deploy_") {
                        Some(repo.id)
                    } else {
                        None
                    }
                );
            }
        },
    )
    .await;
    let found = events(&db, user.id).await;
    for event in [
        Event::DeployKeyAdded,
        Event::DeployKeyRemoved,
        Event::RunnerTokenCreated,
        Event::RunnerTokenRevoked,
    ] {
        assert_eq!(found[event.key()], 1);
    }
}

#[tokio::test]
async fn outbox_failure_rolls_back_credential_creation_and_retirement_erases_snapshots() {
    let (db, user) = fixture().await;
    let ctx = super::settings::SaveContext {
        encryption_secret: "fixture-key",
        smtp_available: false,
        metadata: Metadata::default(),
    };
    super::settings::save(
        &db,
        user.id,
        &super::settings::Patch {
            telegram: Some(super::settings::TelegramChange::Set {
                token: Some("123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi".into()),
                chat_id: -1001234,
                message_thread_id: None,
            }),
            ..Default::default()
        },
        &ctx,
        |_, _| Ok(()),
    )
    .await
    .unwrap();
    notification_delivery_ops::confirm_telegram(&db, user.id, 1)
        .await
        .unwrap();
    super::settings::save(
        &db,
        user.id,
        &super::settings::Patch {
            telegram: Some(super::settings::TelegramChange::Remove),
            ..Default::default()
        },
        &ctx,
        |_, _| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(
        notification_delivery::Entity::find()
            .filter(notification_delivery::Column::PreviousDestinationEncrypted.is_not_null())
            .count(&db)
            .await
            .unwrap(),
        1
    );
    assert!(user_ops::begin_user_retirement(&db, user.id).await.unwrap());
    assert_eq!(
        notification_delivery::Entity::find()
            .filter(notification_delivery::Column::PreviousDestinationEncrypted.is_not_null())
            .count(&db)
            .await
            .unwrap(),
        0
    );
    user_ops::abort_user_retirement(&db, user.id).await.unwrap();
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "DROP TABLE notification_deliveries".to_owned(),
    ))
    .await
    .unwrap();
    assert!(
        token_ops::create(&db, token_model(user.id, "must-rollback"), &[])
            .await
            .is_err()
    );
    assert_eq!(access_token::Entity::find().count(&db).await.unwrap(), 0);
    assert!(passkey_credential_ops::create(
        &db,
        user.id,
        "rollback-credential",
        "private-key-material",
        "rollback",
        "forge.invalid"
    )
    .await
    .is_err());
    assert_eq!(
        passkey_credential::Entity::find().count(&db).await.unwrap(),
        0
    );
}
