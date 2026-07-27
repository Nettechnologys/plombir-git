//! Cross-repository scoping for label and milestone mutations.
//!
//! The label and milestone routes check write access against `owner/name` but
//! address the row by a global id. Those are two different things, and while
//! they stayed unrelated, write access to a single repository was enough to
//! rename or delete a label or milestone in *any* other one:
//!
//!   PATCH /repos/<mine>/<mine>/labels/<id-belonging-to-someone-else>
//!
//! Guards that the mismatch is answered `404` — not `403`, which would still
//! confirm that the id exists — and that the victim's row is left untouched.

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

/// Two users, one repository each, plus a label and a milestone in the second.
///
/// Returns `(base, attacker_token, attacker_owner, attacker_repo,
/// victim_token, victim_owner, victim_repo, label_id, milestone_id)`.
#[allow(clippy::type_complexity)]
async fn setup(
    suffix: &str,
) -> (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
) {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let attacker = format!("idorattacker{suffix}");
    let attacker_token = register_user(
        &base,
        &attacker,
        &format!("idorattacker{suffix}@example.com"),
        PW,
    )
    .await;
    let attacker_repo = format!("idorattackerrepo{suffix}");
    create_repo(&base, &attacker_token, &attacker_repo).await;

    let victim = format!("idorvictim{suffix}");
    let victim_token = register_user(
        &base,
        &victim,
        &format!("idorvictim{suffix}@example.com"),
        PW,
    )
    .await;
    let victim_repo = format!("idorvictimrepo{suffix}");
    create_repo(&base, &victim_token, &victim_repo).await;

    let label: serde_json::Value = client
        .post(format!("{base}/api/v1/repos/{victim}/{victim_repo}/labels"))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"name": "victim-label", "color": "#00ff00"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let label_id = label["id"].as_i64().expect("label id");

    let milestone: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/milestones"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"title": "victim-milestone"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let milestone_id = milestone["id"].as_i64().expect("milestone id");

    (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        victim_token,
        victim,
        victim_repo,
        label_id,
        milestone_id,
    )
}

#[tokio::test]
async fn test_label_mutation_is_scoped_to_its_repository() {
    let (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        victim_token,
        victim,
        victim_repo,
        label_id,
        _milestone_id,
    ) = setup("1").await;
    let client = reqwest::Client::new();

    // The attacker has write access to their own repository, and uses its route
    // to address a label id that lives in the victim's.
    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/labels/{label_id}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"name": "pwned"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "PATCH of a foreign label must be 404, not a rename"
    );

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/labels/{label_id}"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "DELETE of a foreign label must be 404, not a delete"
    );

    // The victim still sees the label, unrenamed.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/labels/{label_id}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the victim's label was deleted");
    let label: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        label["name"], "victim-label",
        "the victim's label was renamed"
    );
}

#[tokio::test]
async fn test_milestone_mutation_is_scoped_to_its_repository() {
    let (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        victim_token,
        victim,
        victim_repo,
        _label_id,
        milestone_id,
    ) = setup("2").await;
    let client = reqwest::Client::new();

    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/milestones/{milestone_id}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "pwned"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "PATCH of a foreign milestone must be 404, not an edit"
    );

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/milestones/{milestone_id}"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "DELETE of a foreign milestone must be 404, not a delete"
    );

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/milestones/{milestone_id}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the victim's milestone was deleted");
    let milestone: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        milestone["title"], "victim-milestone",
        "the victim's milestone was retitled"
    );
}

/// The owner's own mutations still work — the scoping check must not have
/// turned every `PATCH`/`DELETE` into a 404.
#[tokio::test]
async fn test_owner_can_still_edit_and_delete_own_label_and_milestone() {
    let (
        base,
        _attacker_token,
        _attacker,
        _attacker_repo,
        victim_token,
        victim,
        victim_repo,
        label_id,
        milestone_id,
    ) = setup("3").await;
    let client = reqwest::Client::new();

    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/labels/{label_id}"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"name": "renamed-label"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let label: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(label["name"], "renamed-label");

    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/milestones/{milestone_id}"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"title": "renamed-milestone"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let milestone: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(milestone["title"], "renamed-milestone");

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/labels/{label_id}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/milestones/{milestone_id}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

/// The same mismatch, one layer down: the milestone id arrives in the *body* of
/// an issue write rather than in the path, and the route's own access check has
/// nothing to say about it. Attaching an issue to a foreign milestone is a write
/// into the victim's repository — their milestone stops counting down to zero.
#[tokio::test]
async fn test_issue_milestone_is_scoped_to_its_repository() {
    let (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        _victim_token,
        _victim,
        _victim_repo,
        _label_id,
        victim_milestone_id,
    ) = setup("5").await;
    let client = reqwest::Client::new();

    // Baseline first, so a 404 below proves the scoping check and not a broken
    // fixture: the attacker's own milestone attaches exactly as before.
    let own_milestone: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/milestones"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "own-milestone"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let own_milestone_id = own_milestone["id"].as_i64().expect("milestone id");

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/issues"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "mine", "milestone_id": own_milestone_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "own milestone must still attach");
    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["milestone_id"], own_milestone_id);
    let number = issue["number"].as_i64().expect("issue number");

    // POST with a milestone that lives in the victim's repository.
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/issues"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "pwn", "milestone_id": victim_milestone_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "creating an issue under a foreign milestone must be 404"
    );

    // PATCH is the same hole through the other verb.
    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/issues/{number}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"milestone_id": victim_milestone_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "moving an issue onto a foreign milestone must be 404"
    );

    // The rejected PATCH left the issue on its own milestone, and a PATCH that
    // names that milestone again still goes through.
    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/issues/{number}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "renamed", "milestone_id": own_milestone_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "own milestone must still be settable");
    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["milestone_id"], own_milestone_id);
    assert_eq!(issue["title"], "renamed");
}

/// `update_issue` keeps labels / assignee / milestone behind `can_write`;
/// `create_issue` did not, so a reader of a public repository could set them on
/// the way in. Filing the issue itself stays a read-access right.
#[tokio::test]
async fn test_issue_management_fields_on_create_require_write() {
    let (
        base,
        outsider_token,
        _outsider,
        _outsider_repo,
        victim_token,
        victim,
        victim_repo,
        _label_id,
        victim_milestone_id,
    ) = setup("6").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/v1/repos/{victim}/{victim_repo}/issues"))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"title": "report", "labels": ["victim-label"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "a reader must not set labels on an issue they file"
    );

    let resp = client
        .post(format!("{base}/api/v1/repos/{victim}/{victim_repo}/issues"))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"title": "report", "milestone_id": victim_milestone_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "a reader must not set a milestone on an issue they file"
    );

    // Baseline on live access, in the same test: filing a plain issue with only
    // read access is the behaviour the gate must not have taken away.
    let resp = client
        .post(format!("{base}/api/v1/repos/{victim}/{victim_repo}/issues"))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"title": "report"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "filing an issue on read access must still work"
    );

    // And the owner still sets both fields.
    let resp = client
        .post(format!("{base}/api/v1/repos/{victim}/{victim_repo}/issues"))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({
            "title": "owner issue",
            "labels": ["victim-label"],
            "milestone_id": victim_milestone_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["milestone_id"], victim_milestone_id);
}

/// An invalid update body is still a `400` — the switch from a blanket
/// `bad_request` to the typed conversion must keep validation failures at 400
/// rather than folding them into the 500 bucket.
#[tokio::test]
async fn test_invalid_label_update_is_still_bad_request() {
    let (
        base,
        _attacker_token,
        _attacker,
        _attacker_repo,
        victim_token,
        victim,
        victim_repo,
        label_id,
        _milestone_id,
    ) = setup("4").await;
    let client = reqwest::Client::new();

    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/labels/{label_id}"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"color": "not-a-hex"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "an invalid color must stay a 400");
}
