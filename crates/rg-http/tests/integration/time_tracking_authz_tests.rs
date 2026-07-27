//! Authorization for the issue time-tracking write paths.
//!
//! Both write routes used to stop at "the bearer token parses":
//!
//!   POST   /repos/:o/:r/issues/:n/time      — no repository check at all, so
//!                                             any account could log hours onto
//!                                             a private repository's issues.
//!   DELETE /repos/:o/:r/issues/:n/time/:id  — the owner, name and issue number
//!                                             were unbound and the entry was
//!                                             deleted by its *global* id, so
//!                                             naming a repository of one's own
//!                                             deleted any entry on the
//!                                             instance.
//!
//! Guards that both now require write access to the repository in the path, and
//! that `{id}` is re-anchored to the issue it was authorized against — a
//! mismatch answering `404`, not `403`, so the route cannot be walked to learn
//! which entry ids exist.

use crate::common::{create_issue, create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

struct Fixture {
    base: String,
    attacker: String,
    attacker_token: String,
    attacker_repo: String,
    attacker_issue: i64,
    victim: String,
    victim_token: String,
    victim_repo: String,
    victim_issue: i64,
    victim_entry_id: i64,
}

/// Create a repository with an explicit visibility — `create_repo` only makes
/// the default (public) kind.
async fn create_repo_with_visibility(base: &str, token: &str, name: &str, private: bool) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": private}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create_repo({name}) should succeed");
}

/// Two users with a repository and a tracked issue each; only the victim's
/// issue carries a time entry.
async fn setup(suffix: &str) -> Fixture {
    setup_with_visibility(suffix, false).await
}

/// [`setup`] with the victim's repository made private on request: the defect
/// was reported against a private tracker, so the regression has to be able to
/// stand one up.
async fn setup_with_visibility(suffix: &str, victim_repo_private: bool) -> Fixture {
    let base = spawn_test_app().await;

    let attacker = format!("ttattacker{suffix}");
    let attacker_token = register_user(
        &base,
        &attacker,
        &format!("ttattacker{suffix}@example.com"),
        PW,
    )
    .await;
    let attacker_repo = format!("ttattackerrepo{suffix}");
    create_repo(&base, &attacker_token, &attacker_repo).await;
    let (_, attacker_issue) = create_issue(
        &base,
        &attacker_token,
        &attacker,
        &attacker_repo,
        "Attacker task",
    )
    .await;

    let victim = format!("ttvictim{suffix}");
    let victim_token =
        register_user(&base, &victim, &format!("ttvictim{suffix}@example.com"), PW).await;
    let victim_repo = format!("ttvictimrepo{suffix}");
    create_repo_with_visibility(&base, &victim_token, &victim_repo, victim_repo_private).await;
    let (_, victim_issue) =
        create_issue(&base, &victim_token, &victim, &victim_repo, "Victim task").await;

    let victim_entry_id = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/issues/{victim_issue}/time"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"duration_minutes": 45, "description": "victim work"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["id"]
        .as_i64()
        .expect("time entry id");

    Fixture {
        base,
        attacker,
        attacker_token,
        attacker_repo,
        attacker_issue,
        victim,
        victim_token,
        victim_repo,
        victim_issue,
        victim_entry_id,
    }
}

/// How many entries the victim's issue still has, read as the victim.
async fn victim_entry_count(f: &Fixture) -> usize {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time",
            f.base, f.victim, f.victim_repo, f.victim_issue
        ))
        .bearer_auth(&f.victim_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["data"].as_array().expect("data array").len()
}

#[tokio::test]
async fn logging_time_requires_write_access_to_the_repository() {
    let f = setup("1").await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/v1/repos/{}/{}/issues/{}/time",
        f.base, f.victim, f.victim_repo, f.victim_issue
    );

    let resp = client
        .post(&url)
        .json(&serde_json::json!({"duration_minutes": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "anonymous add_time must be 401");

    let resp = client
        .post(&url)
        .bearer_auth(&f.attacker_token)
        .json(&serde_json::json!({"duration_minutes": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "an outsider with a valid token must not log time on someone else's issue"
    );

    assert_eq!(
        victim_entry_count(&f).await,
        1,
        "no entry may have been appended by the rejected calls"
    );
}

#[tokio::test]
async fn deleting_a_time_entry_is_scoped_to_its_repository() {
    let f = setup("2").await;
    let client = reqwest::Client::new();

    // The attacker has write access to their own repository, and uses its route
    // to address an entry id that lives in the victim's.
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.attacker, f.attacker_repo, f.attacker_issue, f.victim_entry_id
        ))
        .bearer_auth(&f.attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "DELETE of a foreign time entry must be 404, not a delete"
    );

    // Naming the victim's own route instead is a plain permission failure.
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.victim, f.victim_repo, f.victim_issue, f.victim_entry_id
        ))
        .bearer_auth(&f.attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "an outsider must not delete an entry through the owning repository either"
    );

    assert_eq!(
        victim_entry_count(&f).await,
        1,
        "the victim's time entry was deleted"
    );
}

#[tokio::test]
async fn deleting_a_time_entry_requires_authentication() {
    let f = setup("3").await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.victim, f.victim_repo, f.victim_issue, f.victim_entry_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "anonymous delete must be 401");

    assert_eq!(victim_entry_count(&f).await, 1, "the entry was deleted");
}

/// The scoping check must not have turned every delete into a 404: the entry's
/// own repository still deletes it, and an id that does not exist anywhere is
/// the same 404 as one that lives elsewhere.
#[tokio::test]
async fn the_owning_repository_still_deletes_its_own_entry() {
    let f = setup("4").await;
    let client = reqwest::Client::new();

    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base,
            f.victim,
            f.victim_repo,
            f.victim_issue,
            f.victim_entry_id + 100_000
        ))
        .bearer_auth(&f.victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "an unknown entry id must be 404");

    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.victim, f.victim_repo, f.victim_issue, f.victim_entry_id
        ))
        .bearer_auth(&f.victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "the owner must still delete their entry"
    );

    assert_eq!(victim_entry_count(&f).await, 0, "the entry survived");
}

/// The defect was reported against a *private* tracker: an outsider appended
/// hours to it and deleted an entry out of it through a repository of their
/// own. A write gate answers the same for either visibility, which is exactly
/// why the private case is worth pinning — the repository nobody may even read
/// was the one being written to.
#[tokio::test]
async fn the_private_write_surface_is_closed_to_outsiders() {
    let f = setup_with_visibility("5", true).await;
    let client = reqwest::Client::new();
    let victim_url = format!(
        "{}/api/v1/repos/{}/{}/issues/{}/time",
        f.base, f.victim, f.victim_repo, f.victim_issue
    );

    let resp = client
        .post(&victim_url)
        .json(&serde_json::json!({"duration_minutes": 90, "description": "classified"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "anonymous add_time must be 401");

    let resp = client
        .post(&victim_url)
        .bearer_auth(&f.attacker_token)
        .json(&serde_json::json!({"duration_minutes": 90, "description": "intruder"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "an outsider must not log time on a private repository's issue"
    );

    // The entry lives in the private repository; the attacker addresses it
    // through their own, which is the route that used to delete it.
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.attacker, f.attacker_repo, f.attacker_issue, f.victim_entry_id
        ))
        .bearer_auth(&f.attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "a private repository's entry must not be reachable through another route"
    );

    assert_eq!(
        victim_entry_count(&f).await,
        1,
        "the private repository's time log was modified"
    );

    // Baseline in the same test: the gate closed the route to outsiders without
    // closing it to the repository's own owner.
    let resp = client
        .post(&victim_url)
        .bearer_auth(&f.victim_token)
        .json(&serde_json::json!({"duration_minutes": 15, "description": "owner work"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the owner must still log time");

    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            f.base, f.victim, f.victim_repo, f.victim_issue, f.victim_entry_id
        ))
        .bearer_auth(&f.victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "the owner must still delete their entry"
    );

    assert_eq!(
        victim_entry_count(&f).await,
        1,
        "exactly the owner's own two writes should be visible"
    );
}
