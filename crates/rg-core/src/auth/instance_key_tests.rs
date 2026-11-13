use super::*;
use crate::test_support::migrated_memory_database;
use sea_orm::{ActiveModelTrait, Set};

const JWT: &str = "the-signing-secret";
const ENC: &str = "the-at-rest-key";

fn cipher() -> [u8; 32] {
    encryption::derive_key(ENC)
}

async fn stored(db: &DatabaseConnection) -> Vec<row::Model> {
    ops::all(db).await.unwrap()
}

/// A row as `m20261113_000001_instance_signing_keys` carries it over: sealed
/// seed, no kid or public key yet.
async fn carried_over(db: &DatabaseConnection, key: &InstanceKey, origin: &str) {
    row::ActiveModel {
        id: sea_orm::NotSet,
        kid: Set(None),
        public_key: Set(None),
        seed_encrypted: Set(Some(encrypt_seed(key, &cipher()).unwrap())),
        origin: Set(origin.to_owned()),
        status: Set(STATUS_ACTIVE.to_owned()),
        active_slot: Set(Some(row::ACTIVE_SLOT)),
        created_at: Set(chrono::Utc::now()),
        retired_at: Set(None),
        revoked_at: Set(None),
    }
    .insert(db)
    .await
    .unwrap();
}

async fn store_attestation(
    db: &DatabaseConnection,
    signer: &InstanceKey,
) -> crate::attestation::Envelope {
    // The subject is the key that signed, not referential integrity: the asset
    // gets a synthetic parent instead of a repository/release chain.
    sea_orm::ConnectionTrait::execute_unprepared(db, "PRAGMA foreign_keys = OFF")
        .await
        .unwrap();
    let statement = crate::attestation::Statement::new(
        "app.tar.gz",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        crate::attestation::PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
        serde_json::json!({ "builder": { "id": "https://forge.example" } }),
    );
    let envelope = crate::attestation::sign_statement(signer, &statement).unwrap();
    rg_db::ops::release_ops::create_asset(
        db,
        rg_db::entities::release_asset::ActiveModel {
            release_id: Set(1),
            filename: Set("app.tar.gz".into()),
            size: Set(1),
            content_type: Set("application/octet-stream".into()),
            download_count: Set(0),
            uploader_id: Set(Some(1)),
            created_at: Set(chrono::Utc::now()),
            sha256: Set(Some("e3b0".into())),
            attestation: Set(Some(serde_json::to_string(&envelope).unwrap())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    envelope
}

fn verify(
    ring: &KeyRing,
    envelope: &crate::attestation::Envelope,
) -> crate::attestation::VerificationStatus {
    match crate::attestation::verify_envelope(
        ring,
        envelope,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        &crate::attestation::VerifierRegistry::with_defaults(),
    ) {
        Ok(_) => crate::attestation::VerificationStatus::Verified,
        Err(error) => error.status(),
    }
}

/// S11: a fresh installation must not hold a key anyone who knows
/// `jwt_secret` can recompute.
#[tokio::test]
async fn a_fresh_installation_generates_a_key_unrelated_to_the_jwt_secret() {
    let db = migrated_memory_database().await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    assert_ne!(
        ring.active().seed(),
        InstanceKey::derived_from_secret(JWT).seed()
    );
    assert_eq!(ring.keys().len(), 1);
    assert_eq!(ring.keys()[0].origin, KeyOrigin::Generated);
    assert_eq!(ring.keys()[0].status, KeyStatus::Active);
}

/// A second start must neither mint a second identity nor rotate a generated
/// one.
#[tokio::test]
async fn establishing_twice_keeps_one_generated_key() {
    let db = migrated_memory_database().await;
    let first = establish(&db, JWT, ENC).await.unwrap();
    let second = establish(&db, "another-secret", ENC).await.unwrap();
    assert_eq!(first.active().kid(), second.active().kid());
    assert_eq!(stored(&db).await.len(), 1);
}

/// An instance upgraded from before keys were stored keeps its attestations
/// verifiable here, but the derived key no longer signs and its seed is gone.
#[tokio::test]
async fn an_upgrade_with_attestations_adopts_then_retires_the_derived_key() {
    let db = migrated_memory_database().await;
    let derived = InstanceKey::derived_from_secret(JWT);
    let envelope = store_attestation(&db, &derived).await;

    let ring = establish(&db, JWT, ENC).await.unwrap();
    assert_ne!(ring.active().kid(), derived.kid());
    let old = ring
        .find(derived.kid())
        .expect("derived key kept as a public half");
    assert_eq!(old.status, KeyStatus::Retired);
    assert_eq!(old.origin, KeyOrigin::LegacyDerived);
    assert_eq!(
        verify(&ring, &envelope),
        crate::attestation::VerificationStatus::Verified
    );

    let rows = stored(&db).await;
    let retired = rows
        .iter()
        .find(|r| r.kid.as_deref() == Some(derived.kid()))
        .unwrap();
    assert!(
        retired.seed_encrypted.is_none(),
        "a retired key keeps no seed"
    );
}

/// The statement `docs/install.md` tells an operator who lost the encryption
/// key to run before starting the server, so the documented step is the
/// tested one.
const RETIRE_LOST_ACTIVE_KEY: &str = "UPDATE instance_signing_keys \
     SET status = 'retired', active_slot = NULL, seed_encrypted = NULL, \
         retired_at = CURRENT_TIMESTAMP \
     WHERE status = 'active'";

/// B1: keys recorded but none active, attestations stored — the derived key
/// must not come back. It may already be recorded (here it is, retired on
/// upgrade), so adopting it would either collide with its row or put a key
/// computable from `jwt_secret` back into service.
#[tokio::test]
async fn a_retired_active_key_is_replaced_by_a_generated_key_never_the_derived_one() {
    let db = migrated_memory_database().await;
    let derived = InstanceKey::derived_from_secret(JWT);
    let envelope = store_attestation(&db, &derived).await;
    let upgraded = establish(&db, JWT, ENC).await.unwrap();

    sea_orm::ConnectionTrait::execute_unprepared(&db, RETIRE_LOST_ACTIVE_KEY)
        .await
        .unwrap();
    let ring = establish(&db, JWT, ENC).await.unwrap();
    assert_ne!(ring.active().kid(), derived.kid());
    assert_ne!(ring.active().kid(), upgraded.active().kid());
    assert_eq!(
        ring.find(ring.active().kid()).unwrap().origin,
        KeyOrigin::Generated
    );
    assert_eq!(ring.find(derived.kid()).unwrap().status, KeyStatus::Retired);
    assert_eq!(
        verify(&ring, &envelope),
        crate::attestation::VerificationStatus::Verified
    );
}

/// An empty table adopts the derived key only if a stored attestation was
/// signed by it; attestations under some other key do not make the derived
/// key worth reviving.
#[tokio::test]
async fn attestations_under_another_key_do_not_adopt_the_derived_key() {
    let db = migrated_memory_database().await;
    store_attestation(&db, &InstanceKey::derived_from_secret("an-older-secret")).await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    assert_ne!(
        ring.active().kid(),
        InstanceKey::derived_from_secret(JWT).kid()
    );
    assert_eq!(ring.keys().len(), 1);
    assert_eq!(ring.keys()[0].origin, KeyOrigin::Generated);
}

/// The migrated row of an instance whose `jwt_secret` changed since adoption:
/// its origin comes from the old table, not from comparing with today's secret.
#[tokio::test]
async fn a_carried_over_derived_key_is_identified_and_rotated_on_start() {
    let db = migrated_memory_database().await;
    let derived_from_old_secret = InstanceKey::derived_from_secret("the-secret-of-2025");
    carried_over(&db, &derived_from_old_secret, ORIGIN_LEGACY_DERIVED).await;

    let ring = establish(&db, JWT, ENC).await.unwrap();
    let old = ring
        .find(derived_from_old_secret.kid())
        .expect("kid filled in from the seed");
    assert_eq!(old.status, KeyStatus::Retired);
    assert_eq!(ring.keys().len(), 2);
    assert_eq!(
        ring.find(ring.active().kid()).unwrap().origin,
        KeyOrigin::Generated
    );
}

/// A carried-over random key keeps its identity: external verifiers see no
/// change from the migration.
#[tokio::test]
async fn a_carried_over_generated_key_keeps_its_kid() {
    let db = migrated_memory_database().await;
    let key = InstanceKey::generate();
    carried_over(&db, &key, ORIGIN_GENERATED).await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    assert_eq!(ring.active().kid(), key.kid());
    assert_eq!(ring.keys().len(), 1);
}

/// A3: CLI commands run against a database migrated offline, before any server
/// start, see a complete ring.
#[tokio::test]
async fn cli_load_fills_in_the_carried_over_identity() {
    let db = migrated_memory_database().await;
    let key = InstanceKey::generate();
    carried_over(&db, &key, ORIGIN_GENERATED).await;
    let ring = load(&db, ENC).await.unwrap();
    assert_eq!(ring.keys()[0].kid, key.kid());
    assert_eq!(stored(&db).await[0].kid.as_deref(), Some(key.kid()));
}

#[tokio::test]
async fn a_routine_rotation_keeps_the_old_key_verifying_and_briefly_published() {
    let db = migrated_memory_database().await;
    let before = establish(&db, JWT, ENC).await.unwrap();
    let envelope = crate::attestation::sign_statement(
        before.active(),
        &crate::attestation::Statement::new(
            "app.tar.gz",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            crate::attestation::PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
            serde_json::json!({ "builder": { "id": "https://forge.example" } }),
        ),
    )
    .unwrap();

    let rotation = rotate(&db, ENC, RotationReason::Routine).await.unwrap();
    assert_eq!(rotation.previous_kid, before.active().kid());
    let ring = load(&db, ENC).await.unwrap();
    assert_eq!(ring.active().kid(), rotation.key.kid());
    assert_eq!(
        verify(&ring, &envelope),
        crate::attestation::VerificationStatus::Verified
    );

    let now = chrono::Utc::now();
    let published =
        |at| -> Vec<String> { ring.published(at).iter().map(|k| k.kid.clone()).collect() };
    assert_eq!(published(now).len(), 2);
    assert_eq!(
        published(now + JWKS_OVERLAP + chrono::Duration::seconds(1)),
        vec![rotation.key.kid().to_owned()]
    );
    let rows = stored(&db).await;
    assert!(rows
        .iter()
        .filter(|r| r.status != STATUS_ACTIVE)
        .all(|r| r.seed_encrypted.is_none()));
}

#[tokio::test]
async fn a_compromise_rotation_revokes_the_old_key_at_once() {
    let db = migrated_memory_database().await;
    let before = establish(&db, JWT, ENC).await.unwrap();
    let envelope = crate::attestation::sign_statement(
        before.active(),
        &crate::attestation::Statement::new(
            "app.tar.gz",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            crate::attestation::PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
            serde_json::json!({ "builder": { "id": "https://forge.example" } }),
        ),
    )
    .unwrap();

    let rotation = rotate(&db, ENC, RotationReason::Compromised).await.unwrap();
    let ring = load(&db, ENC).await.unwrap();
    let published: Vec<_> = ring
        .published(chrono::Utc::now())
        .iter()
        .map(|k| k.kid.clone())
        .collect();
    assert_eq!(published, vec![rotation.key.kid().to_owned()]);
    assert_eq!(
        ring.find(before.active().kid()).unwrap().status,
        KeyStatus::Revoked
    );
    assert_eq!(
        verify(&ring, &envelope),
        crate::attestation::VerificationStatus::Revoked
    );
}

/// A6 and the follow-up case: the derived key retired on upgrade can be
/// revoked later; the active key cannot be revoked without a replacement.
#[tokio::test]
async fn revoke_refuses_the_active_key_and_revokes_a_retired_one() {
    let db = migrated_memory_database().await;
    let first = establish(&db, JWT, ENC).await.unwrap();
    let error = revoke(&db, ENC, first.active().kid()).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("--reason compromised"),
        "{error:#}"
    );

    rotate(&db, ENC, RotationReason::Routine).await.unwrap();
    revoke(&db, ENC, first.active().kid()).await.unwrap();
    let ring = load(&db, ENC).await.unwrap();
    assert_eq!(
        ring.find(first.active().kid()).unwrap().status,
        KeyStatus::Revoked
    );
    assert!(revoke(&db, ENC, "0000000000000000").await.is_err());
}

/// The database, not the code, keeps a second key from becoming active.
#[tokio::test]
async fn a_second_active_key_is_rejected_by_the_database() {
    let db = migrated_memory_database().await;
    establish(&db, JWT, ENC).await.unwrap();
    let extra = InstanceKey::generate();
    assert!(insert_key(&db, &extra, ORIGIN_GENERATED, &cipher())
        .await
        .is_err());
    assert_eq!(stored(&db).await.len(), 1);
}

/// Two replicas rotating the same key: one wins, the other learns it lost.
#[tokio::test]
async fn concurrent_rotations_of_one_key_produce_one_new_key() {
    let db = migrated_memory_database().await;
    establish(&db, JWT, ENC).await.unwrap();
    let active = ops::find_active(&db).await.unwrap().unwrap();
    let key = cipher();
    let (a, b) = tokio::join!(
        rotate_row(&db, &key, &active, RotationReason::Routine),
        rotate_row(&db, &key, &active, RotationReason::Routine),
    );
    let won = [a.unwrap(), b.unwrap()].into_iter().flatten().count();
    assert_eq!(won, 1);
    let rows = stored(&db).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r.status == STATUS_ACTIVE).count(), 1);
}

/// R1: a key rotated by the CLI stops signing in a running process once its
/// cache expires, without a restart.
#[tokio::test]
async fn a_running_process_picks_up_a_rotation_after_the_cache_expires() {
    let db = migrated_memory_database().await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    let old = ring.active().kid().to_owned();
    let keys = InstanceKeys::new(db.clone(), ENC, ring).with_ttl(Duration::ZERO);

    let rotation = rotate(&db, ENC, RotationReason::Compromised).await.unwrap();
    let current = keys.current().await.unwrap();
    assert_eq!(current.active().kid(), rotation.key.kid());
    assert!(current
        .published(chrono::Utc::now())
        .iter()
        .all(|k| k.kid != old));
}

/// Within the TTL the cached ring is served, which bounds the window the
/// documentation promises.
#[tokio::test]
async fn the_cached_ring_is_served_within_the_ttl() {
    let db = migrated_memory_database().await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    let old = ring.active().kid().to_owned();
    let keys = InstanceKeys::new(db.clone(), ENC, ring);
    rotate(&db, ENC, RotationReason::Routine).await.unwrap();
    assert_eq!(keys.current().await.unwrap().active().kid(), old);
    assert_ne!(keys.fresh().await.unwrap().active().kid(), old);
}

#[tokio::test]
async fn the_seed_is_encrypted_at_rest() {
    let db = migrated_memory_database().await;
    let ring = establish(&db, JWT, ENC).await.unwrap();
    let rows = stored(&db).await;
    let sealed = rows[0].seed_encrypted.as_deref().unwrap();
    assert!(encryption::looks_like_ciphertext(sealed));
    assert!(!sealed.contains(&hex::encode(ring.active().seed())));
    assert!(load(&db, "the-wrong-at-rest-key").await.is_err());
}

/// Attestations signed by a key that was never recorded are named on start.
#[tokio::test]
async fn establishing_warns_about_attestations_signed_by_an_unrecorded_key() {
    let db = migrated_memory_database().await;
    let earlier = InstanceKey::derived_from_secret("the-secret-before-the-rotation");
    store_attestation(&db, &earlier).await;

    let (logs, _guard) = crate::test_support::CapturedLogs::capture();
    let ring = establish(&db, JWT, ENC).await.unwrap();
    let rendered = logs.rendered();
    assert!(rendered.contains(earlier.kid()), "{rendered}");
    assert!(
        !rendered.contains(&hex::encode(ring.active().seed())),
        "{rendered}"
    );
}

/// `{:?}` on application state must not print the private half.
#[test]
fn debug_shows_the_kid_and_nothing_secret() {
    let key = InstanceKey::derived_from_secret(JWT);
    let rendered = format!("{key:?}");
    assert!(rendered.contains(key.kid()));
    assert!(!rendered.contains(&hex::encode(key.seed())));
}

/// The lifecycle on PostgreSQL or MySQL: the compare-and-set, the rotation
/// transaction, the single-active slot and the row-state CHECKs are dialect
/// behaviour, and every test above runs on SQLite. Leaves the table empty.
#[tokio::test]
#[ignore = "needs PLOMBIR_GIT_TEST_DATABASE_URL pointing at a disposable server database"]
async fn server_instance_key_lifecycle_holds_on_a_server_database() {
    use sea_orm::ConnectionTrait;

    let url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(!url.starts_with("sqlite"), "SQLite is covered above");
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to the disposable server database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db.execute_unprepared("DELETE FROM instance_signing_keys")
        .await
        .unwrap();
    // A secret no other step of the job uses, so no stored attestation there
    // is signed by its derived key.
    let jwt = format!(
        "server-lifecycle-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );

    let first = establish(&db, &jwt, ENC).await.unwrap();
    assert_eq!(first.keys().len(), 1);
    assert_eq!(first.keys()[0].origin, KeyOrigin::Generated);
    let first = first.active().kid().to_owned();

    let second = rotate(&db, ENC, RotationReason::Routine).await.unwrap();
    let retired = ops::find_by_kid(&db, &first).await.unwrap().unwrap();
    assert_eq!(retired.status, STATUS_RETIRED);
    assert!(retired.seed_encrypted.is_none() && retired.active_slot.is_none());

    revoke(&db, ENC, &first).await.unwrap();
    let third = rotate(&db, ENC, RotationReason::Compromised).await.unwrap();
    let ring = load(&db, ENC).await.unwrap();
    assert_eq!(ring.active().kid(), third.key.kid());
    assert_eq!(ring.find(&first).unwrap().status, KeyStatus::Revoked);
    assert_eq!(
        ring.find(second.key.kid()).unwrap().status,
        KeyStatus::Revoked
    );

    // Two replicas rotating the same key at once, on separate connections.
    let active = ops::find_active(&db).await.unwrap().unwrap();
    let key = cipher();
    let (a, b) = tokio::join!(
        rotate_row(&db, &key, &active, RotationReason::Routine),
        rotate_row(&db, &key, &active, RotationReason::Routine),
    );
    assert_eq!([a.unwrap(), b.unwrap()].into_iter().flatten().count(), 1);
    let rows = stored(&db).await;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows.iter().filter(|r| r.status == STATUS_ACTIVE).count(), 1);

    // A second active key, and every row state the code does not expect.
    assert!(
        insert_key(&db, &InstanceKey::generate(), ORIGIN_GENERATED, &key)
            .await
            .is_err()
    );
    for (status, slot, seed, origin) in [
        ("active", "NULL", "'sealed'", "generated"),
        // Slot 2: slot 1 is taken, so 1 would test the unique index instead.
        ("retired", "2", "NULL", "generated"),
        ("retired", "NULL", "'sealed'", "generated"),
        ("paused", "NULL", "NULL", "generated"),
        ("retired", "NULL", "NULL", "imported"),
    ] {
        let sql = format!(
            "INSERT INTO instance_signing_keys \
             (kid, seed_encrypted, origin, status, active_slot, created_at) \
             VALUES ('ffffffffffffffff', {seed}, '{origin}', '{status}', {slot}, \
             CURRENT_TIMESTAMP)"
        );
        assert!(
            db.execute_unprepared(&sql).await.is_err(),
            "accepted: {status} slot={slot} seed={seed} origin={origin}"
        );
    }
    assert_eq!(stored(&db).await.len(), 4);

    db.execute_unprepared("DELETE FROM instance_signing_keys")
        .await
        .unwrap();
}
