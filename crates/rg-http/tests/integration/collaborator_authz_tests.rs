//! Authorization regressions for the collaborator endpoints.
//!
//! Two separate failures used to live here, and both are covered below:
//!
//!   1. `POST`, `PATCH` and `DELETE` under `.../collaborators` stopped at
//!      `extract_user_id(...).is_none()`. That is authentication, not
//!      authorization: any account holding a valid token could grant *itself*
//!      `admin` on any repository — private ones included — in one request, and
//!      strip the collaborators off repositories it had nothing to do with.
//!
//!   2. `PATCH .../collaborators/{id}` discarded both path segments
//!      (`Path((_owner, _repo, id))`) and acted on `id` alone, which is a
//!      global `repo_collaborators` primary key. Even once a permission check
//!      was in place, an admin of one repository would still reach the access
//!      list of another until the row is matched against the repo the check was
//!      about.

use crate::common::{create_repo, register_full, spawn_test_app};

// ── helpers ──────────────────────────────────────────────────────────────────

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

/// Add a collaborator through the owner's own token and return the row id.
async fn add_collaborator(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    username: &str,
    permission: &str,
) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": permission}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_collaborator '{username}' failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn permission_of(base: &str, token: &str, owner: &str, repo: &str, user_id: i64) -> String {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "list_collaborators failed");
    let collaborators: Vec<serde_json::Value> = resp.json().await.unwrap();
    let collab = collaborators
        .into_iter()
        .find(|collab| collab["user_id"].as_i64() == Some(user_id))
        .unwrap_or_else(|| panic!("user {user_id} is not a collaborator of {owner}/{repo}"));
    collab["permission"].as_str().unwrap().to_string()
}

// ── 1. authenticated is not authorized ───────────────────────────────────────

/// The escalation in its shortest form: one POST turns an outsider into an
/// admin of a private repository it could not even read a moment earlier.
#[tokio::test]
async fn outsider_cannot_grant_itself_access_to_a_private_repo() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) =
        register_full(&base, "cauthz_owner", "cauthz_o@example.com").await;
    let (outsider_token, outsider_id) =
        register_full(&base, "cauthz_outsider", "cauthz_x@example.com").await;
    create_private_repo(&base, &owner_token, "secret").await;

    let client = reqwest::Client::new();
    let grant = client
        .post(format!(
            "{base}/api/v1/repos/cauthz_owner/secret/collaborators"
        ))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"user_id": outsider_id, "permission": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        grant.status(),
        403,
        "an outsider with a valid token granted itself access to a private repo"
    );

    // And the refusal is real, not just a status code: the repo stays unreadable.
    let read = client
        .get(format!(
            "{base}/api/v1/repos/cauthz_owner/secret/collaborators"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        read.status(),
        403,
        "the private repo became readable anyway"
    );
}

#[tokio::test]
async fn outsider_cannot_update_a_collaborator_permission() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) =
        register_full(&base, "cupd_owner", "cupd_owner@example.com").await;
    let (_alice_token, alice_id) =
        register_full(&base, "cupd_alice", "cupd_alice@example.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "cupd_outsider", "cupd_x@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    let row_id = add_collaborator(
        &base,
        &owner_token,
        "cupd_owner",
        "proj",
        "cupd_alice",
        "read",
    )
    .await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/cupd_owner/proj/collaborators/{row_id}"
        ))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"permission": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "an outsider rewrote someone else's collaborator permission"
    );

    assert_eq!(
        permission_of(&base, &owner_token, "cupd_owner", "proj", alice_id).await,
        "read",
        "the permission was changed despite the refusal"
    );
}

#[tokio::test]
async fn outsider_cannot_remove_a_collaborator() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) =
        register_full(&base, "crem_owner", "crem_owner@example.com").await;
    let (_alice_token, alice_id) =
        register_full(&base, "crem_alice", "crem_alice@example.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "crem_outsider", "crem_x@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    add_collaborator(
        &base,
        &owner_token,
        "crem_owner",
        "proj",
        "crem_alice",
        "write",
    )
    .await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/repos/crem_owner/proj/collaborators/{alice_id}"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "an outsider removed a collaborator from someone else's repo"
    );

    assert_eq!(
        permission_of(&base, &owner_token, "crem_owner", "proj", alice_id).await,
        "write",
        "the collaborator was removed despite the refusal"
    );
}

/// A collaborator with `write` is not an administrator — write access to the
/// contents of a repo must not carry the right to hand out access to it.
#[tokio::test]
async fn write_collaborator_cannot_manage_collaborators() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cwri_owner", "cwri_o@example.com").await;
    let (writer_token, _writer_id) =
        register_full(&base, "cwri_writer", "cwri_w@example.com").await;
    let (_bob_token, bob_id) = register_full(&base, "cwri_bob", "cwri_bob@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    add_collaborator(
        &base,
        &owner_token,
        "cwri_owner",
        "proj",
        "cwri_writer",
        "write",
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/cwri_owner/proj/collaborators"))
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({"user_id": bob_id, "permission": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "a write collaborator handed out repository access"
    );
}

// ── 2. the id is global — scope it to the repo that was authorized ───────────

/// Being an admin somewhere is not being an admin everywhere: the row id in the
/// path belongs to another repository, so it has to read as absent.
#[tokio::test]
async fn repo_admin_cannot_reach_another_repos_collaborator_row() {
    let base = spawn_test_app().await;
    let (a_owner_token, _a_owner_id) = register_full(&base, "cx_a_owner", "cx_a@example.com").await;
    let (b_owner_token, _b_owner_id) = register_full(&base, "cx_b_owner", "cx_b@example.com").await;
    let (mallory_token, _mallory_id) = register_full(&base, "cx_mallory", "cx_m@example.com").await;
    let (_victim_token, victim_id) = register_full(&base, "cx_victim", "cx_v@example.com").await;

    // Mallory is a genuine admin of repo A ...
    create_repo(&base, &a_owner_token, "repo_a").await;
    add_collaborator(
        &base,
        &a_owner_token,
        "cx_a_owner",
        "repo_a",
        "cx_mallory",
        "admin",
    )
    .await;

    // ... and the target row lives in repo B, which Mallory has nothing to do with.
    create_private_repo(&base, &b_owner_token, "repo_b").await;
    let victim_row = add_collaborator(
        &base,
        &b_owner_token,
        "cx_b_owner",
        "repo_b",
        "cx_victim",
        "read",
    )
    .await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/cx_a_owner/repo_a/collaborators/{victim_row}"
        ))
        .bearer_auth(&mallory_token)
        .json(&serde_json::json!({"permission": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "an admin of repo A rewrote a collaborator row of repo B"
    );

    assert_eq!(
        permission_of(&base, &b_owner_token, "cx_b_owner", "repo_b", victim_id).await,
        "read",
        "repo B's access list was modified from repo A"
    );
}

// ── 3. the legitimate paths still work ───────────────────────────────────────

#[tokio::test]
async fn owner_and_repo_admin_can_manage_collaborators() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cok_owner", "cok_owner@example.com").await;
    let (admin_token, _admin_id) = register_full(&base, "cok_admin", "cok_admin@example.com").await;
    let (_bob_token, bob_id) = register_full(&base, "cok_bob", "cok_bob@example.com").await;
    create_repo(&base, &owner_token, "proj").await;

    // The owner promotes a second administrator ...
    add_collaborator(
        &base,
        &owner_token,
        "cok_owner",
        "proj",
        "cok_admin",
        "admin",
    )
    .await;

    // ... who can then run the full add / update / remove cycle themselves.
    let bob_row =
        add_collaborator(&base, &admin_token, "cok_owner", "proj", "cok_bob", "read").await;

    let client = reqwest::Client::new();
    let patch = client
        .patch(format!(
            "{base}/api/v1/repos/cok_owner/proj/collaborators/{bob_row}"
        ))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        patch.status(),
        200,
        "a repo admin could not update a permission"
    );
    assert_eq!(
        permission_of(&base, &owner_token, "cok_owner", "proj", bob_id).await,
        "write"
    );

    let delete = client
        .delete(format!(
            "{base}/api/v1/repos/cok_owner/proj/collaborators/{bob_id}"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        delete.status(),
        204,
        "a repo admin could not remove a collaborator"
    );
}

/// An id that never existed and one that belongs elsewhere must be the same
/// answer, otherwise the 404/400 split turns the id space into an oracle.
#[tokio::test]
async fn unknown_collaborator_row_is_not_found() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cnf_owner", "cnf_owner@example.com").await;
    create_repo(&base, &owner_token, "proj").await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/cnf_owner/proj/collaborators/424242"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    // A fixed description with no `db: ...` chain behind it (H-05).
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["message"], "collaborator not found");
}

/// The rejected-permission path is a client error, and must stay one now that
/// the handler forwards service errors instead of flattening them into 400.
#[tokio::test]
async fn invalid_permission_is_a_bad_request() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cbad_owner", "cbad_o@example.com").await;
    let (_alice_token, _alice_id) = register_full(&base, "cbad_alice", "cbad_a@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    let row_id = add_collaborator(
        &base,
        &owner_token,
        "cbad_owner",
        "proj",
        "cbad_alice",
        "read",
    )
    .await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/cbad_owner/proj/collaborators/{row_id}"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"permission": "root"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ── 4. a delete that removed nothing is not a success ────────────────────────
//
// `DELETE .../collaborators/{id}` keys off `users.id`, while `PATCH` on the
// identical URL keys off the `repo_collaborators` row id — axum will not mount
// two verbs with differently named segments in one position, so the spec cannot
// spell the difference out. That makes the mistake easy and, while the delete
// discarded `rows_affected`, invisible: passing the row id answered `204` for a
// delete that removed nothing, or removed whoever happened to own that number
// as a `users.id`.

/// A user id that belongs to nobody must not read as "removed".
#[tokio::test]
async fn removing_an_unknown_collaborator_is_not_found() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cdel_owner", "cdel_o@example.com").await;
    create_repo(&base, &owner_token, "proj").await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/repos/cdel_owner/proj/collaborators/424242"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "removing a collaborator that does not exist reported success"
    );
    // A fixed description with no `db: ...` chain behind it (H-05).
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["message"], "collaborator not found");
}

/// The second removal of the same user has nothing left to remove.
#[tokio::test]
async fn removing_the_same_collaborator_twice_is_not_found() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "cdup_owner", "cdup_o@example.com").await;
    let (_alice_token, alice_id) = register_full(&base, "cdup_alice", "cdup_a@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    add_collaborator(
        &base,
        &owner_token,
        "cdup_owner",
        "proj",
        "cdup_alice",
        "write",
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/cdup_owner/proj/collaborators/{alice_id}");

    let first = client
        .delete(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 204, "the removal itself stopped working");

    let second = client
        .delete(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        404,
        "removing an already-removed collaborator reported success"
    );
}

/// The `PATCH` key passed to `DELETE`: a row id that matches no `users.id` in
/// this repository. The answer is `404`, and no other collaborator is touched.
#[tokio::test]
async fn removing_a_collaborator_by_row_id_is_not_found() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "crow_owner", "crow_o@example.com").await;
    let (_alice_token, alice_id) = register_full(&base, "crow_alice", "crow_a@example.com").await;
    let (_bob_token, bob_id) = register_full(&base, "crow_bob", "crow_bob@example.com").await;
    create_repo(&base, &owner_token, "proj").await;
    let alice_row = add_collaborator(
        &base,
        &owner_token,
        "crow_owner",
        "proj",
        "crow_alice",
        "write",
    )
    .await;
    add_collaborator(
        &base,
        &owner_token,
        "crow_owner",
        "proj",
        "crow_bob",
        "read",
    )
    .await;

    // The point of the test is a row id that is nobody's user id here; if the
    // fixture ever produces a collision the delete below would legitimately
    // remove someone, so fail loudly instead of asserting the wrong thing.
    assert!(
        alice_row != alice_id && alice_row != bob_id,
        "fixture: row id {alice_row} collides with a collaborator user id"
    );

    let resp = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/repos/crow_owner/proj/collaborators/{alice_row}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "a delete keyed by the row id removed nothing and reported success"
    );

    // Both collaborators are still there — including the one whose row id was
    // in the path.
    assert_eq!(
        permission_of(&base, &owner_token, "crow_owner", "proj", alice_id).await,
        "write"
    );
    assert_eq!(
        permission_of(&base, &owner_token, "crow_owner", "proj", bob_id).await,
        "read"
    );
}
