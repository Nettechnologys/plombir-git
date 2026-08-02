//! Regression coverage for card_aa048c2956b1: a *failed git layer* must not be
//! reported as an *absent file* — and must not put the server's storage path in
//! the response body.
//!
//! `get_blob` and `get_commit_signature` had a single error arm,
//! `AppError::not_found(e)`, so every outcome of `get_blob_content` /
//! `verify_commit_signature` became a `404`. The first thing both helpers do is
//! `gix::open(repo_path).with_context(|| format!("failed to open repository:
//! {repo_path:?}"))`, so an unopenable repository answered `404` with the
//! absolute on-disk path of the repository in the body — and unlike the 5xx
//! variants, a `404` is *not* sanitized by `IntoResponse`, so that leak reached
//! the client verbatim (H-05).
//!
//! The break has to happen to the repository *on disk*: closing the database
//! pool fails the authentication lookup first, so the request would never reach
//! the git layer these tests are about.

use std::path::Path;

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

/// Make the bare repository unopenable while leaving its directory in place.
/// The missing-directory case is covered by the differential failure sweep;
/// this fixture keeps exercising the sibling "directory exists but is not a
/// usable git repository" path.
fn break_repository(bare: &Path) {
    std::fs::remove_dir_all(bare.join("objects")).expect("bare repo must have an objects dir");
    assert!(
        bare.exists(),
        "the repository directory must survive so the handler still reaches the git layer"
    );
    assert!(
        gix::open(bare).is_err(),
        "fixture must produce a repository that gix refuses to open, otherwise \
         the test exercises a different arm"
    );
}

/// Remove one object while retaining a repository gix can open and whose ref
/// still resolves. This separates an object-store failure from both a missing
/// ref and the broader "repository will not open" failure fixture above.
fn remove_loose_object(bare: &Path, object_id: &str) {
    let object_path = bare
        .join("objects")
        .join(&object_id[..2])
        .join(&object_id[2..]);
    assert!(
        object_path.exists(),
        "fixture must keep {object_id} as a loose object"
    );
    std::fs::remove_file(&object_path).expect("remove commit object");
    assert!(
        gix::open(bare).is_ok(),
        "fixture must leave repository opening intact so the log reaches its target-object read"
    );
}

/// Keep a loose object's name but replace its payload with an invalid commit.
/// This preserves index/prefix lookup while making the object-store read fail.
fn corrupt_loose_commit_object(bare: &Path, object_id: &str) {
    use std::io::Write as _;

    let object_path = bare
        .join("objects")
        .join(&object_id[..2])
        .join(&object_id[2..]);
    assert!(
        object_path.exists(),
        "fixture must start with a loose commit"
    );
    std::fs::remove_file(&object_path).expect("remove original commit object");
    let file = std::fs::File::create(&object_path).expect("overwrite commit object");
    let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
    encoder
        .write_all(b"commit 1\0x")
        .expect("write corrupt commit object");
    encoder.finish().expect("finish corrupt commit object");
}

/// `GET /repos/{owner}/{name}/blob/{path}`.
#[tokio::test]
async fn broken_repository_on_the_blob_endpoint_is_not_a_missing_file() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "blobfail-owner", "blobfail@example.com").await;
    create_repo(&base, &token, "blobfail-repo").await;
    let client = reqwest::Client::new();
    // The repository needs a commit for the baseline below to be about an
    // absent *file*: on an unborn HEAD the ref is what fails to resolve, and
    // since card_7cb31c61cee2 separated the two, that answers `ref not found`.
    commit_a_file(&client, &base, &token, "blobfail-owner", "blobfail-repo").await;

    let url = format!("{base}/api/v1/repos/blobfail-owner/blobfail-repo/blob/NOT-THERE.md");

    // Baseline on a healthy repository: the file really is absent → 404 with a
    // fixed message. Without it the assertion below cannot tell "we fixed the
    // status" from "this endpoint 500s on everything".
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent file is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "file not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    break_repository(&repo_root.join("blobfail-owner/blobfail-repo.git"));

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must be a 5xx, not {status} — a 404 \
         tells the client the file was deleted and puts nothing in the alerts \
         (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// `GET /repos/{owner}/{name}/commits/{sha}/signature` — same helper shape, same
/// single error arm.
#[tokio::test]
async fn broken_repository_on_the_signature_endpoint_is_not_a_missing_commit() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "sigfail-owner", "sigfail@example.com").await;
    create_repo(&base, &token, "sigfail-repo").await;

    // Well-formed but absent: the SHA-format guard runs before the git layer, so
    // a malformed SHA would never reach the arm under test.
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let url = format!("{base}/api/v1/repos/sigfail-owner/sigfail-repo/commits/{sha}/signature");
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent commit is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "commit not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    break_repository(&repo_root.join("sigfail-owner/sigfail-repo.git"));

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must be a 5xx, not {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The repository can remain open and the requested SHA can remain indexed
/// while decoding its commit object fails. That is a storage fault, not a
/// legitimate absent commit or an unsigned commit.
#[tokio::test]
async fn a_corrupt_signature_commit_object_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "sigobject-owner", "sigobject@example.com").await;
    create_repo(&base, &token, "sigobject-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "sigobject-owner", "sigobject-repo").await;

    let bare = repo_root.join("sigobject-owner/sigobject-repo.git");
    let repo = gix::open(&bare).expect("fixture repository must open");
    let commit_id = repo
        .head()
        .expect("HEAD must be readable")
        .try_into_peeled_id()
        .expect("HEAD must resolve")
        .expect("fixture must have a commit")
        .to_string();
    drop(repo);
    corrupt_loose_commit_object(&bare, &commit_id);

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/sigobject-owner/sigobject-repo/commits/{commit_id}/signature"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a corrupt commit object must be a 5xx, not {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// Put a file through the contents API so the repository has a commit — the
/// tree endpoint answers `200 {entries: []}` on an unborn HEAD (the empty-repo
/// path), so a freshly created repo would never reach the arms under test.
struct CommitFile<'a> {
    path: &'a str,
    content: &'a str,
    message: &'a str,
}

async fn commit_file(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    file: CommitFile<'_>,
) {
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/contents/{}",
            file.path
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": file.content,
            "message": file.message,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "the fixture needs a commit: {}",
        resp.text().await.unwrap_or_default()
    );
}

async fn commit_a_file(client: &reqwest::Client, base: &str, token: &str, owner: &str, repo: &str) {
    commit_file(
        client,
        base,
        token,
        owner,
        repo,
        CommitFile {
            path: "README.md",
            content: "# hello\n",
            message: "add README.md",
        },
    )
    .await;
}

/// card_784afeaf9603, the mirror of the bug above: on `GET .../tree` nothing
/// was typed, so the direction of the error inverted — a client that mistyped
/// `?ref=` got a `500` (and an `error`-level log line per miss) instead of a
/// `404`.
#[tokio::test]
async fn a_missing_ref_on_the_tree_endpoint_is_not_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "treeref-owner", "treeref@example.com").await;
    create_repo(&base, &token, "treeref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "treeref-owner", "treeref-repo").await;

    let url = format!("{base}/api/v1/repos/treeref-owner/treeref-repo/tree");

    // Baseline: the endpoint works on this repository, so a 404 below is about
    // the ref and not about the whole handler being broken.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the default ref must still list");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["entries"][0]["name"], "README.md",
        "fixture must have a committed file, got: {body}"
    );

    let resp = client
        .get(&url)
        .query(&[("ref", "no-such-branch")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 404,
        "a ref the client asked for and that does not exist is the client's \
         miss, not a server fault (body: {body})"
    );
    assert_eq!(
        body["error"]["message"], "ref not found",
        "the 404 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The other half: the typing must not swallow a genuine git-layer failure on
/// the same endpoint. A repository that will not open is still a 5xx — and
/// `is_empty_repo` must not paper over it with `200 {entries: []}` either.
#[tokio::test]
async fn a_broken_repository_on_the_tree_endpoint_is_still_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "treefail-owner", "treefail@example.com").await;
    create_repo(&base, &token, "treefail-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "treefail-owner", "treefail-repo").await;

    break_repository(&repo_root.join("treefail-owner/treefail-repo.git"));

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/treefail-owner/treefail-repo/tree"
        ))
        .query(&[("ref", "no-such-branch")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must stay a 5xx, not {status} — \
         answering 404 would tell the client its ref was wrong and put nothing \
         in the alerts (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The second client outcome of the same helper: `?path=` names a directory
/// that is not in the tree.
#[tokio::test]
async fn a_missing_sub_path_on_the_tree_endpoint_is_not_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "treepath-owner", "treepath@example.com").await;
    create_repo(&base, &token, "treepath-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "treepath-owner", "treepath-repo").await;

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/treepath-owner/treepath-repo/tree"
        ))
        .query(&[("path", "no/such/dir")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 404,
        "a sub-path that is not in the tree is a miss, not a fault (body: {body})"
    );
    assert_eq!(
        body["error"]["message"], "path not found",
        "the 404 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// And the outcome that used to reach `find_tree` with a blob oid: the path
/// exists, it just is not a directory. "Not found" would be a lie, so this one
/// is a 400 — the mirror of `get_blob_content`'s "path is not a file".
#[tokio::test]
async fn a_file_as_the_tree_sub_path_is_not_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "treeblob-owner", "treeblob@example.com").await;
    create_repo(&base, &token, "treeblob-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "treeblob-owner", "treeblob-repo").await;

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/treeblob-owner/treeblob-repo/tree"
        ))
        .query(&[("path", "README.md")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 400,
        "listing a file as a directory is the client's mistake, not a server \
         fault (body: {body})"
    );
    assert_eq!(
        body["error"]["message"], "path is not a directory",
        "the 400 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// card_511b208e49df: the log helper used to flatten every rev-parse error into
/// `Ok(vec![])`. A missing ref on a repository that demonstrably has history is
/// absence, not an empty history.
#[tokio::test]
async fn a_missing_ref_on_the_log_endpoint_is_not_an_empty_history() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "logref-owner", "logref@example.com").await;
    create_repo(&base, &token, "logref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "logref-owner", "logref-repo").await;

    let url = format!("{base}/api/v1/repos/logref-owner/logref-repo/log");

    // Non-vacuous baseline: this exact repository has one readable commit.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the default ref must still be readable");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["commits"].as_array().map(Vec::len),
        Some(1),
        "fixture must expose one commit, got: {body}"
    );

    let resp = client
        .get(&url)
        .query(&[("ref", "no-such-branch")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 404,
        "a missing ref must not look like a healthy empty history (body: {body})"
    );
    assert_eq!(
        body["error"]["message"], "ref not found",
        "the 404 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The legitimate empty result must survive the split above: a newly-created
/// bare repository has an unborn HEAD, not a server failure.
#[tokio::test]
async fn an_unborn_repository_still_has_an_empty_commit_log() {
    let (base, _) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "logempty-owner", "logempty@example.com").await;
    create_repo(&base, &token, "logempty-repo").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/logempty-owner/logempty-repo/log");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 200,
        "an unborn HEAD is a healthy empty history (body: {body})"
    );
    assert_eq!(
        body["commits"],
        serde_json::json!([]),
        "an unborn repository must return the documented empty list"
    );

    let resp = client
        .get(&url)
        .query(&[("ref", "no-such-branch")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 404,
        "only unborn HEAD is empty; an explicitly missing ref is still absent (body: {body})"
    );
    assert_eq!(body["error"]["message"], "ref not found");
}

/// A malformed HEAD is deliberately different from an unborn one. Merely
/// changing the old `Ok([])` into typed NotFound would still lie here — as a
/// 404 instead of a 200 — so this test pins the storage-failure side too.
#[tokio::test]
async fn an_unreadable_head_on_the_log_endpoint_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "loghead-owner", "loghead@example.com").await;
    create_repo(&base, &token, "loghead-repo").await;
    let bare = repo_root.join("loghead-owner/loghead-repo.git");
    std::fs::write(bare.join("HEAD"), "not a ref at all\n").expect("corrupt HEAD fixture");

    // Prove this reaches the helper's HEAD-read arm rather than failing the
    // repository-open guard added by card_b013d630a280.
    let repo = gix::open(&bare).expect("a malformed HEAD must still open the repository");
    assert!(
        repo.head().is_err(),
        "fixture must make HEAD unreadable without making the repository unopenable"
    );

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/loghead-owner/loghead-repo/log"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "an unreadable HEAD is a storage failure, not empty history or a missing ref: \
         {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// A ref can still resolve after its target commit disappears from the object
/// store. `rev_parse_single` alone cannot tell that from a healthy ref, and
/// the old helper therefore reached its best-effort walker and replied `200`
/// with an empty history. The target object must be read before that boundary.
#[tokio::test]
async fn a_log_ref_with_a_missing_target_commit_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "logobject-owner", "logobject@example.com").await;
    create_repo(&base, &token, "logobject-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "logobject-owner", "logobject-repo").await;

    let bare = repo_root.join("logobject-owner/logobject-repo.git");
    let repo = gix::open(&bare).expect("fixture repository must open");
    let head = repo.head().expect("HEAD must be readable");
    let target_ref = head
        .referent_name()
        .expect("fixture HEAD must point to a branch")
        .as_bstr()
        .to_string();
    let target_id = head
        .try_into_peeled_id()
        .expect("HEAD must resolve")
        .expect("fixture repository must have a commit")
        .to_string();
    remove_loose_object(&bare, &target_id);

    let repo = gix::open(&bare).expect("repository must still open after object removal");
    let reference = repo
        .try_find_reference(target_ref.as_str())
        .expect("the ref store must stay readable after target removal")
        .expect("the branch ref must still exist after target removal");
    assert_eq!(
        reference
            .try_id()
            .expect("fixture branch must carry a direct object id")
            .to_string(),
        target_id
    );
    assert!(
        repo.find_object(
            gix::ObjectId::from_hex(target_id.as_bytes())
                .expect("fixture commit must have a full object id"),
        )
        .is_err(),
        "fixture must fail only when gix reads the resolved commit object"
    );

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/logobject-owner/logobject-repo/log"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a missing target object is a server failure, not missing ref or empty history: \
         {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The target commit can be intact while a reachable parent disappears. A
/// best-effort rev-walk used to return the target alone as a normal successful
/// list, so clients had no way to tell a damaged history from a one-commit repo.
#[tokio::test]
async fn a_log_with_an_unreadable_reachable_parent_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "logparent-owner", "logparent@example.com").await;
    create_repo(&base, &token, "logparent-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "logparent-owner", "logparent-repo").await;
    commit_file(
        &client,
        &base,
        &token,
        "logparent-owner",
        "logparent-repo",
        CommitFile {
            path: "SECOND.md",
            content: "second\n",
            message: "add SECOND.md",
        },
    )
    .await;

    let url = format!("{base}/api/v1/repos/logparent-owner/logparent-repo/log");
    let healthy = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("healthy request");
    assert_eq!(
        healthy.status(),
        200,
        "the fixture must expose both commits before corruption"
    );
    let healthy: serde_json::Value = healthy.json().await.expect("healthy JSON");
    let commits = healthy["commits"]
        .as_array()
        .expect("healthy log response must contain commits");
    assert_eq!(
        commits.len(),
        2,
        "fixture must create a reachable parent: {healthy}"
    );
    let parent_id = commits[1]["sha"]
        .as_str()
        .expect("second listed commit must have a SHA")
        .to_string();

    let bare = repo_root.join("logparent-owner/logparent-repo.git");
    remove_loose_object(&bare, &parent_id);
    let repo = gix::open(&bare).expect("repository must remain open after parent removal");
    assert!(
        repo.find_object(
            gix::ObjectId::from_hex(parent_id.as_bytes())
                .expect("fixture parent must have a full object id"),
        )
        .is_err(),
        "fixture must fail only when gix reads the reachable parent"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("error JSON");
    assert!(
        status.is_server_error(),
        "an unreadable reachable parent must not produce a partial 200: {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// card_7cb31c61cee2: the same ref that still resolves after its target commit
/// left the object store. `list_tree_entries` mapped *every* `rev_parse_single`
/// failure onto `NotFound("ref")`, so a repository losing objects answered the
/// tree endpoint with `404 ref not found` — indistinguishable from a branch the
/// client mistyped, and invisible in the alerts.
#[tokio::test]
async fn a_tree_ref_with_a_missing_target_commit_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "treeobject-owner", "treeobject@example.com").await;
    create_repo(&base, &token, "treeobject-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(
        &client,
        &base,
        &token,
        "treeobject-owner",
        "treeobject-repo",
    )
    .await;

    let url = format!("{base}/api/v1/repos/treeobject-owner/treeobject-repo/tree");

    // Non-vacuous baseline: this exact repository lists before the break.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the fixture must list before the break");

    let bare = repo_root.join("treeobject-owner/treeobject-repo.git");
    let repo = gix::open(&bare).expect("fixture repository must open");
    let target_id = repo
        .head()
        .expect("HEAD must be readable")
        .try_into_peeled_id()
        .expect("HEAD must resolve")
        .expect("fixture repository must have a commit")
        .to_string();
    remove_loose_object(&bare, &target_id);

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a ref whose commit left the object store is a storage failure, not a \
         missing ref and not an empty repository: {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The blob half of the same collapse: `get_blob_content` resolved the whole
/// `ref:path` pair with one `rev_parse_single`, so a file whose blob object had
/// disappeared was reported as `404 file not found` — "someone deleted it" —
/// while the ref and the tree entry naming it were both still there.
#[tokio::test]
async fn a_blob_whose_object_is_missing_is_a_server_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "blobobject-owner", "blobobject@example.com").await;
    create_repo(&base, &token, "blobobject-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(
        &client,
        &base,
        &token,
        "blobobject-owner",
        "blobobject-repo",
    )
    .await;

    let url = format!("{base}/api/v1/repos/blobobject-owner/blobobject-repo/blob/README.md");

    // Non-vacuous baseline: the file reads before the break, and a file that
    // really is absent is still a 404 after it (asserted at the end).
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the fixture must read before the break");

    let bare = repo_root.join("blobobject-owner/blobobject-repo.git");
    let repo = gix::open(&bare).expect("fixture repository must open");
    let entry = repo
        .head()
        .expect("HEAD must be readable")
        .try_into_peeled_id()
        .expect("HEAD must resolve")
        .expect("fixture repository must have a commit")
        .object()
        .expect("the commit object must be readable")
        .peel_to_tree()
        .expect("the commit must have a tree")
        .lookup_entry_by_path("README.md")
        .expect("the tree must be readable")
        .expect("the fixture file must be in the tree");
    let blob_id = entry.object_id().to_string();
    remove_loose_object(&bare, &blob_id);

    // The tree entry naming the file survives — only its object is gone, which
    // is what separates this from a deleted file.
    let repo = gix::open(&bare).expect("repository must still open after object removal");
    assert!(
        repo.head()
            .expect("HEAD must still be readable")
            .try_into_peeled_id()
            .expect("HEAD must still resolve")
            .expect("the commit must still be there")
            .object()
            .expect("the commit object must still be readable")
            .peel_to_tree()
            .expect("the tree must still be readable")
            .lookup_entry_by_path("README.md")
            .expect("the tree must still be readable")
            .is_some(),
        "fixture must leave the tree entry in place so only the blob read fails"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a blob that left the object store is a storage failure, not a deleted \
         file: {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);

    // The 404 contract survives the fix: a path this commit never had is still
    // a client miss on the very same broken repository.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/blobobject-owner/blobobject-repo/blob/NOT-THERE.md"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 404,
        "an absent path is still the client's miss (body: {body})"
    );
    assert_eq!(body["error"]["message"], "file not found");
}

/// Branches and tags are snapshots clients use to choose a ref. If gix cannot
/// enumerate even one ref, a shortened 200 list falsely tells the client that
/// the omitted branch or tag does not exist.
#[tokio::test]
async fn unreadable_branch_or_tag_ref_is_a_server_error_not_a_partial_list() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "reffail-owner", "reffail@example.com").await;
    create_repo(&base, &token, "reffail-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "reffail-owner", "reffail-repo").await;

    let bare = repo_root.join("reffail-owner/reffail-repo.git");
    let repo = gix::open(&bare).expect("fixture repository must open");
    let head = repo
        .head()
        .expect("HEAD must be readable")
        .try_into_peeled_id()
        .expect("HEAD must resolve")
        .expect("fixture repository must have a commit")
        .to_string();
    drop(repo);
    std::fs::create_dir_all(bare.join("refs/tags")).expect("tag directory");
    std::fs::write(bare.join("refs/tags/release"), format!("{head}\n")).expect("healthy tag ref");

    for endpoint in ["branches", "tags"] {
        let resp = client
            .get(format!(
                "{base}/api/v1/repos/reffail-owner/reffail-repo/{endpoint}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert!(
            body.as_array().is_some_and(|names| !names.is_empty()),
            "the healthy {endpoint} fixture must yield a non-empty list, got: {body}"
        );
    }

    std::fs::write(bare.join("refs/heads/broken"), "not an object id\n")
        .expect("broken branch ref");
    std::fs::write(bare.join("refs/tags/broken"), "not an object id\n").expect("broken tag ref");

    for endpoint in ["branches", "tags"] {
        let resp = client
            .get(format!(
                "{base}/api/v1/repos/reffail-owner/reffail-repo/{endpoint}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert!(
            status.is_server_error(),
            "an unreadable {endpoint} ref must not look like a complete 200 list: {status} (body: {body})"
        );
        assert_no_internal_detail(&body, &repo_root);
    }
}

/// H-05: whatever the status, the body must not carry the storage path or the
/// git library's own wording. This is the half of the bug a status-only fix
/// would have left in place.
fn assert_no_internal_detail(body: &serde_json::Value, repo_root: &Path) {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let root = repo_root.to_string_lossy();
    assert!(
        !message.contains(root.as_ref()),
        "the response body must not carry the server's storage path, got: {message}"
    );
    for leak in ["failed to open repository", "gix", ".git", "objects"] {
        assert!(
            !message.contains(leak),
            "the response body must not carry internal error detail ({leak}), got: {message}"
        );
    }
}
