//! The Git LFS File Locking API (card_e8afcaf3edf6), at the protocol level.
//!
//! `git lfs lock` / `unlock` / `locks` and the `locks/verify` call a stock
//! client makes before every push. Before this, `locks/verify` answered `404`,
//! which the client reads as "this server has no locking", so a team working
//! on files nobody can merge had nothing to stop two people editing one.
//!
//! These tests drive the routes the way the client does — through the endpoint
//! it derives from the clone URL, with a PAT over Basic auth and the protocol's
//! media type — and pin who may do what: reading lists, writing locks and
//! verifies, and only an administrator may take someone else's lock away.
//! The stock client itself is driven by `lfs_locks_stock_client_tests`.

use crate::common::{register_full, spawn_test_app};

const OWNER: &str = "lock_admin";
const WRITER: &str = "lock_writer";
const OTHER_WRITER: &str = "lock_writer_two";
const READER: &str = "lock_reader";
const REPO: &str = "assets";
const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

struct Caller {
    name: &'static str,
    pat: String,
}

async fn pat_for(base: &str, session: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(session)
        .json(&serde_json::json!({ "name": "git-lfs", "scopes": "repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn add_collaborator(base: &str, token: &str, username: &str, permission: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": permission}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "adding {username} failed");
}

fn endpoint(base: &str, owner: &str) -> String {
    format!("{base}/git/{owner}/{REPO}.git/info/lfs/locks")
}

async fn call(
    method: reqwest::Method,
    url: &str,
    caller: Option<&Caller>,
    body: Option<serde_json::Value>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .header("Accept", LFS_MEDIA_TYPE);
    if let Some(caller) = caller {
        request = request.basic_auth(caller.name, Some(&caller.pat));
    }
    if let Some(body) = body {
        request = request
            .header("Content-Type", LFS_MEDIA_TYPE)
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn lock(base: &str, caller: &Caller, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    call(
        reqwest::Method::POST,
        &endpoint(base, OWNER),
        Some(caller),
        Some(serde_json::json!({"path": path, "ref": {"name": "refs/heads/main"}})),
    )
    .await
}

async fn unlock(
    base: &str,
    caller: &Caller,
    id: &str,
    force: bool,
) -> (reqwest::StatusCode, serde_json::Value) {
    call(
        reqwest::Method::POST,
        &format!("{}/{id}/unlock", endpoint(base, OWNER)),
        Some(caller),
        Some(serde_json::json!({"force": force, "ref": {"name": "refs/heads/main"}})),
    )
    .await
}

async fn verify(base: &str, caller: &Caller) -> (reqwest::StatusCode, serde_json::Value) {
    call(
        reqwest::Method::POST,
        &format!("{}/verify", endpoint(base, OWNER)),
        Some(caller),
        Some(serde_json::json!({"ref": {"name": "refs/heads/main"}})),
    )
    .await
}

fn paths(locks: &serde_json::Value) -> Vec<String> {
    locks
        .as_array()
        .expect("a list of locks")
        .iter()
        .map(|lock| lock["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn locks_are_taken_listed_verified_and_released_by_the_right_people() {
    let base = spawn_test_app().await;
    let mut callers = Vec::new();
    let mut sessions = Vec::new();
    for name in [OWNER, WRITER, OTHER_WRITER, READER] {
        let (session, _) = register_full(&base, name, &format!("{name}@example.com")).await;
        callers.push(Caller {
            name,
            pat: pat_for(&base, &session).await,
        });
        sessions.push(session);
    }
    let [admin, writer, other_writer, reader] = <[Caller; 4]>::try_from(callers).ok().unwrap();
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&sessions[0])
        .json(&serde_json::json!({ "name": REPO, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    add_collaborator(&base, &sessions[0], WRITER, "write").await;
    add_collaborator(&base, &sessions[0], OTHER_WRITER, "write").await;
    add_collaborator(&base, &sessions[0], READER, "read").await;

    // A writer locks a file; the lock names them.
    let (status, body) = lock(&base, &writer, "art/hero.psd").await;
    assert_eq!(status, 201, "{body}");
    let lock_id = body["lock"]["id"].as_str().expect("lock id").to_string();
    assert_eq!(body["lock"]["path"], "art/hero.psd");
    assert_eq!(body["lock"]["owner"]["name"], WRITER);
    assert!(body["lock"]["locked_at"].is_string());

    // The same path again — by anyone — is a 409 carrying the lock that holds it.
    let (status, body) = lock(&base, &admin, "art/hero.psd").await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["lock"]["id"], lock_id.as_str());
    assert_eq!(body["lock"]["owner"]["name"], WRITER);
    assert!(body["message"].as_str().unwrap().contains(WRITER));

    // Reading lists; it does not lock. Strangers are told nothing.
    let (status, body) = call(
        reqwest::Method::GET,
        &endpoint(&base, OWNER),
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(paths(&body["locks"]), vec!["art/hero.psd"]);
    assert_eq!(body["next_cursor"], "");
    let (status, _) = lock(&base, &reader, "art/other.psd").await;
    assert_eq!(status, 403, "a reader took a lock");
    let (status, _) = call(reqwest::Method::GET, &endpoint(&base, OWNER), None, None).await;
    assert_eq!(
        status, 401,
        "an anonymous caller listed a private repository's locks"
    );

    // Filters and pages, the way `git lfs locks --path` / `--id` and the
    // client's pagination ask.
    for path in ["art/b.psd", "art/c.psd"] {
        assert_eq!(lock(&base, &writer, path).await.0, 201);
    }
    let (_, body) = call(
        reqwest::Method::GET,
        &format!("{}?path=art/b.psd", endpoint(&base, OWNER)),
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(paths(&body["locks"]), vec!["art/b.psd"]);
    let (_, body) = call(
        reqwest::Method::GET,
        &format!("{}?id={lock_id}", endpoint(&base, OWNER)),
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(paths(&body["locks"]), vec!["art/hero.psd"]);
    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let (status, body) = call(
            reqwest::Method::GET,
            &format!("{}?limit=2&cursor={cursor}", endpoint(&base, OWNER)),
            Some(&reader),
            None,
        )
        .await;
        assert_eq!(status, 200, "{body}");
        seen.extend(paths(&body["locks"]));
        cursor = body["next_cursor"].as_str().unwrap().to_string();
        if cursor.is_empty() {
            break;
        }
    }
    assert_eq!(seen, vec!["art/hero.psd", "art/b.psd", "art/c.psd"]);

    // `locks/verify` splits the locks by who holds them, for the client to
    // refuse a push touching someone else's.
    let (status, body) = verify(&base, &writer).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(paths(&body["ours"]).len(), 3);
    assert_eq!(body["theirs"], serde_json::json!([]));
    let (_, body) = verify(&base, &admin).await;
    assert_eq!(body["ours"], serde_json::json!([]));
    assert_eq!(paths(&body["theirs"]).len(), 3);
    let (status, _) = verify(&base, &reader).await;
    assert_eq!(status, 403, "verify is asked by a client about to push");

    // Someone else's lock: not without force, and force is an admin's word.
    let (status, body) = unlock(&base, &other_writer, &lock_id, false).await;
    assert_eq!(status, 403, "{body}");
    assert!(body["message"].as_str().unwrap().contains("--force"));
    let (status, _) = unlock(&base, &other_writer, &lock_id, true).await;
    assert_eq!(status, 403, "a writer forced someone else's lock off");
    let (status, _) = unlock(&base, &writer, &lock_id, true).await;
    assert_eq!(
        status, 403,
        "force from a non-admin must be refused even on their own lock"
    );

    // The holder unlocks their own; an admin forces another off.
    let (status, body) = unlock(&base, &writer, &lock_id, false).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["lock"]["path"], "art/hero.psd");
    let (status, _) = unlock(&base, &writer, &lock_id, false).await;
    assert_eq!(status, 404, "a lock that is gone is not found");
    let (_, body) = call(
        reqwest::Method::GET,
        &format!("{}?path=art/b.psd", endpoint(&base, OWNER)),
        Some(&reader),
        None,
    )
    .await;
    let b_id = body["locks"][0]["id"].as_str().unwrap().to_string();
    let (status, body) = unlock(&base, &admin, &b_id, true).await;
    assert_eq!(status, 200, "{body}");

    // A path that cannot name a file in the repository is refused, not stored.
    for refused in ["/etc/passwd", "art/../x", ""] {
        let (status, body) = lock(&base, &writer, refused).await;
        assert_eq!(status, 400, "{refused:?}: {body}");
        assert!(body["message"].is_string(), "{body}");
    }

    // A transfer moves the repository, and its locks with it.
    let org = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&sessions[0])
        .json(&serde_json::json!({"name": "lock-org"}))
        .send()
        .await
        .unwrap();
    assert_eq!(org.status(), 201);
    let transferred = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/transfer"))
        .bearer_auth(&sessions[0])
        .json(&serde_json::json!({"new_owner": "lock-org"}))
        .send()
        .await
        .unwrap();
    assert_eq!(transferred.status(), 200);
    let (status, body) = call(
        reqwest::Method::GET,
        &endpoint(&base, "lock-org"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(paths(&body["locks"]), vec!["art/c.psd"]);

    // Deleting the repository leaves no lock behind for a namesake to inherit.
    let deleted = reqwest::Client::new()
        .delete(format!("{base}/api/v1/repos/lock-org/{REPO}"))
        .bearer_auth(&sessions[0])
        .send()
        .await
        .unwrap();
    assert!(deleted.status().is_success(), "{}", deleted.status());
    let recreated = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&sessions[0])
        .json(&serde_json::json!({ "name": REPO, "org": "lock-org" }))
        .send()
        .await
        .unwrap();
    assert_eq!(recreated.status(), 201, "recreating the namesake failed");
    let (status, body) = call(
        reqwest::Method::GET,
        &endpoint(&base, "lock-org"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["locks"],
        serde_json::json!([]),
        "the namesake inherited a lock"
    );
}

/// A lock id is instance-wide, and the unlock route takes it from the path. A
/// writer of one repository naming a lock in another — a private one they
/// cannot see — has to get exactly the answer an id nobody ever held gets, or
/// walking the ids enumerates every lock on the instance.
#[tokio::test]
async fn a_lock_id_from_another_repository_is_answered_like_one_that_never_existed() {
    let base = spawn_test_app().await;
    let (owner_session, _) = register_full(&base, OWNER, "lock_admin@example.com").await;
    let (outsider_session, _) =
        register_full(&base, "lock_outsider", "lock_outsider@example.com").await;
    let owner = Caller {
        name: OWNER,
        pat: pat_for(&base, &owner_session).await,
    };
    let outsider = Caller {
        name: "lock_outsider",
        pat: pat_for(&base, &outsider_session).await,
    };
    for (session, name) in [(&owner_session, REPO), (&outsider_session, "own")] {
        let created = reqwest::Client::new()
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(session)
            .json(&serde_json::json!({ "name": name, "is_private": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201);
    }
    let (status, body) = lock(&base, &owner, "secret/plan.psd").await;
    assert_eq!(status, 201, "{body}");
    let lock_id = body["lock"]["id"].as_str().unwrap().to_string();

    let unlock_in_own = |id: String| {
        let outsider = &outsider;
        let base = &base;
        async move {
            call(
                reqwest::Method::POST,
                &format!("{base}/git/lock_outsider/own.git/info/lfs/locks/{id}/unlock"),
                Some(outsider),
                Some(serde_json::json!({"force": false})),
            )
            .await
        }
    };
    // Each answer carries its own request id; everything else has to match.
    let without_request_id = |(status, mut body): (reqwest::StatusCode, serde_json::Value)| {
        if let Some(error) = body.get_mut("error").and_then(|e| e.as_object_mut()) {
            error.remove("request_id");
        }
        (status, body)
    };
    let foreign = without_request_id(unlock_in_own(lock_id.clone()).await);
    let absent = without_request_id(unlock_in_own("999999".to_string()).await);
    assert_eq!(foreign.0, 404, "{}", foreign.1);
    assert_eq!(
        foreign, absent,
        "a foreign lock id was answered differently from an absent one"
    );

    // And the lock is still there for its holder.
    let (status, _) = unlock(&base, &owner, &lock_id, false).await;
    assert_eq!(status, 200);
}
