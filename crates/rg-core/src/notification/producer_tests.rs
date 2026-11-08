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

async fn access_repo(db: &DatabaseConnection, owner: i64, org: Option<i64>) -> repository::Model {
    let now = chrono::Utc::now();
    repo_ops::create(
        db,
        repository::ActiveModel {
            owner_id: Set(owner),
            org_id: Set(org),
            name: Set("access-fixture".into()),
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
    .unwrap()
}
async fn access_rows(db: &DatabaseConnection, event: Event) -> Vec<notification_delivery::Model> {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::EventKey.eq(event.key()))
        .order_by_asc(notification_delivery::Column::Id)
        .all(db)
        .await
        .unwrap()
}
#[tokio::test]
async fn collaborator_changes_notify_owner_and_affected_account_once_without_actor_device_leaks() {
    let (db, owner) = fixture().await;
    let member = user_ops::create_user(&db, "member", "member@example.invalid", "hash", "Member")
        .await
        .unwrap();
    let repo = access_repo(&db, owner.id, None).await;
    rg_db::notification::scope(
        Some(owner.id),
        Metadata {
            actor: Some(owner.username.clone()),
            ip: Some("198.51.100.9".into()),
            device: Some("Owner Browser".into()),
            ..Default::default()
        },
        async {
            let added = repo_collaborator_ops::create(
                &db,
                repo_collaborator::ActiveModel {
                    repo_id: Set(repo.id),
                    user_id: Set(member.id),
                    permission: Set("read".into()),
                    created_at: Set(chrono::Utc::now()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(
                !repo_collaborator_ops::update_permission(&db, added.id, "read".into())
                    .await
                    .unwrap()
                    .unwrap()
                    .changed
            );
            assert_eq!(
                access_rows(&db, Event::RepositoryAccessChanged).await.len(),
                2
            );
            assert!(
                repo_collaborator_ops::update_permission(&db, added.id, "admin".into())
                    .await
                    .unwrap()
                    .unwrap()
                    .changed
            );
            assert!(
                repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, member.id)
                    .await
                    .unwrap()
            );
            assert!(
                !repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, member.id)
                    .await
                    .unwrap()
            );
        },
    )
    .await;
    let rows = access_rows(&db, Event::RepositoryAccessChanged).await;
    assert_eq!(rows.len(), 6);
    for row in rows {
        let metadata: Metadata = serde_json::from_str(&row.metadata).unwrap();
        assert_eq!(metadata.actor.as_deref(), Some(owner.username.as_str()));
        if row.user_id == owner.id {
            assert!(row.admin_only && row.repo_id == Some(repo.id));
            assert_eq!(metadata.ip.as_deref(), Some("198.51.100.9"));
        } else {
            assert_eq!(row.user_id, member.id);
            assert!(metadata.ip.is_none() && metadata.device.is_none());
            assert!(
                row.repo_id.is_none() && !row.admin_only,
                "revoked account still receives its historical notice"
            );
        }
    }
}

#[tokio::test]
async fn outbox_failure_rolls_back_access_grants_revocations_and_visibility() {
    let (db, owner) = fixture().await;
    let member = user_ops::create_user(&db, "member", "member@example.invalid", "hash", "Member")
        .await
        .unwrap();
    let repo = access_repo(&db, owner.id, None).await;
    let model = || repo_collaborator::ActiveModel {
        repo_id: Set(repo.id),
        user_id: Set(member.id),
        permission: Set("read".into()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    let row = repo_collaborator_ops::create(&db, model()).await.unwrap();
    let org = org_ops::create_org(&db, "org", None, None, owner.id, "private")
        .await
        .unwrap();
    let team = org_ops::create_team(&db, org.id, "admin-team", None, "admin")
        .await
        .unwrap();
    org_ops::add_org_member(&db, org.id, member.id, "member")
        .await
        .unwrap();
    org_ops::add_team_member(&db, team.id, member.id, "member")
        .await
        .unwrap();
    db.execute(Statement::from_string(DatabaseBackend::Sqlite,
        "CREATE TRIGGER refuse_notices BEFORE INSERT ON notification_deliveries BEGIN SELECT RAISE(ABORT, 'fixture notification failure'); END".to_owned())).await.unwrap();
    assert!(
        repo_collaborator_ops::update_permission(&db, row.id, "admin".into())
            .await
            .is_err()
    );
    assert_eq!(
        repo_collaborator_ops::find_by_id(&db, row.id)
            .await
            .unwrap()
            .unwrap()
            .permission,
        "read"
    );
    assert!(
        repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, member.id)
            .await
            .is_err()
    );
    assert!(repo_collaborator_ops::find_by_id(&db, row.id)
        .await
        .unwrap()
        .is_some());
    assert!(repo_ops::update_settings(&db, repo.id, None, Some(false))
        .await
        .is_err());
    assert!(
        repo_ops::find_by_id(&db, repo.id)
            .await
            .unwrap()
            .unwrap()
            .is_private
    );
    assert!(org_ops::remove_org_member(&db, org.id, member.id)
        .await
        .is_err());
    assert!(org_ops::find_org_member(&db, org.id, member.id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        org_ops::list_team_members(&db, team.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(org_ops::delete_team(&db, team.id).await.is_err());
    assert!(org_ops::get_team(&db, team.id).await.unwrap().is_some());
    assert!(ci_secret_ops::upsert(
        &db,
        repo.id,
        None,
        "SECRET",
        "encrypted-fixture-secret",
        owner.id
    )
    .await
    .is_err());
    assert!(
        ci_secret_ops::find_by_repo_and_name(&db, repo.id, None, "SECRET")
            .await
            .unwrap()
            .is_none()
    );
    assert!(ci_variable_ops::upsert(
        &db,
        ci_variable_ops::Owner::Repository(repo.id),
        "CONFIG",
        "fixture-value",
        owner.id
    )
    .await
    .is_err());
    assert!(
        ci_variable_ops::list(&db, ci_variable_ops::Owner::Repository(repo.id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn removing_organization_membership_erases_team_authority_and_reinvitation_cannot_restore_it()
{
    let (db, owner) = fixture().await;
    let member = user_ops::create_user(&db, "member", "member@example.invalid", "hash", "Member")
        .await
        .unwrap();
    let org = org_ops::create_org(&db, "org", None, None, owner.id, "private")
        .await
        .unwrap();
    let repo = access_repo(&db, owner.id, Some(org.id)).await;
    let team = org_ops::create_team(&db, org.id, "admins", None, "admin")
        .await
        .unwrap();
    let refused = crate::org::add_team_member(&db, team.id, member.id, "member")
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("organization before"));
    org_ops::add_org_member(&db, org.id, member.id, "member")
        .await
        .unwrap();
    crate::org::add_team_member(&db, team.id, member.id, "member")
        .await
        .unwrap();
    assert!(
        crate::repo::service::can_admin_repo(&db, &repo, Some(member.id))
            .await
            .unwrap()
    );
    crate::org::remove_org_member(&db, org.id, member.id)
        .await
        .unwrap();
    assert!(
        !crate::repo::service::can_admin_repo(&db, &repo, Some(member.id))
            .await
            .unwrap()
    );
    assert!(org_ops::list_team_members(&db, team.id)
        .await
        .unwrap()
        .is_empty());
    org_ops::add_org_member(&db, org.id, member.id, "member")
        .await
        .unwrap();
    assert!(
        !crate::repo::service::can_admin_repo(&db, &repo, Some(member.id))
            .await
            .unwrap()
    );
    assert!(
        !crate::repo::service::can_write_repo(&db, &repo, Some(member.id))
            .await
            .unwrap()
    );
    // Old installations can contain dormant rows: invitation discards those too.
    crate::org::remove_org_member(&db, org.id, member.id)
        .await
        .unwrap();
    team_member::ActiveModel {
        team_id: Set(team.id),
        user_id: Set(member.id),
        role: Set("member".into()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    org_ops::add_org_member(&db, org.id, member.id, "member")
        .await
        .unwrap();
    assert!(
        !crate::repo::service::can_admin_repo(&db, &repo, Some(member.id))
            .await
            .unwrap()
    );
    assert!(org_ops::list_team_members(&db, team.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn configuration_producers_skip_no_ops_and_never_queue_destination_or_secret_values() {
    let (db, owner) = fixture().await;
    let repo = access_repo(&db, owner.id, None).await;
    let now = chrono::Utc::now();
    let hook = webhook_ops::create_webhook(
        &db,
        webhook::ActiveModel {
            repo_id: Set(repo.id),
            scope_id: Set(repo.id),
            scope_kind: Set("repo".into()),
            url: Set("https://private-destination.invalid/path".into()),
            content_type: Set("json".into()),
            secret_encrypted: Set(Some("secret-hook-ciphertext".into())),
            active: Set(true),
            events: Set("push".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    webhook_ops::update_webhook(
        &db,
        hook.id,
        hook.url.clone(),
        hook.content_type.clone(),
        hook.secret_encrypted.clone(),
        hook.active,
        hook.events.clone(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(access_rows(&db, Event::WebhookChanged).await.len(), 1);
    webhook_ops::delete_webhook_by_id(&db, hook.id)
        .await
        .unwrap();
    assert!(!webhook_ops::delete_webhook_by_id(&db, hook.id)
        .await
        .unwrap());
    assert_eq!(access_rows(&db, Event::WebhookChanged).await.len(), 2);
    let mirror = mirror_ops::create(
        &db,
        mirror::ActiveModel {
            repo_id: Set(repo.id),
            url: Set("https://private-destination.invalid/repo".into()),
            password_encrypted: Set(Some("secret-mirror-ciphertext".into())),
            sync_interval_seconds: Set(600),
            status: Set("active".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    mirror_ops::update_settings(
        &db,
        mirror.id,
        None,
        None,
        None,
        None,
        None,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(access_rows(&db, Event::MirrorChanged).await.len(), 1);
    assert_eq!(
        mirror_ops::delete_by_id_unless_syncing(
            &db,
            mirror.id,
            repo.id,
            now - chrono::Duration::hours(1)
        )
        .await
        .unwrap(),
        mirror_ops::MirrorRetirement::Deleted
    );
    assert_eq!(access_rows(&db, Event::MirrorChanged).await.len(), 2);
    let tag = protected_tag_ops::create_with_push_grants(
        &db,
        protected_tag::ActiveModel {
            repo_id: Set(repo.id),
            pattern: Set("v*".into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![owner.id]),
    )
    .await
    .unwrap();
    protected_tag_ops::update_with_push_grants(&db, tag.clone(), vec![owner.id])
        .await
        .unwrap();
    assert_eq!(access_rows(&db, Event::CiSecurityChanged).await.len(), 1);
    assert!(protected_tag_ops::delete_by_id(&db, tag.id).await.unwrap());
    assert_eq!(access_rows(&db, Event::CiSecurityChanged).await.len(), 2);
    let environment = ci_environment_ops::create_with_approvers(
        &db,
        ci_environment::ActiveModel {
            repo_id: Set(repo.id),
            name: Set("production".into()),
            protected: Set(true),
            required_approvals: Set(1),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        vec![owner.id],
    )
    .await
    .unwrap();
    assert!(ci_environment_ops::delete(&db, environment.id)
        .await
        .unwrap());
    ci_secret_ops::upsert(&db, repo.id, None, "KEY", "secret-ci-ciphertext", owner.id)
        .await
        .unwrap();
    ci_secret_ops::upsert(&db, repo.id, None, "KEY", "secret-ci-ciphertext", owner.id)
        .await
        .unwrap();
    assert!(
        ci_secret_ops::delete_by_repo_and_name(&db, repo.id, None, "KEY")
            .await
            .unwrap()
    );
    let ci_owner = ci_variable_ops::Owner::Repository(repo.id);
    ci_variable_ops::upsert(
        &db,
        ci_owner,
        "CONFIG",
        "private-configuration-value",
        owner.id,
    )
    .await
    .unwrap();
    ci_variable_ops::upsert(
        &db,
        ci_owner,
        "CONFIG",
        "private-configuration-value",
        owner.id,
    )
    .await
    .unwrap();
    assert!(ci_variable_ops::delete(&db, ci_owner, "CONFIG")
        .await
        .unwrap());
    assert_eq!(access_rows(&db, Event::CiSecurityChanged).await.len(), 8);
    for row in notification_delivery::Entity::find()
        .all(&db)
        .await
        .unwrap()
    {
        for forbidden in [
            "private-destination",
            "secret-hook",
            "secret-mirror",
            "secret-ci",
            "private-configuration",
        ] {
            assert!(
                !row.metadata.contains(forbidden),
                "{} leaked {forbidden}",
                row.event_key
            );
        }
    }
}

#[tokio::test]
async fn grant_only_updates_emit_once_and_identical_lists_are_no_ops() {
    let (db, owner) = fixture().await;
    let other = user_ops::create_user(
        &db,
        "approver",
        "approver@example.invalid",
        "hash",
        "Approver",
    )
    .await
    .unwrap();
    let repo = access_repo(&db, owner.id, None).await;
    let now = chrono::Utc::now();
    let branch = protected_branch_ops::create_with_push_grants(
        &db,
        protected_branch::ActiveModel {
            repo_id: Set(repo.id),
            branch_name: Set("main".into()),
            require_pr: Set(true),
            require_status_check: Set(false),
            require_approval: Set(false),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![owner.id]),
    )
    .await
    .unwrap();
    let environment = ci_environment_ops::create_with_approvers(
        &db,
        ci_environment::ActiveModel {
            repo_id: Set(repo.id),
            name: Set("production".into()),
            protected: Set(true),
            required_approvals: Set(1),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        vec![owner.id],
    )
    .await
    .unwrap();
    let baseline = access_rows(&db, Event::CiSecurityChanged).await.len();
    let changed = protected_branch_ops::update_with_push_grants(&db, branch, Some(vec![other.id]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        access_rows(&db, Event::CiSecurityChanged).await.len(),
        baseline + 1
    );
    protected_branch_ops::update_with_push_grants(&db, changed, Some(vec![other.id]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        access_rows(&db, Event::CiSecurityChanged).await.len(),
        baseline + 1
    );
    for expected in [baseline + 2, baseline + 2] {
        ci_environment_ops::update_with_approvers(
            &db,
            environment.id,
            repo.id,
            environment.name.clone(),
            environment.protected,
            environment.required_approvals,
            chrono::Utc::now(),
            vec![other.id],
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            access_rows(&db, Event::CiSecurityChanged).await.len(),
            expected
        );
    }
}

#[tokio::test]
async fn transfer_notices_keep_the_new_private_namespace_out_of_former_principals_copies() {
    let (db, owner) = fixture().await;
    let recipient = user_ops::create_user(
        &db,
        "newowner",
        "newowner@example.invalid",
        "hash",
        "New owner",
    )
    .await
    .unwrap();
    let collaborator = user_ops::create_user(
        &db,
        "collaborator",
        "collaborator@example.invalid",
        "hash",
        "Collaborator",
    )
    .await
    .unwrap();
    let org = org_ops::create_org(
        &db,
        "secret-destination",
        Some("Destination"),
        None,
        recipient.id,
        "private",
    )
    .await
    .unwrap();
    let repo = access_repo(&db, owner.id, None).await;
    repo_collaborator_ops::create(
        &db,
        repo_collaborator::ActiveModel {
            repo_id: Set(repo.id),
            user_id: Set(collaborator.id),
            permission: Set("read".into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let outcome = repo_ops::transfer_owner(
        &db,
        repo.id,
        recipient.id,
        Some(org.id),
        owner.id,
        None,
        &owner.username,
        &org.name,
        &repo.name,
        &repo.name,
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        repo_ops::TransferOwnerOutcome::Transferred(_)
    ));
    let rows = access_rows(&db, Event::RepositorySecurityChanged).await;
    assert_eq!(rows.len(), 3);
    for row in rows {
        let metadata: Metadata = serde_json::from_str(&row.metadata).unwrap();
        if row.user_id == recipient.id {
            assert_eq!(
                metadata.name.as_deref(),
                Some("secret-destination/access-fixture")
            );
            assert!(row.admin_only);
            assert_eq!(row.repo_id, Some(repo.id));
        } else {
            assert!([owner.id, collaborator.id].contains(&row.user_id));
            assert_eq!(metadata.name.as_deref(), Some("producer/access-fixture"));
            assert!(!row.metadata.contains("secret-destination"));
            assert!(!row.admin_only);
            assert_eq!(row.repo_id, None);
        }
    }
}

#[tokio::test]
async fn bot_grants_notify_the_human_owner_and_name_the_affected_account() {
    let (db, owner) = fixture().await;
    let human = user_ops::create_user(
        &db,
        "agentowner",
        "agentowner@example.invalid",
        "hash",
        "Agent owner",
    )
    .await
    .unwrap();
    let bot = user_ops::create_bot(
        &db,
        human.id,
        "grant-bot",
        "grant-bot@example.invalid",
        None,
    )
    .await
    .unwrap();
    let repo = access_repo(&db, owner.id, None).await;
    repo_collaborator_ops::create(
        &db,
        repo_collaborator::ActiveModel {
            repo_id: Set(repo.id),
            user_id: Set(bot.id),
            permission: Set("read".into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rows = access_rows(&db, Event::RepositoryAccessChanged).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.user_id == human.id));
    for row in rows {
        let metadata: Metadata = serde_json::from_str(&row.metadata).unwrap();
        assert_eq!(metadata.subject.as_deref(), Some("grant-bot"));
        assert_ne!(row.user_id, bot.id);
    }
}

#[tokio::test]
async fn anonymous_account_proof_keeps_its_context_but_admin_action_hides_it_from_target() {
    let (db, owner) = fixture().await;
    let admin = user_ops::create_user(
        &db,
        "administrator",
        "admin@example.invalid",
        "hash",
        "Admin",
    )
    .await
    .unwrap();
    let context = Metadata {
        ip: Some("198.51.100.50".into()),
        device: Some("Proof browser".into()),
        ..Default::default()
    };
    for actor in [None, Some(admin.id)] {
        rg_db::notification::scope(
            actor,
            context.clone(),
            notification_delivery_ops::account_event(
                &db,
                owner.id,
                Event::PasswordChanged,
                None,
                None,
                None,
            ),
        )
        .await
        .unwrap();
    }
    let rows = access_rows(&db, Event::PasswordChanged).await;
    assert_eq!(rows.len(), 2);
    let own: Metadata = serde_json::from_str(&rows[0].metadata).unwrap();
    let foreign: Metadata = serde_json::from_str(&rows[1].metadata).unwrap();
    assert_eq!(own.ip, context.ip);
    assert_eq!(own.device, context.device);
    assert_eq!(foreign.ip, None);
    assert_eq!(foreign.device, None);
}
