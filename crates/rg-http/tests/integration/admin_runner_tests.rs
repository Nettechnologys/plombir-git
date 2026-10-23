use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

/// `GET /admin/runners/{id}` was implemented, documented in the published spec,
/// and mounted by nothing — a generated client would call it and get a 404
/// (card_a76ad95240d5). The route table is now the only place that can answer
/// "does this door exist", so the test asks the running server.
///
/// The 404 for an id nothing created is the other half of the argument: without
/// it, a route that answered `200 {}` to everything would pass the first half.
#[tokio::test]
async fn the_documented_single_runner_endpoint_answers_the_admin_who_asks() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "runneradmin", "runneradmin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let repo_id = create_repo(&base, &admin_token, "runner-scope").await;

    let registered = client
        .post(format!("{base}/api/v1/runners/register"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "repository": "runneradmin/runner-scope",
            "name": "linux-runner-01",
            "labels": ["linux", "x86_64"],
        }))
        .send()
        .await
        .expect("register a runner");
    assert_eq!(registered.status(), 201);
    let runner_id = registered.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("registration must return the new runner's id");

    let found = client
        .get(format!("{base}/api/v1/admin/runners/{runner_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read the runner back");
    assert_eq!(
        found.status(),
        200,
        "the spec advertises this endpoint — it must be mounted"
    );
    let body: serde_json::Value = found.json().await.unwrap();
    assert_eq!(body["id"], runner_id);
    assert_eq!(body["repo_id"], repo_id);
    assert_eq!(body["repository"], "runneradmin/runner-scope");
    assert_eq!(body["name"], "linux-runner-01");

    let missing = client
        .get(format!("{base}/api/v1/admin/runners/{}", runner_id + 9_999))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read a runner that does not exist");
    assert_eq!(
        missing.status(),
        404,
        "an id nothing created must not be answered as if it existed"
    );
}

/// card_28cc6b110902: a repository's administrator reads its runners without
/// the instance administrator's keys — what a monitor needs to tell "the
/// runner's process is up" from "the runner is working". The status follows
/// the runner: `offline` until it polls, `online` while it does, `offline`
/// again once it stops, `online` when it is back. Somebody who may not
/// administer the repository is refused, a token confined to another
/// repository is refused, and the token hash never leaves the row.
#[tokio::test]
async fn a_repository_admin_reads_its_runners_and_their_liveness() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (owner_token, _) = register_full(&base, "runner-owner", "runner-owner@example.com").await;
    let (stranger_token, _) =
        register_full(&base, "runner-stranger", "runner-stranger@example.com").await;
    let mut repo_ids = Vec::new();
    for name in ["watched", "elsewhere"] {
        let created = client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({ "name": name, "is_private": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201);
        repo_ids.push(
            created.json::<serde_json::Value>().await.unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_ids[0],
        "ci-box",
        r#"["linux","x64"]"#,
        Some("0.2.0"),
        Some("linux"),
        Some("x86_64"),
    )
    .await
    .unwrap();
    // A row whose labels column cannot be read is still listed, so one bad
    // row cannot hide every other runner's liveness from a monitor.
    let (unlabelled, _) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_ids[0],
        "odd-box",
        "linux",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    // A runner of the other repository is not this repository's.
    rg_db::ops::runner_ops::register_runner(&db, repo_ids[1], "other-box", "[]", None, None, None)
        .await
        .unwrap();

    let url = format!("{base}/api/v1/repos/runner-owner/watched/actions/runners");
    let read = |token: String| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let response = client.get(&url).bearer_auth(&token).send().await.unwrap();
            let status = response.status().as_u16();
            let body: serde_json::Value = response.json().await.unwrap();
            (status, body)
        }
    };
    let runner_status = |body: &serde_json::Value| {
        let runners = body.as_array().expect("a list of runners");
        assert_eq!(runners.len(), 2, "only this repository's runners: {body}");
        let listed = runners
            .iter()
            .find(|listed| listed["id"] == runner.id)
            .unwrap_or_else(|| panic!("ci-box is listed: {body}"));
        listed["status"].as_str().unwrap().to_owned()
    };
    let poll = || {
        let client = client.clone();
        let base = base.clone();
        let runner_token = runner_token.clone();
        async move {
            let polled = client
                .get(format!(
                    "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
                    runner.id
                ))
                .bearer_auth(&runner_token)
                .send()
                .await
                .unwrap();
            assert_eq!(polled.status(), 204, "nothing queued, the poll times out");
        }
    };

    let (status, body) = read(owner_token.clone()).await;
    assert_eq!(status, 200, "{body}");
    let by_id = |id: i64| {
        body.as_array()
            .unwrap()
            .iter()
            .find(|listed| listed["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("runner {id} is listed: {body}"))
    };
    assert_eq!(by_id(unlabelled.id)["labels"], serde_json::json!([]));
    let listed = &by_id(runner.id);
    assert_eq!(listed["id"], runner.id);
    assert_eq!(listed["name"], "ci-box");
    assert_eq!(listed["labels"], serde_json::json!(["linux", "x64"]));
    assert_eq!(listed["busy"], false);
    assert_eq!(listed["os"], "linux");
    assert!(listed["last_seen_at"].is_string(), "{listed}");
    assert!(
        listed.get("token_hash").is_none() && !body.to_string().contains(&runner.token_hash),
        "the token hash must not leave the row: {body}"
    );
    assert_eq!(runner_status(&body), "offline", "registered, never polled");

    poll().await;
    assert_eq!(runner_status(&read(owner_token.clone()).await.1), "online");

    // A runner holding a job is still online, and busy.
    rg_db::ops::runner_ops::update_status(&db, runner.id, "busy")
        .await
        .unwrap();
    let busy = read(owner_token.clone()).await.1;
    assert_eq!(runner_status(&busy), "online");
    assert!(
        busy.as_array()
            .unwrap()
            .iter()
            .any(|listed| listed["id"] == runner.id && listed["busy"] == true),
        "{busy}"
    );
    rg_db::ops::runner_ops::update_status(&db, runner.id, "online")
        .await
        .unwrap();

    let stopped = client
        .post(format!("{base}/api/v1/runners/{}/deregister", runner.id))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert!(stopped.status().is_success(), "{}", stopped.status());
    assert_eq!(runner_status(&read(owner_token.clone()).await.1), "offline");

    poll().await;
    assert_eq!(
        runner_status(&read(owner_token.clone()).await.1),
        "online",
        "a stopped runner that comes back is online again"
    );

    // Somebody who may not administer the private repository is refused, and
    // the refusal carries no runner. 403 rather than 404 is the repository
    // gate's rule for an authenticated outsider (`check_read_for`), the same
    // on every route behind it.
    let (status, body) = read(stranger_token.clone()).await;
    assert_eq!(status, 403, "{body}");
    assert!(!body.to_string().contains("ci-box"), "{body}");

    // The owner's own token, confined to the other repository, does not reach
    // this one.
    let confined = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "elsewhere-only", "scopes": "repo", "repositories": ["runner-owner/elsewhere"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(confined.status(), 201);
    let confined = confined.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = read(confined).await;
    assert!(matches!(status, 403 | 404), "{status}: {body}");

    // And a token confined to this repository reads it.
    let allowed = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "watched-only", "scopes": "repo", "repositories": ["runner-owner/watched"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 201);
    let allowed = allowed.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = read(allowed).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(runner_status(&body), "online");
}
