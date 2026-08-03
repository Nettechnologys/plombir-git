//! card_8e5d79bdb8bd: a resource in the wrong state is a 409, not a 400.
//!
//! These four CI routes answered `400 Bad Request` when the request was
//! perfect and the *resource* had moved: the job was already released, the
//! pipeline had already finished, the approval had already been given. A client
//! that receives 400 goes and fixes its request, of which there is nothing to
//! fix; a client that receives 409 re-reads the state, which is the only thing
//! that can help it.
//!
//! The most harmful of them was "manual job was already released" — that is
//! literally the double-click and the retry-after-timeout, the case `Conflict`
//! exists for, and 400 turned a harmless repeat into "you sent rubbish".
//!
//! Each conflict is paired with a genuinely malformed request on the same
//! route, so the tests prove the two cases stayed *distinct* rather than that
//! everything moved to 409 together.

use axum::http::StatusCode;
use sea_orm::ConnectionTrait;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

struct Fixture {
    base: String,
    owner: String,
    repo: String,
    token: String,
    pipeline_id: i64,
    job_id: i64,
}

/// One repository, one pipeline, one job. `manual` decides whether the job is
/// the kind `/play` is allowed to release.
async fn seed(suffix: &str, manual: bool) -> (Fixture, rg_db::DatabaseConnection) {
    let (base, db) = spawn_test_app_with_db().await;
    let username = format!("ci-{suffix}");
    let (token, _) = register_full(&base, &username, &format!("{username}@example.test")).await;
    let repo_name = format!("ci-{suffix}");
    let repo_id = create_repo(&base, &token, &repo_name).await;

    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "test",
        "echo ok",
        None,
        Some(r#"["linux"]"#),
        None,
        None,
        None,
        false,
        None,
        if manual { Some("manual") } else { None },
        None,
    )
    .await
    .expect("create job");

    if manual {
        rg_db::ops::pipeline_ops::update_pipeline_status(&db, pipeline.id, "manual", None, None)
            .await
            .expect("park the pipeline on its manual job");
    }

    (
        Fixture {
            base,
            owner: username,
            repo: repo_name,
            token,
            pipeline_id: pipeline.id,
            job_id: job.id,
        },
        db,
    )
}

impl Fixture {
    fn play_url(&self) -> String {
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/{}/jobs/{}/play",
            self.base, self.owner, self.repo, self.pipeline_id, self.job_id
        )
    }

    fn cancel_url(&self) -> String {
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/{}/cancel",
            self.base, self.owner, self.repo, self.pipeline_id
        )
    }

    fn approve_url(&self) -> String {
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/{}/jobs/{}/approve",
            self.base, self.owner, self.repo, self.pipeline_id, self.job_id
        )
    }

    async fn post(&self, url: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("request")
    }
}

/// The double click. Several `/play` requests land on one manual job; exactly
/// one releases it, and every other one is told the state moved — not that its
/// request was malformed.
#[tokio::test]
async fn a_manual_job_released_twice_answers_conflict_not_bad_request() {
    let (fixture, _db) = seed("play-race", true).await;

    const RACERS: usize = 6;
    let mut tasks = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let url = fixture.play_url();
        let token = fixture.token.clone();
        tasks.push(tokio::spawn(async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .send()
                .await
                .expect("play request")
                .status()
                .as_u16()
        }));
    }
    let mut statuses = Vec::with_capacity(RACERS);
    for task in tasks {
        statuses.push(task.await.expect("play task panicked"));
    }

    assert_eq!(
        statuses.iter().filter(|status| **status == 200).count(),
        1,
        "exactly one request may release the job, got {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|status| **status == 409).count(),
        RACERS - 1,
        "every request that found the job already released must answer 409, got {statuses:?}"
    );
    assert!(
        !statuses.contains(&400),
        "a repeat of a well-formed release is not a malformed request, got {statuses:?}"
    );
}

/// The deterministic half of the same rule: a job that never had a manual
/// action to take.
#[tokio::test]
async fn playing_a_job_that_is_not_manual_answers_conflict() {
    let (fixture, _db) = seed("play-plain", false).await;

    let response = fixture.post(&fixture.play_url()).await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a job with no manual action pending is a state conflict"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("job is not awaiting manual action"),
        "the conflict must still say what is wrong, got: {body}"
    );
}

/// The pipeline finished on its own before the cancel landed. The request was
/// fine; the state moved.
#[tokio::test]
async fn canceling_a_finished_pipeline_answers_conflict() {
    let (fixture, db) = seed("cancel-done", false).await;
    rg_db::ops::pipeline_ops::update_pipeline_status(&db, fixture.pipeline_id, "success", None, None)
        .await
        .expect("finish the pipeline");

    let response = fixture.post(&fixture.cancel_url()).await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "cancelling an already-finished pipeline is a state conflict"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("pipeline is not active"),
        "the conflict must still say what is wrong, got: {body}"
    );
}

/// The fourth site, in `ci_environments.rs`: an approval for a job that is not
/// waiting on one.
#[tokio::test]
async fn approving_a_job_that_is_not_waiting_answers_conflict() {
    let (fixture, _db) = seed("approve-plain", false).await;

    let response = fixture.post(&fixture.approve_url()).await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "approving a job that is not awaiting approval is a state conflict"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("job is not awaiting environment approval"),
        "the conflict must still say what is wrong, got: {body}"
    );
}

/// Park the seeded job and its pipeline on an approval, optionally pointing the
/// job at `environment_id`. Both of the branches below are only reachable once
/// the job really is `waiting_approval`, which is what the status check ahead of
/// them enforces.
async fn park_on_approval(
    db: &rg_db::DatabaseConnection,
    fixture: &Fixture,
    environment_id: Option<i64>,
) {
    let environment = match environment_id {
        Some(id) => id.to_string(),
        None => "NULL".to_string(),
    };
    db.execute_unprepared(&format!(
        "UPDATE pipeline_jobs SET status = 'waiting_approval', environment_id = {environment} \
         WHERE id = {}; \
         UPDATE pipelines SET status = 'waiting_approval' WHERE id = {};",
        fixture.job_id, fixture.pipeline_id
    ))
    .await
    .expect("park the job on an approval");
}

/// card_a118728d147c, first of the pair: the job says it is waiting for an
/// approval and names no environment to be approved for. Two of our own rows
/// disagree — there is nothing in the request to fix, and 400 would send the
/// caller looking for it.
#[tokio::test]
async fn approving_a_job_with_no_environment_answers_conflict() {
    let (fixture, db) = seed("approve-no-env", false).await;
    park_on_approval(&db, &fixture, None).await;

    let response = fixture.post(&fixture.approve_url()).await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a job waiting on an approval it has no environment for is a state conflict"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("job has no protected environment"),
        "the conflict must still say what is wrong, got: {body}"
    );
}

/// card_a118728d147c, second of the pair: the environment the job is waiting on
/// was deleted (or un-protected) while it waited. The comment at the call site
/// already reasoned that this is a *stale request* rather than a missing
/// resource — which rules out the helper's 404 and lands exactly on 409, not on
/// the 400 it used to answer.
#[tokio::test]
async fn approving_a_job_whose_environment_vanished_answers_conflict() {
    let (fixture, db) = seed("approve-gone-env", false).await;

    // A real protected environment, attached to the job, then deleted — the
    // sequence an operator produces by removing an environment that still has a
    // job parked on it.
    let environment = rg_db::ops::ci_environment_ops::create(
        &db,
        rg_db::entities::ci_environment::ActiveModel {
            repo_id: sea_orm::ActiveValue::Set(
                rg_db::ops::pipeline_ops::get_pipeline(&db, fixture.pipeline_id)
                    .await
                    .expect("load pipeline")
                    .expect("pipeline exists")
                    .repo_id,
            ),
            name: sea_orm::ActiveValue::Set("production".to_string()),
            protected: sea_orm::ActiveValue::Set(true),
            required_approvals: sea_orm::ActiveValue::Set(1),
            allowed_approver_ids: sea_orm::ActiveValue::Set(None),
            created_at: sea_orm::ActiveValue::Set(chrono::Utc::now()),
            updated_at: sea_orm::ActiveValue::Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("create protected environment");

    park_on_approval(&db, &fixture, Some(environment.id)).await;
    assert!(
        rg_db::ops::ci_environment_ops::delete(&db, environment.id)
            .await
            .expect("delete environment"),
        "the environment must actually be gone for this test to mean anything"
    );

    let response = fixture.post(&fixture.approve_url()).await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a job waiting on an environment that no longer exists is a state conflict"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("protected environment no longer exists"),
        "the conflict must still say what is wrong, got: {body}"
    );
}

/// The other half of the claim: 400 did not simply become 409 everywhere. A
/// request that really is malformed — a pipeline id that is not a number — is
/// still a 400 on the very same routes.
#[tokio::test]
async fn a_genuinely_malformed_request_is_still_a_bad_request() {
    let (fixture, _db) = seed("malformed", false).await;

    for url in [
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/not-a-number/cancel",
            fixture.base, fixture.owner, fixture.repo
        ),
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/not-a-number/jobs/{}/play",
            fixture.base, fixture.owner, fixture.repo, fixture.job_id
        ),
        // The approve route carries two more 409s since card_a118728d147c, so
        // its 400 needs pinning here too.
        format!(
            "{}/api/v1/repos/{}/{}/pipelines/not-a-number/jobs/{}/approve",
            fixture.base, fixture.owner, fixture.repo, fixture.job_id
        ),
    ] {
        let status = fixture.post(&url).await.status();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a path that cannot be parsed is still the client's mistake: {url}"
        );
    }
}
