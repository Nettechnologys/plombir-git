use crate::common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn runner_register_requires_admin() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let unauth_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .json(&serde_json::json!({"name": "unauth-runner"}))
        .send()
        .await
        .unwrap();
    assert!(unauth_resp.status() == 401 || unauth_resp.status() == 403);

    let (admin_token, _admin_id) =
        register_full(&base, "runner_admin", "runner_admin@example.com").await;
    let (user_token, _user_id) =
        register_full(&base, "runner_user", "runner_user@example.com").await;
    let user_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({"name": "user-runner"}))
        .send()
        .await
        .unwrap();
    assert!(user_resp.status() == 401 || user_resp.status() == 403);

    let admin_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"name": "admin-runner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 201);

    let body: serde_json::Value = admin_resp.json().await.unwrap();
    assert!(body["id"].as_i64().is_some());
    assert!(body["token"].as_str().is_some());
}

#[tokio::test]
async fn runner_register_accepts_admin_httponly_cookie() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "runner_cookie", "runner_cookie@example.com").await;

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");

    let resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .header(
            reqwest::header::COOKIE,
            format!("forgekeep_token={}", admin_token),
        )
        .json(&serde_json::json!({"name": "cookie-runner"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
}

/// The runner token is a bearer credential for the whole CI API — jobs,
/// artifacts, the build cache — so a database dump must not contain a working
/// one. It is issued once, in this response, and stored only as its SHA-256.
///
/// The assertion is against the *whole* row, not against the column we happen
/// to have renamed: a future column that quietly kept a copy would fail this.
#[tokio::test]
async fn a_registered_runners_token_is_not_recoverable_from_the_database() {
    use sea_orm::EntityTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "runner_at_rest", "runner_at_rest@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");

    let resp = client
        .post(format!("{base}/api/v1/runners/register"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"name": "at-rest-runner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let issued = body["token"]
        .as_str()
        .expect("a token was issued")
        .to_owned();
    let runner_id = body["id"].as_i64().unwrap();

    let row = rg_db::entities::runner::Entity::find_by_id(runner_id)
        .one(&db)
        .await
        .unwrap()
        .expect("the registered runner is in the database");
    let dump = serde_json::to_string(&row).unwrap();
    assert!(
        !dump.contains(&issued),
        "the issued runner token is readable from the row: {dump}"
    );
    assert_eq!(row.token_hash, rg_db::ops::runner_ops::hash_token(&issued));

    // ...and the credential that is no longer stored still works, which is what
    // makes the absence above a fix rather than a broken registration.
    let heartbeat = client
        .post(format!("{base}/api/v1/runners/{runner_id}/heartbeat"))
        .bearer_auth(&issued)
        .send()
        .await
        .unwrap();
    assert_eq!(heartbeat.status(), 200);
}

/// A token issued *before* the tokens were hashed keeps working: the migration
/// hashes the stored values in place rather than reissuing them, so no operator
/// has to re-register a fleet of runners to install this fix.
///
/// The stored value here is a literal SHA-256 hex digest, computed outside the
/// code under test. That pins the two halves to the same algorithm: if either
/// the migration's digest or `find_by_token`'s ever changed, this row would
/// stop opening.
#[tokio::test]
async fn a_token_issued_before_the_hashing_still_authenticates() {
    use sea_orm::{ActiveModelTrait, Set};

    const LEGACY_TOKEN: &str = "legacy-runner-token";
    const LEGACY_TOKEN_SHA256: &str =
        "41470fa2ea0568b3bc5b14c8c29e0450bd39dad7b0cd457f8ce08343867ee34e";

    let (base, db) = spawn_test_app_with_db().await;
    let now = chrono::Utc::now();
    let runner = rg_db::entities::runner::ActiveModel {
        id: sea_orm::NotSet,
        name: Set("pre-migration-runner".to_string()),
        token_hash: Set(LEGACY_TOKEN_SHA256.to_string()),
        status: Set("offline".to_string()),
        labels: Set("[]".to_string()),
        last_seen_at: Set(now),
        version: Set(None),
        os: Set(None),
        arch: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db)
    .await
    .expect("seed a runner as the migration leaves it");

    let heartbeat = reqwest::Client::new()
        .post(format!("{base}/api/v1/runners/{}/heartbeat", runner.id))
        .bearer_auth(LEGACY_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(
        heartbeat.status(),
        200,
        "a runner token issued before the migration was locked out"
    );
}

/// `{job_id}` is an instance-wide primary key, so another runner's job must be
/// indistinguishable from a job that does not exist.
///
/// The pair `403`/`404` is an existence oracle: a runner that answers `403` for
/// "assigned to somebody else" and `404` for "no such id" hands out, to anyone
/// holding a runner token and a `for` loop, the exact set of live jobs on the
/// instance — including those of private repositories whose names the runner
/// never learns. The project's rule is written down in `api::boards`: "a
/// mismatch answers 404, not 403: a 403 would confirm the id exists". This is
/// the runner axis of it, across all six routes that take a `{job_id}`.
///
/// Three things make the assertion mean something:
///
/// - the comparison is against a **fresh unused id**, not against a literal
///   `404` — the two replies have to be the same reply, not merely the same
///   status, so a message reading "not yours" would still fail;
/// - bodies are compared by `(code, message)` rather than whole: `AppError`
///   stamps a new `request_id` into every response, so `assert_eq!` on the
///   whole body fails always and for the wrong reason;
/// - the owning runner drives its own job in the same run. Without that, two
///   matching `404`s would attest to a correct scope and to a broken route
///   equally well.
#[tokio::test]
async fn another_runners_job_id_is_indistinguishable_from_an_unused_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "jobscope", "jobscope@example.com").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"jobscope","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let log = client
        .get(format!("{base}/api/v1/repos/jobscope/jobscope/log"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(log.status(), 200);
    let sha = log.json::<serde_json::Value>().await.unwrap()["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_owned();

    let (mine, mine_token) =
        rg_db::ops::runner_ops::register_runner(&db, "mine", "[]", None, None, None)
            .await
            .unwrap();
    let (stranger, stranger_token) =
        rg_db::ops::runner_ops::register_runner(&db, "stranger", "[]", None, None, None)
            .await
            .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &sha,
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "scoped",
        "true",
        None,
        None,
        None,
        Some("scope-key"),
        Some(r#"["target"]"#),
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(&db, job.id, mine.id)
        .await
        .unwrap();
    // An id no job was ever handed out under, so "somebody else's" has something
    // to be indistinguishable *from*.
    let unused = job.id + 10_000;

    // (method, path suffix, extra headers) for every route that takes a job id.
    let probes: &[(&str, &str)] = &[
        ("POST", "start"),
        ("POST", "log"),
        ("GET", "workspace"),
        ("GET", "cache"),
        ("PUT", "cache"),
        ("POST", "finish"),
        ("POST", "artifacts"),
    ];

    for (method, suffix) in probes {
        let mut shapes = Vec::new();
        for id in [job.id, unused] {
            let url = format!("{base}/api/v1/runners/{}/jobs/{id}/{suffix}", stranger.id);
            let mut req = match *method {
                "GET" => client.get(url),
                "PUT" => client.put(url),
                _ => client.post(url),
            };
            req = req.bearer_auth(&stranger_token);
            if *suffix == "cache" {
                req = req.header("x-cache-key", "scope-key");
            }
            if *suffix == "finish" {
                req = req.json(&serde_json::json!({"status": "success", "exit_code": 0}));
            }
            if *suffix == "artifacts" {
                // The artifact endpoint accepts metadata only. The path is
                // deliberately unusable: job ownership must reject this
                // runner before artifact metadata is interpreted.
                req = req.json(&serde_json::json!({
                    "name": "a.txt",
                    "file_path": "/definitely-not-this-job/a.txt",
                }));
            }
            let resp = req.send().await.unwrap();
            let status = resp.status();
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
            assert_eq!(
                status, 404,
                "{method} .../jobs/{{id}}/{suffix} told a stranger which id was real ({status})"
            );
            shapes.push((
                body["error"]["code"].as_str().map(str::to_owned),
                body["error"]["message"].as_str().map(str::to_owned),
            ));
        }
        assert_eq!(
            shapes[0], shapes[1],
            "{method} .../jobs/{{id}}/{suffix}: another runner's job and an unused id \
             answer with different bodies, so the pair is still an oracle"
        );
    }

    // Baseline, in this same run: the runner the job *is* assigned to drives it.
    // Without this, every 404 above is equally good evidence of a dead route.
    let started = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/start",
            mine.id, job.id
        ))
        .bearer_auth(&mine_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        started.status(),
        200,
        "the assigned runner cannot start its own job — the denials above prove nothing"
    );
}
