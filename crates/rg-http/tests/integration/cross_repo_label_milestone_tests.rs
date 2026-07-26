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
