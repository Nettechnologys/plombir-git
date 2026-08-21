use crate::common::answer::Answer;
use crate::common::ws::handshake_request;
use crate::common::{
    register_full, spawn_test_app_with_db, spawn_test_app_with_overrides, StateOverrides,
};
use tokio_tungstenite::tungstenite::Error;

/// An id no job on the instance has ever carried — the reference a refusal on a
/// real job has to be indistinguishable from.
const ABSENT_JOB_ID: i64 = 999_999;

async fn create_private_job(
    base: &str,
    db: &rg_db::DatabaseConnection,
    token: &str,
    owner_id: i64,
) -> i64 {
    create_job_in_repo(base, db, token, owner_id, "private-ws", true)
        .await
        .1
}

async fn create_job_in_repo(
    base: &str,
    db: &rg_db::DatabaseConnection,
    token: &str,
    owner_id: i64,
    name: &str,
    is_private: bool,
) -> (i64, i64) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": is_private}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let repo_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        Some(owner_id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .unwrap();
    let job_id = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "test", "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap()
    .id;
    (repo_id, job_id)
}

fn websocket_request(
    base: &str,
    job_id: i64,
    token: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    handshake_request(base, &format!("/api/v1/ws/job/{job_id}"), token)
}

/// Everything a caller learns from a handshake this route refused.
///
/// A status is one bit of a reply and masking is a claim about the whole of it,
/// so the comparison goes through [`Answer::shape`] — the one normalizer the
/// id-scope sweeps share — rather than a second local notion of "the same
/// answer". `AppError` stamps a fresh `request_id` into every envelope, which is
/// what makes a raw body comparison permanently red.
async fn refusal(base: &str, job_id: i64, token: Option<&str>) -> Answer {
    crate::common::ws::refusal(base, &format!("/api/v1/ws/job/{job_id}"), token).await
}

/// card_6b2cadf41876 — the WebSocket axis of the instance-wide-id oracle.
///
/// `{job_id}` is a global primary key and this route's path names no repository,
/// so a refusal on somebody else's private job and a refusal on an id nothing
/// ever carried have to be the same reply. They were not: the read gate answered
/// `403 access denied` for a job that exists and the resolver answered
/// `404 job not found` for one that does not, which hands anyone holding an
/// account and a `for` loop the exact set of live CI jobs on the instance —
/// private repositories included, whose names they never learn.
///
/// It is the fourth axis of one defect (`card_24c64a130b82` user,
/// `card_e704fe5ca25f` repo, `card_39b8ccb74dfe` runner) and the first one
/// reached over a transport rather than over REST, which is exactly why the
/// route sweep never judged it: `Access::Foreign` is owed `Expect::Unchecked`,
/// and that exemption is about the *credential* — it was never a licence to skip
/// the one question that needs no foreign credential at all, because both probes
/// are driven by the same person.
///
/// Four things make the assertion mean something:
///
/// - the comparison is against a fresh unused id rather than a literal `404`, so
///   a message reading "not yours" would still fail it;
/// - the two replies are compared whole (status *and* normalized body), because
///   this is where the oracle went the last three times it was pushed out of a
///   status code;
/// - the owner opens a socket on the same job in the same run, so a wall of
///   refusals cannot be a dead fixture reported as a passing security test;
/// - the anonymous pair is checked too — the `401` is owed *before* the job is
///   resolved, and an anonymous caller who could tell the two ids apart would be
///   enumerating the instance with no account at all.
#[tokio::test]
async fn another_users_private_job_is_indistinguishable_from_an_unused_id() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) = register_full(&base, "ws_owner", "ws_owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "ws_outsider", "ws_outsider@example.com").await;
    let job_id = create_private_job(&base, &db, &owner_token, owner_id).await;

    let real = refusal(&base, job_id, Some(&outsider_token)).await;
    let absent = refusal(&base, ABSENT_JOB_ID, Some(&outsider_token)).await;
    assert_ne!(
        real.status,
        403,
        "the job-log handshake answered 403 for a job in a private repository the caller has \
         no rights to: the refusal confirmed the id exists, which is the one thing a masked \
         denial may not do — body: {}",
        real.excerpt()
    );
    assert_eq!(
        real.shape(),
        absent.shape(),
        "an outsider is answered differently for another user's private job and for an id no \
         job ever had, so walking {{job_id}} enumerates every CI job on the instance.\n  \
         real   ({}): {}\n  absent ({}): {}",
        real.status,
        real.excerpt(),
        absent.status,
        absent.excerpt(),
    );
    assert!(
        real.speaks() && absent.speaks(),
        "both refusals came back with an empty body, so the comparison above asserted the \
         status alone — which is exactly the half the oracle moves out of"
    );

    // No session at all: still refused before the job is resolved, and refused
    // identically, so the `401` is not itself the oracle.
    let anon_real = refusal(&base, job_id, None).await;
    let anon_absent = refusal(&base, ABSENT_JOB_ID, None).await;
    assert_eq!(
        anon_real.status, 401,
        "an anonymous caller must be turned away before the job is looked up"
    );
    assert_eq!(
        anon_real.shape(),
        anon_absent.shape(),
        "an anonymous caller can tell a real job from an absent one, so the route enumerates \
         private rows to callers with no account at all.\n  real   ({}): {}\n  absent ({}): {}",
        anon_real.status,
        anon_real.excerpt(),
        anon_absent.status,
        anon_absent.excerpt(),
    );

    // The baseline, in the same run: the owner still gets a socket on that very
    // job. Without it every refusal above is equally good evidence of a dead
    // fixture.
    let (mut socket, response) =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, Some(&owner_token)))
            .await
            .expect("the owner of the repository must reach their own job's log socket");
    assert_eq!(response.status(), 101);
    assert_eq!(
        response.headers()["sec-websocket-protocol"]
            .to_str()
            .unwrap(),
        format!("bearer.{owner_token}")
    );
    socket.close(None).await.unwrap();
}

/// card_f7128fd4a2e2, the job-log half: `check_read_for` runs once, at the
/// handshake, and a socket that is already open never asks again. The account
/// need not go anywhere for that answer to expire — flipping the repository to
/// private revokes an outsider's read on the spot, and the log stream must not
/// be the one place that keeps honouring the old answer.
#[tokio::test]
async fn a_repository_turning_private_closes_an_outsider_job_log_socket() {
    use futures::StreamExt;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use tokio_tungstenite::tungstenite::Message;

    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        ws_session_recheck_secs: Some(1),
        ..Default::default()
    })
    .await;
    let (owner_token, owner_id) = register_full(&base, "ws_pub_owner", "ws_pub@example.com").await;
    let (reader_token, _) = register_full(&base, "ws_pub_reader", "ws_reader@example.com").await;
    let (repo_id, job_id) =
        create_job_in_repo(&base, &db, &owner_token, owner_id, "public-ws", false).await;

    let (mut socket, response) =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, Some(&reader_token)))
            .await
            .expect("a public repository's job logs are readable by anyone");
    assert_eq!(response.status(), 101);

    let welcome = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the socket must greet a reader who may read")
        .expect("the socket closed before the welcome frame")
        .expect("the welcome frame must be readable");
    assert!(
        matches!(&welcome, Message::Text(text) if text.contains("\"connected\"")),
        "unexpected welcome frame: {welcome:?}"
    );

    // Baseline on live access: several re-check intervals with the repository
    // still public, and the socket stays up.
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(4), socket.next())
            .await
            .is_err(),
        "the re-check closed a socket whose reader may still read"
    );

    let repository = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut active: rg_db::entities::repository::ActiveModel = repository.into();
    active.is_private = Set(true);
    active.update(&db).await.unwrap();

    let ended = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(frame) = socket.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await;
    assert!(
        ended.unwrap_or(false),
        "the job-log socket kept streaming a repository its reader may no longer read"
    );
}

/// card_7898025803a6, the job-log half: the re-check above re-asks the read
/// gate and the account's standing, and both keep answering "yes" after a
/// `POST /users/logout` — logging out revokes the *session*, deliberately
/// leaving the account and its rights exactly where they were. So the one thing
/// a user does to end a stream on a machine they are walking away from was the
/// one thing this socket could not see.
#[tokio::test]
async fn a_logout_closes_a_job_log_socket_the_revoked_session_opened() {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        ws_session_recheck_secs: Some(1),
        ..Default::default()
    })
    .await;
    let (owner_token, owner_id) =
        register_full(&base, "ws_job_logout", "ws_job_logout@example.com").await;
    let (_repo_id, job_id) =
        create_job_in_repo(&base, &db, &owner_token, owner_id, "logout-ws", true).await;

    let (mut socket, response) =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, Some(&owner_token)))
            .await
            .expect("the owner of the repository must reach their own job's log socket");
    assert_eq!(response.status(), 101);

    let welcome = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the socket must greet a reader who may read")
        .expect("the socket closed before the welcome frame")
        .expect("the welcome frame must be readable");
    assert!(
        matches!(&welcome, Message::Text(text) if text.contains("\"connected\"")),
        "unexpected welcome frame: {welcome:?}"
    );

    // Baseline: several re-check intervals on a session in good standing, so
    // the close below means "revoked" and not "this socket was never going to
    // survive".
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(4), socket.next())
            .await
            .is_err(),
        "the re-check closed a socket whose session is in good standing"
    );

    let logout = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200, "logout failed");

    let ended = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(frame) = socket.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await;
    assert!(
        ended.unwrap_or(false),
        "the job-log socket kept streaming to a session its owner had logged out of"
    );
}

#[tokio::test]
async fn job_websocket_rejects_invalid_token_before_upgrade() {
    let (base, _) = spawn_test_app_with_db().await;
    let denied =
        tokio_tungstenite::connect_async(websocket_request(&base, 999, Some("invalid"))).await;
    match denied {
        Err(Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected unauthorized WebSocket handshake, got {other:?}"),
    }
}
