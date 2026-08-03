//! An OCI blob-upload session belongs to the repository that opened it.
//!
//! `PATCH` and `PUT /v2/{owner}/{repo}/blobs/uploads/{uuid}` gate the
//! repository in the path and then resolve the session by the uuid alone. The
//! two halves never met: `require_access` proved the caller may push to
//! `{owner}/{repo}`, and the row that came back could be any repository's, so a
//! caller holding `push` on a repository of their own could name a stranger's
//! active session — rewriting its offset on `PATCH`, deleting its row on `PUT`.
//!
//! No bytes cross over either way: the staging file is keyed by
//! `{owner}/{repo}`, so the attacker's chunks land in the attacker's own
//! staging area. What crossed was control of the victim's push — the `Range`
//! their client resumes from, and whether their session exists at all.
//!
//! Both tests below check the *database row*, not the status code: the fix is
//! about which row a write lands on, and a handler that answers 404 while still
//! writing would pass a status-only assertion. Each also finishes by having the
//! session's owner carry on successfully, so a refusal proves the scope rather
//! than a fixture that never worked.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sha2::Digest as _;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

/// A uuid drawn from the same pool as a real session, belonging to nothing.
const ABSENT_UUID: &str = "8b1a9953-4c2b-4f0d-9a1e-7c3f5d2e6b40";

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(payload)))
}

/// Open an upload session, returning `(location, uuid)`.
async fn start_session(base: &str, token: &str, owner: &str, repo: &str) -> (String, String) {
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
        .and_then(|v| v.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location.rsplit('/').next().unwrap().to_string();
    (location, uuid)
}

/// `bytes_uploaded` of the session row, or `None` when the row is gone.
async fn session_bytes(db: &rg_db::DatabaseConnection, uuid: &str) -> Option<i64> {
    rg_db::entities::oci_upload::Entity::find()
        .filter(rg_db::entities::oci_upload::Column::Uuid.eq(uuid))
        .one(db)
        .await
        .expect("read the upload session row")
        .map(|row| row.bytes_uploaded)
}

/// Status plus body of a request, for comparing two answers as a whole.
async fn answer(response: reqwest::Response) -> (u16, serde_json::Value) {
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.expect("OCI error envelope");
    (status, body)
}

/// A push into one repository must not move another repository's upload offset.
///
/// The attacker opens a session of their own first, so their repository has an
/// `oci_repository` row and the session lookup cannot be refused for the
/// uninteresting reason that the registry has never heard of the namespace.
/// What is left is exactly the comparison under test.
#[tokio::test]
async fn a_push_elsewhere_cannot_move_another_repositorys_upload_offset() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (victim, _) =
        register_full(&base, "oci_scope_victim", "oci_scope_victim@example.com").await;
    create_repo(&base, &victim, "victim-image").await;
    let (attacker, _) =
        register_full(&base, "oci_scope_pusher", "oci_scope_pusher@example.com").await;
    create_repo(&base, &attacker, "pusher-image").await;

    let (victim_location, victim_uuid) =
        start_session(&base, &victim, "oci_scope_victim", "victim-image").await;
    let first = client
        .patch(format!("{base}{victim_location}"))
        .bearer_auth(&victim)
        .body(b"forgekeep-".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        first.status(),
        202,
        "the owner's own chunk must be accepted"
    );
    assert_eq!(session_bytes(&db, &victim_uuid).await, Some(10));

    let (_, attacker_uuid) =
        start_session(&base, &attacker, "oci_scope_pusher", "pusher-image").await;

    // The attacker names the victim's session through their own repository.
    let stolen = client
        .patch(format!(
            "{base}/v2/oci_scope_pusher/pusher-image/blobs/uploads/{victim_uuid}"
        ))
        .bearer_auth(&attacker)
        .body(vec![b'x'; 4096])
        .send()
        .await
        .unwrap();
    let stolen = answer(stolen).await;

    // The same request against a uuid that never existed anywhere.
    let absent = client
        .patch(format!(
            "{base}/v2/oci_scope_pusher/pusher-image/blobs/uploads/{ABSENT_UUID}"
        ))
        .bearer_auth(&attacker)
        .body(vec![b'x'; 4096])
        .send()
        .await
        .unwrap();
    let absent = answer(absent).await;

    assert_eq!(
        stolen, absent,
        "a session in someone else's repository must answer exactly like one that never \
         existed — a different status or message confirms the uuid to whoever guessed it"
    );
    assert_eq!(
        stolen.0, 404,
        "the refusal is 404, never 403: {:?}",
        stolen.1
    );
    assert_eq!(stolen.1["errors"][0]["code"], "BLOB_UPLOAD_UNKNOWN");

    assert_eq!(
        session_bytes(&db, &victim_uuid).await,
        Some(10),
        "the victim's recorded offset must be untouched — the `Range` their client resumes \
         from is read straight off this row"
    );
    assert_eq!(
        session_bytes(&db, &attacker_uuid).await,
        Some(0),
        "the attacker's own session must not have absorbed the write either"
    );

    // Baseline: the owner carries on, so the refusal above proves the scope and
    // not a session that had stopped working.
    let second = client
        .patch(format!("{base}{victim_location}"))
        .bearer_auth(&victim)
        .body(b"layer".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 202);
    assert_eq!(
        second
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|v| v.to_str().ok()),
        Some("0-14"),
        "the owner's chunk appends to their own staging file"
    );
    assert_eq!(session_bytes(&db, &victim_uuid).await, Some(15));
}

/// Finalizing a push into one repository must not end another repository's
/// session.
///
/// `PUT` succeeds off the staging file alone — which is the attacker's own —
/// and then deletes the session row by uuid. That delete was the cross-repo
/// write: the victim's `docker push` would be cancelled by a stranger's, and
/// its next chunk would be told the session is unknown.
#[tokio::test]
async fn a_finalize_elsewhere_cannot_delete_another_repositorys_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (victim, _) = register_full(&base, "oci_kill_victim", "oci_kill_victim@example.com").await;
    create_repo(&base, &victim, "victim-image").await;
    let (attacker, _) =
        register_full(&base, "oci_kill_pusher", "oci_kill_pusher@example.com").await;
    create_repo(&base, &attacker, "pusher-image").await;

    let payload = b"forgekeep-oci-layer";
    let (victim_location, victim_uuid) =
        start_session(&base, &victim, "oci_kill_victim", "victim-image").await;
    let staged = client
        .patch(format!("{base}{victim_location}"))
        .bearer_auth(&victim)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(staged.status(), 202);

    // The attacker's namespace is a live registry too.
    start_session(&base, &attacker, "oci_kill_pusher", "pusher-image").await;

    let attacker_payload = b"attacker-layer";
    let finalize = client
        .put(format!(
            "{base}/v2/oci_kill_pusher/pusher-image/blobs/uploads/{victim_uuid}"
        ))
        .query(&[("digest", sha256(attacker_payload).as_str())])
        .bearer_auth(&attacker)
        .body(attacker_payload.to_vec())
        .send()
        .await
        .unwrap();
    let (status, body) = answer(finalize).await;
    assert_eq!(
        status, 404,
        "finalizing a session of another repository must be refused: {body}"
    );
    assert_eq!(body["errors"][0]["code"], "BLOB_UPLOAD_UNKNOWN");

    assert_eq!(
        session_bytes(&db, &victim_uuid).await,
        Some(payload.len() as i64),
        "the victim's session row must still be there — deleting it cancels their push"
    );

    // Baseline: the victim finishes their own push, so the refusal above is
    // about ownership rather than a session that was already broken.
    let finish = client
        .put(format!("{base}{victim_location}"))
        .query(&[("digest", sha256(payload).as_str())])
        .bearer_auth(&victim)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, payload.len()).await;
    assert_eq!(
        session_bytes(&db, &victim_uuid).await,
        None,
        "a completed push consumes its own session"
    );
}
