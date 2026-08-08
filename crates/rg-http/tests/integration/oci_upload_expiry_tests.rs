//! card_487dc1247247: an abandoned `docker push` must stop costing disk.
//!
//! `create_upload` has always stamped a 24h `expires_at` on the session row, and
//! nothing ever read it. A dropped connection — a client that crashed, a network
//! that blinked, the ordinary way a push ends — left the row *and* a staging
//! directory holding every layer byte already sent, and nothing came back for
//! either.
//!
//! The refusal half is what makes the test mean anything. Collecting the expired
//! session proves only that something ran; a sweep that deleted every session it
//! found would pass that half and destroy a push in flight. So a live session of
//! the same repository is created in the same fixture and asserted intact.

use crate::common::{create_repo, register_full, spawn_test_app_with_state};

/// Start a blob upload and send one chunk, leaving a session with real bytes
/// staged behind it — the state an interrupted push leaves.
async fn start_a_push(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> String {
    let client = reqwest::Client::new();
    let start = client
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "start upload failed");
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location
        .rsplit('/')
        .next()
        .expect("uuid in Location")
        .to_string();

    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");
    uuid
}

#[tokio::test]
async fn an_expired_upload_session_loses_its_row_and_its_staged_chunks() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "oci_ttl", "oci_ttl@example.com").await;
    create_repo(&base, &token, "leaky").await;

    let abandoned = start_a_push(&base, &token, "oci_ttl", "leaky", b"abandoned layer").await;
    let live = start_a_push(&base, &token, "oci_ttl", "leaky", b"a push still going").await;

    let abandoned_file = state.oci_storage.upload_file("oci_ttl", "leaky", &abandoned);
    let live_file = state.oci_storage.upload_file("oci_ttl", "leaky", &live);
    assert!(
        abandoned_file.is_file() && live_file.is_file(),
        "the fixture must stage bytes for both sessions"
    );

    // Age one session past its TTL. Reaching into the column rather than
    // waiting 24 hours is the point of having a column.
    let expired_row = rg_db::ops::oci_ops::find_upload(&db, &abandoned)
        .await
        .unwrap()
        .expect("the abandoned session must have a row");
    let mut aged: rg_db::entities::oci_upload::ActiveModel = expired_row.into();
    aged.expires_at = sea_orm::Set(chrono::Utc::now() - chrono::Duration::hours(1));
    sea_orm::ActiveModelTrait::update(aged, &db)
        .await
        .expect("age the session past its TTL");

    let summary = rg_http::api::ci_retention::cleanup_expired_storage(&state, None)
        .await
        .expect("the retention sweep must run");
    assert_eq!(summary.oci_uploads_deleted, 1, "{summary:?}");
    assert_eq!(summary.failures, 0, "{summary:?}");

    assert!(
        rg_db::ops::oci_ops::find_upload(&db, &abandoned)
            .await
            .unwrap()
            .is_none(),
        "the expired session's row survived the sweep"
    );
    assert!(
        !abandoned_file.exists(),
        "the expired session's staged chunks are still on disk at {}",
        abandoned_file.display()
    );

    // The refusal half: same repository, same sweep, still in flight.
    assert!(
        rg_db::ops::oci_ops::find_upload(&db, &live)
            .await
            .unwrap()
            .is_some(),
        "the sweep took a session that had not expired"
    );
    assert!(
        live_file.is_file(),
        "the sweep destroyed the staged chunks of a push still in flight"
    );

    // And a second pass finds nothing left to do — a sweep that reports the
    // same work twice is one that is not actually removing it.
    let again = rg_http::api::ci_retention::cleanup_expired_storage(&state, None)
        .await
        .expect("the retention sweep must run again");
    assert_eq!(again.oci_uploads_deleted, 0, "{again:?}");
    assert_eq!(again.failures, 0, "{again:?}");
}
