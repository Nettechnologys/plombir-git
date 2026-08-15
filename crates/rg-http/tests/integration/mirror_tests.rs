//! Regression coverage for card_d33afb82797f: the mirror feature was dead.
//!
//! `m20260607_000001_create_mirrors` derived its table name from a bare
//! `#[derive(Iden)] enum Mirror`, producing `mirror`, while `entities::mirror`
//! reads `mirrors`. Every mirror endpoint answered 500 with `no such table:
//! mirrors`, and nothing caught it: the mirror handlers validate the remote URL
//! before touching the database, so the only mirror requests any test ever made
//! — the ones with a deliberately bad URL — returned an honest 400 without
//! reaching the table, and the background `sync_due_mirrors` logged its error
//! and moved on.
//!
//! These tests take the happy path all the way to the row, which is the one
//! thing that was never exercised.

use crate::common::{
    create_repo, register_full, spawn_test_app, spawn_test_app_with_db,
    spawn_test_app_with_db_and_repo_root, TEST_ENCRYPTION_KEY,
};

const REMOTE: &str = "https://example.com/upstream.git";

/// The core of the card: a mirror can be created and read back.
#[tokio::test]
async fn a_mirror_can_be_created_and_read_back() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-owner", "mirror-owner@example.com").await;
    create_repo(&base, &token, "mirrored").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-owner/mirrored/mirror");

    // Nothing configured yet — the read is an honest empty state, not a 500.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "a repository with no mirror must report 'not configured', not a server error"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 201, "creating a mirror failed: {body}");
    assert_eq!(body["url"], REMOTE);
    assert_eq!(body["sync_interval_seconds"], 3600);
    assert_eq!(body["status"], "active");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "reading the mirror back failed: {body}");
    assert_eq!(body["url"], REMOTE);
    assert_eq!(body["sync_interval_seconds"], 3600);
}

/// The rest of the surface — update and delete — reaches the same table, so it
/// was just as dead. Walking the whole lifecycle keeps every verb honest.
#[tokio::test]
async fn a_mirror_can_be_updated_and_deleted() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-admin", "mirror-admin@example.com").await;
    create_repo(&base, &token, "lifecycle").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-admin/lifecycle/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"sync_interval_seconds": 7200, "status": "inactive"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "updating the mirror failed: {body}");
    assert_eq!(body["sync_interval_seconds"], 7200);
    assert_eq!(body["status"], "inactive");

    // The change is persisted, not just echoed back.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["sync_interval_seconds"], 7200);
    assert_eq!(body["status"], "inactive");

    let resp = client
        .delete(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 204, "deleting the mirror failed");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "the deleted mirror is still readable");
}

/// card_c974e2bc1d73: the handlers used to answer `Json(json!(mirror))` — the
/// whole entity row, `password_encrypted` included. Every verb that hands a
/// mirror back is checked, because the leak was in the shared shape, not in one
/// handler.
#[tokio::test]
async fn a_mirror_reply_never_carries_the_stored_password() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-creds", "mirror-creds@example.com").await;
    create_repo(&base, &token, "credentialed").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-creds/credentialed/mirror");

    let created: serde_json::Value = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");

    for (verb, body) in [
        ("POST", created.clone()),
        ("GET", get_mirror(&client, &url, &token).await),
        (
            "PATCH",
            client
                .patch(&url)
                .bearer_auth(&token)
                .json(&serde_json::json!({"sync_interval_seconds": 7200}))
                .send()
                .await
                .expect("request")
                .json()
                .await
                .expect("json body"),
        ),
    ] {
        assert!(
            body.get("password_encrypted").is_none(),
            "{verb} leaked the stored mirror password: {body}"
        );
        assert!(
            !body.to_string().contains("hunter2"),
            "{verb} leaked the mirror password verbatim: {body}"
        );
        assert_eq!(
            body["has_credentials"], true,
            "{verb} must still report that a credential is configured: {body}"
        );
        assert_eq!(body["username"], "sync-bot", "{verb} lost the username");
    }
}

/// The other half of the same card: the reply is administrative, so reading it
/// takes push access. `require_read` let any visitor to a *public* repo pull the
/// remote's username and credential state out of the settings.
#[tokio::test]
async fn reading_a_mirror_requires_write_access() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "mirror-pub", "mirror-pub@example.com").await;
    let (outsider_token, _) = register_full(&base, "mirror-nosy", "mirror-nosy@example.com").await;
    create_repo(&base, &owner_token, "public-mirror").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-pub/public-mirror/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"url": REMOTE, "username": "sync-bot", "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client.get(&url).send().await.expect("request");
    assert_eq!(
        resp.status(),
        401,
        "an anonymous reader reached the mirror settings of a public repo"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        403,
        "a reader without push access reached the mirror settings"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the owner lost access to the mirror");
}

/// card_c29cb3416941: `password_encrypted` was a name, not a fact — the column
/// held the password exactly as the operator typed it, so a database dump was a
/// list of other people's remote credentials. Modelled on
/// `admin_sso_audit_tests`' check of `ldap_bind_password_enc`: it is not enough
/// that the stored value *differs* from the input, it has to decrypt back to it
/// with the server's key.
#[tokio::test]
async fn a_stored_mirror_password_is_encrypted_at_rest() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-rest", "mirror-rest@example.com").await;
    create_repo(&base, &token, "at-rest").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-rest/at-rest/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");

    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row")
        .password_encrypted
        .expect("a password was supplied, so one must be stored");
    assert_ne!(stored, "hunter2", "the password is stored in the clear");
    assert_eq!(
        rg_core::auth::encryption::decrypt(&stored, &key).expect("decrypt"),
        "hunter2",
        "the stored ciphertext is not the password we were given"
    );

    // A replacement password goes through the same door...
    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": "correct horse"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "updating the credential failed");
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row")
        .password_encrypted
        .expect("the replacement password must be stored");
    assert_ne!(stored, "correct horse", "the update stored it in the clear");
    assert_eq!(
        rg_core::auth::encryption::decrypt(&stored, &key).expect("decrypt"),
        "correct horse"
    );

    // A request that says nothing about the password leaves it alone. This is
    // the half the web form leans on: its password box is blank on load because
    // the stored credential is never sent back, so saving a new sync interval
    // must not be read as "clear the credential" (card_fad4c3af64f9).
    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"sync_interval_seconds": 7200}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "updating the interval failed: {body}");
    assert_eq!(
        body["has_credentials"], true,
        "saving an unrelated field dropped the stored credential: {body}"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("query")
            .expect("the mirror row")
            .password_encrypted
            .and_then(|stored| rg_core::auth::encryption::decrypt(&stored, &key).ok()),
        Some("correct horse".to_string()),
        "the credential did not survive an unrelated update"
    );

    // ...and an empty one is how a credential is taken back off a mirror.
    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": ""}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "clearing the credential failed: {body}");
    assert_eq!(
        body["has_credentials"], false,
        "the mirror still reports a stored credential: {body}"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("query")
            .expect("the mirror row")
            .password_encrypted
            .is_none(),
        "the cleared credential is still in the database"
    );
}

/// The username follows the password's rule, and for the same reason: the
/// settings form sends every field it displays on every save, so a username the
/// operator deleted arrives as `""`. Stored verbatim it would be an empty name
/// nothing can tell apart from a real one — `create_mirror` has always filtered
/// it, `update_mirror` did not (card_fad4c3af64f9).
#[tokio::test]
async fn an_emptied_mirror_username_clears_the_column() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-user", "mirror-user@example.com").await;
    create_repo(&base, &token, "anon").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-user/anon/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");
    assert_eq!(created["username"], "sync-bot");

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"username": ""}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "clearing the username failed: {body}");
    assert!(
        body["username"].is_null(),
        "the emptied username came back as a value: {body}"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("query")
            .expect("the mirror row")
            .username,
        None,
        "the emptied username is still in the database"
    );
}

/// card_e71e1ba04ae7: the encryption above only covers the credential the
/// operator typed into the *password* field. Typed into the URL instead —
/// `https://user:token@host/repo.git`, the form GitHub and GitLab both document
/// — it used to be stored verbatim in `mirrors.url`, a plaintext column, and
/// handed straight back out by every mirror endpoint.
///
/// This is that acceptance check, from the client's side: create through the
/// API, then read the row *and* the response.
#[tokio::test]
async fn a_credential_typed_into_the_mirror_url_reaches_neither_the_row_nor_the_reply() {
    const URL_TOKEN: &str = "ghp-URL-SECRET-TOKEN";

    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-url", "mirror-url@example.com").await;
    create_repo(&base, &token, "url-credential").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-url/url-credential/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": format!("https://sync-bot:{URL_TOKEN}@example.com/upstream.git"),
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let created: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 201,
        "a pasted credential URL must still register a mirror: {created}"
    );

    // The reply, which is what the settings page renders.
    assert!(
        !created.to_string().contains(URL_TOKEN),
        "the create reply handed the token back: {created}"
    );
    assert_eq!(created["url"], "https://example.com/upstream.git");
    assert_eq!(created["username"], "sync-bot");
    assert_eq!(
        created["has_credentials"], true,
        "the credential was dropped instead of stored: {created}"
    );

    // The row, which is what a database dump holds.
    let repo_id = created["repo_id"].as_i64().expect("repo_id");
    let row = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        !row.url.contains(URL_TOKEN),
        "the token is still in `mirrors.url`: {}",
        row.url
    );
    let stored = row
        .password_encrypted
        .expect("the credential moved to the encrypted column, so it is there");
    assert_ne!(stored, URL_TOKEN, "the token is stored in the clear");
    assert_eq!(
        rg_core::auth::encryption::decrypt(
            &stored,
            &rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY)
        )
        .expect("decrypt"),
        URL_TOKEN,
        "the mirror can no longer authenticate with what was pasted"
    );

    // And the read the settings page performs on every visit.
    let fetched = get_mirror(&client, &url, &token).await;
    assert!(
        !fetched.to_string().contains(URL_TOKEN),
        "GET returned the token: {fetched}"
    );
}

/// The other half of card_c29cb3416941: the credential is not just stored, it
/// is *read* by the sync. Proving that without a network is easy from the
/// failure side — a value the server cannot decrypt (one written under a
/// different secret, or a plaintext leftover from before encryption existed)
/// has to stop the sync with a message that says so, which it can only do by
/// having looked at the column at all. Before the fix the column was never
/// read and this sync would have sailed past it.
#[tokio::test]
async fn a_sync_reports_a_credential_it_cannot_decrypt() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-stale", "mirror-stale@example.com").await;
    create_repo(&base, &token, "stale-secret").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-stale/stale-secret/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");

    // Stand in for a row written before encryption existed: plaintext where
    // ciphertext is expected.
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    let mut model: rg_db::entities::mirror::ActiveModel = stored.into();
    model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    rg_db::ops::mirror_ops::update(&db, model)
        .await
        .expect("plant the legacy value");

    let resp = client
        .post(format!("{url}/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the sync trigger itself must not fail");

    let mirror = get_mirror(&client, &url, &token).await;
    assert_eq!(
        mirror["status"], "error",
        "an unusable credential left the mirror looking healthy: {mirror}"
    );
    let reason = mirror["last_sync_error"].as_str().unwrap_or_default();
    assert!(
        reason.contains("could not be decrypted"),
        "the sync did not report the unreadable credential: {mirror}"
    );
    assert!(
        !mirror.to_string().contains("hunter2"),
        "the sync echoed the stored credential back: {mirror}"
    );
}

/// The acceptance check of card_d2fd29942436: a mirror whose `next_sync_at` has
/// passed is refreshed **without anyone triggering it**.
///
/// `sync_due_mirrors` had no caller in the process, so `sync_interval_seconds`
/// and `next_sync_at` — both accepted by the API and both rendered in the
/// settings UI as a schedule — described something that never happened. Only
/// the "Sync now" button ever refreshed a mirror, and it was also the only
/// thing that ever moved `next_sync_at` forward.
///
/// The sync here is made to fail on the *credential*, the way
/// `a_sync_reports_a_credential_it_cannot_decrypt` does: that failure happens
/// before the SSRF guard and before `git`, so the test proves the scheduler
/// reached `sync_mirror` without going near a network or a clock-length git
/// timeout. Reaching the credential at all is the evidence — a mirror nothing
/// picked up keeps `last_sync_at = NULL` forever, which is exactly what the
/// second mirror below asserts.
#[tokio::test]
async fn a_due_mirror_is_synced_by_the_scheduler_with_no_manual_trigger() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _user_id) = register_full(&base, "mirror-cron", "mirror-cron@example.com").await;
    create_repo(&base, &token, "due").await;
    create_repo(&base, &token, "not-due").await;

    let client = reqwest::Client::new();
    let due_url = format!("{base}/api/v1/repos/mirror-cron/due/mirror");
    let pending_url = format!("{base}/api/v1/repos/mirror-cron/not-due/mirror");

    let mut ids = Vec::new();
    for url in [&due_url, &pending_url] {
        let resp = client
            .post(url)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "url": REMOTE,
                "username": "sync-bot",
                "password": "hunter2",
                "sync_interval_seconds": 3600,
            }))
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 201, "baseline create");
        let created: serde_json::Value = resp.json().await.expect("json body");
        ids.push(created["repo_id"].as_i64().expect("repo_id"));
    }
    let (due_repo_id, pending_repo_id) = (ids[0], ids[1]);

    // Wind the first mirror's schedule into the past — the one state change a
    // test cannot get by waiting an hour — and plant a credential the server
    // cannot read, so the sync fails at the decrypt step instead of reaching
    // out to the remote.
    let due = rg_db::ops::mirror_ops::find_by_repo_id(&db, due_repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    let mut model: rg_db::entities::mirror::ActiveModel = due.into();
    model.next_sync_at =
        sea_orm::ActiveValue::Set(Some(chrono::Utc::now() - chrono::Duration::seconds(60)));
    model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    rg_db::ops::mirror_ops::update(&db, model)
        .await
        .expect("make the mirror due");

    // The scheduler under test: the production spawn path, only with a poll
    // interval a test can sit through.
    let _handle = rg_core::mirror::scheduler::spawn_mirror_sync_with_shutdown(
        db.clone(),
        repo_root.clone(),
        TEST_ENCRYPTION_KEY.to_string(),
        rg_core::mirror::scheduler::MirrorSyncConfig {
            poll_interval_secs: 1,
            batch_size: 10,
        },
        None,
    )
    .expect("the scheduler starts with valid knobs");

    // Hang-guard, not a deadline: the first pass is one poll interval in, so a
    // working scheduler lands in ~1s and a dead one fails at any finite bound.
    let synced = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let mirror = rg_db::ops::mirror_ops::find_by_repo_id(&db, due_repo_id)
                .await
                .expect("query")
                .expect("the mirror row");
            if mirror.last_sync_at.is_some() {
                return mirror;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a due mirror was never picked up — nothing is running the schedule");

    assert_eq!(
        synced.status, "error",
        "the scheduled sync did not record its outcome: {synced:?}"
    );
    assert!(
        synced
            .last_sync_error
            .as_deref()
            .unwrap_or_default()
            .contains("could not be decrypted"),
        "the scheduled pass never reached the credential: {synced:?}"
    );
    assert!(
        synced
            .next_sync_at
            .is_some_and(|next| next > chrono::Utc::now()),
        "`next_sync_at` was not moved forward, so the row stays due forever: {synced:?}"
    );

    // The other half of "due": a mirror an hour out is left alone, or the sweep
    // is just syncing everything on every tick.
    let untouched = rg_db::ops::mirror_ops::find_by_repo_id(&db, pending_repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        untouched.last_sync_at.is_none(),
        "a mirror that is not due yet was synced anyway: {untouched:?}"
    );
}

async fn get_mirror(client: &reqwest::Client, url: &str, token: &str) -> serde_json::Value {
    client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body")
}

/// One mirror per repository — the unique index the create migration builds is
/// on `mirrors`, so this also proves the index followed the table rename.
#[tokio::test]
async fn a_second_mirror_for_the_same_repo_is_refused_as_a_conflict() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-dup", "mirror-dup@example.com").await;
    create_repo(&base, &token, "once-only").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-dup/once-only/mirror");
    let payload = serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&payload)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&payload)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a duplicate mirror is the repository's state, not a malformed request \
         and not a server failure"
    );

    // The genuine 400 on the same route has to stay reachable, or the change
    // above just moved every refusal onto one code.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "not-a-git-url", "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a remote the caller can fix by editing it is still a bad request"
    );
}

/// card_770723efaa96: a failed pass must not take the mirror out of the sweep
/// that is supposed to retry it.
///
/// `sync_mirror` records a failure by writing `status = "error"`, and
/// `list_due_sync` used to select `status = "active"` — so the sweep's own
/// bookkeeping deleted the row from its own queue. Not for a while: forever.
/// Nothing else re-selects mirrors, `last_sync_at` froze at the failure, and
/// the only way back was a hand-written `PATCH` the UI has no field for.
///
/// The pass is failed on the credential (a value the server cannot decrypt),
/// the same way the two tests above do it: that failure lands before the SSRF
/// guard and before `git`, so the test needs no network and no clock-length
/// timeout. Both passes go through `sync_due_mirrors` — the function the
/// scheduler tick calls, and which
/// `a_due_mirror_is_synced_by_the_scheduler_with_no_manual_trigger` already
/// ties to the tick — so "the next tick" is exercised without sleeping through
/// one.
#[tokio::test]
async fn a_mirror_whose_sync_failed_is_still_picked_up_by_the_next_sweep() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _user_id) = register_full(&base, "mirror-retry", "mirror-retry@example.com").await;
    create_repo(&base, &token, "keeps-trying").await;
    create_repo(&base, &token, "switched-off").await;

    let client = reqwest::Client::new();
    let retry_url = format!("{base}/api/v1/repos/mirror-retry/keeps-trying/mirror");
    let off_url = format!("{base}/api/v1/repos/mirror-retry/switched-off/mirror");

    let mut ids = Vec::new();
    for url in [&retry_url, &off_url] {
        let resp = client
            .post(url)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "url": REMOTE,
                "username": "sync-bot",
                "password": "hunter2",
                "sync_interval_seconds": 3600,
            }))
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 201, "baseline create");
        let created: serde_json::Value = resp.json().await.expect("json body");
        ids.push(created["repo_id"].as_i64().expect("repo_id"));
    }
    let (retry_repo_id, off_repo_id) = (ids[0], ids[1]);

    // The mirror under test: due now, and holding a credential the server
    // cannot read, so its pass fails.
    make_due(&db, retry_repo_id, true).await;

    // The control: due now as well, but switched off by the operator. It is
    // what keeps this test from passing on a "sweep everything" fix — the off
    // switch is the one thing the selection is still allowed to obey.
    make_due(&db, off_repo_id, true).await;
    let resp = client
        .patch(&off_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"status": "inactive"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "switching the control mirror off");

    // Pass one: the failure that used to be terminal.
    let synced =
        rg_core::mirror::service::sync_due_mirrors(&db, &repo_root, 10, TEST_ENCRYPTION_KEY)
            .await
            .expect("the sweep itself must not fail");
    assert_eq!(synced, 1, "the sweep skipped the mirror that was due");

    let after_first = rg_db::ops::mirror_ops::find_by_repo_id(&db, retry_repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert_eq!(
        after_first.status, "error",
        "the failed pass was not recorded, so this test proves nothing: {after_first:?}"
    );
    let first_sync_at = after_first.last_sync_at.expect("the pass ran");

    // The clock is the only thing between the mirror and its retry: wind
    // `next_sync_at` back the way an hour of real time would.
    make_due(&db, retry_repo_id, false).await;

    // Pass two — the whole point. Before the fix this returned 0: the row had
    // written itself out of `list_due_sync`.
    let synced =
        rg_core::mirror::service::sync_due_mirrors(&db, &repo_root, 10, TEST_ENCRYPTION_KEY)
            .await
            .expect("the sweep itself must not fail");
    assert_eq!(
        synced, 1,
        "a mirror whose last pass failed was never looked at again — the sweep \
         disabled itself on the first error"
    );

    let after_second = rg_db::ops::mirror_ops::find_by_repo_id(&db, retry_repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        after_second
            .last_sync_at
            .is_some_and(|at| at > first_sync_at),
        "the second sweep counted the mirror but never touched it: {after_second:?}"
    );
    assert!(
        after_second
            .next_sync_at
            .is_some_and(|next| next > chrono::Utc::now()),
        "the retry did not reschedule itself, so it would spin every tick: {after_second:?}"
    );

    // …and the operator's switch still means what it says.
    let off = rg_db::ops::mirror_ops::find_by_repo_id(&db, off_repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        off.last_sync_at.is_none(),
        "a mirror the operator switched off was synced anyway: {off:?}"
    );
}

/// Make a mirror due right now, optionally planting a credential the server
/// cannot decrypt so its pass fails before the SSRF guard and before `git`.
async fn make_due(db: &sea_orm::DatabaseConnection, repo_id: i64, break_credential: bool) {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    let mut model: rg_db::entities::mirror::ActiveModel = mirror.into();
    model.next_sync_at =
        sea_orm::ActiveValue::Set(Some(chrono::Utc::now() - chrono::Duration::seconds(60)));
    if break_credential {
        model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    }
    rg_db::ops::mirror_ops::update(db, model)
        .await
        .expect("make the mirror due");
}

/// The other half of card_770723efaa96: "Sync now" reported success on a mirror
/// it had not synced.
///
/// `sync_mirror` returns `Ok(false)` when it declines, `trigger_sync` dropped
/// that `false`, and the handler answered `200 {"status": "sync_triggered"}` —
/// so the one button an operator has said "done" for a mirror that never moved.
/// A switched-off mirror is now a refusal; a *failed* one is a real sync, which
/// is the case that used to be silently declined.
#[tokio::test]
async fn sync_now_refuses_a_switched_off_mirror_instead_of_reporting_success() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-button", "mirror-btn@example.com").await;
    create_repo(&base, &token, "off").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-button/off/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"status": "inactive"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "switching the mirror off");

    let resp = client
        .post(format!("{url}/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_ne!(
        status, 200,
        "the button reported a sync it did not perform: {body}"
    );
    assert_eq!(
        status, 409,
        "a mirror the operator switched off is the resource's state, not a \
         malformed request and not a server failure: {body}"
    );
    assert_ne!(
        body["status"], "sync_triggered",
        "the refusal still claims a sync was triggered: {body}"
    );

    // And the refusal is honest: nothing was synced.
    let untouched = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        untouched.last_sync_at.is_none(),
        "the mirror was synced despite being switched off: {untouched:?}"
    );

    // The button must still work on the state that used to be terminal: a
    // mirror whose last pass failed is exactly the one an operator presses it
    // on. It fails again here (the credential is unreadable), but it *runs* —
    // which is what "200" is allowed to mean.
    let mut model: rg_db::entities::mirror::ActiveModel = untouched.into();
    model.status = sea_orm::ActiveValue::Set("error".to_string());
    model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    rg_db::ops::mirror_ops::update(&db, model)
        .await
        .expect("plant a failed mirror");

    let resp = client
        .post(format!("{url}/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "a mirror that failed its last pass could not be retried by hand either"
    );
    let retried = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    assert!(
        retried.last_sync_at.is_some(),
        "the manual retry answered 200 without running: {retried:?}"
    );
}

/// The `status` column carries the operator's switch *and* the last outcome, so
/// the write side has to stay narrower than the read side: a caller may set the
/// switch, not describe a pass. Without this, `PATCH {"status": "erorr"}` was
/// stored verbatim and every later sweep read the typo as "switched on".
#[tokio::test]
async fn an_unknown_mirror_status_is_refused_rather_than_stored() {
    let base = spawn_test_app().await;
    let (token, _user_id) =
        register_full(&base, "mirror-status", "mirror-status@example.com").await;
    create_repo(&base, &token, "switch").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-status/switch/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    for rejected in ["erorr", "error", "paused"] {
        let resp = client
            .patch(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"status": rejected}))
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            400,
            "`status: {rejected}` was accepted as the mirror's switch position"
        );
    }

    // Both switch positions stay settable, or the guard above is just an outage.
    for accepted in ["inactive", "active"] {
        let resp = client
            .patch(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"status": accepted}))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(status, 200, "`status: {accepted}` was refused: {body}");
        assert_eq!(body["status"], accepted);
    }
}

/// card_3d4c7b8b27c8: the scheduler range-checks the knobs an instance admin
/// edits in a config file and did not range-check the one any repository owner
/// sets over HTTP — the same switch, one floor down.
///
/// `next_sync_at` is written as `now + sync_interval_seconds` and the sweep
/// selects on `next_sync_at <= now`, so a zero or negative interval makes the
/// row permanently due: every tick reaches out to a third-party host, and each
/// pass moves the schedule forward by nothing. The settings form multiplies
/// hours and never produces one, so the hole is the API alone.
#[tokio::test]
async fn an_interval_that_would_make_a_mirror_permanently_due_is_refused() {
    let base = spawn_test_app().await;
    let (token, _user_id) =
        register_full(&base, "mirror-interval", "mirror-interval@example.com").await;
    create_repo(&base, &token, "paced").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-interval/paced/mirror");

    for refused in [0, -1, 30] {
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": refused}))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status, 400,
            "`sync_interval_seconds: {refused}` was accepted: {body}"
        );
        assert!(
            body.to_string().contains("sync_interval_seconds"),
            "the refusal has to name the field the caller must edit: {body}"
        );

        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            404,
            "a refused create left a mirror row behind"
        );
    }

    // A legitimate interval still goes through, so the guard above is a floor
    // and not a closed door.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "a legitimate interval was refused");

    // And the update path carries the same floor: it writes the same column.
    for refused in [0, -1, 30] {
        let resp = client
            .patch(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"sync_interval_seconds": refused}))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status, 400,
            "`PATCH sync_interval_seconds: {refused}` was accepted: {body}"
        );

        let stored = get_mirror(&client, &url, &token).await;
        assert_eq!(
            stored["sync_interval_seconds"], 3600,
            "a refused update changed the stored interval anyway"
        );
    }
}
