//! The actor column names the person who acted, and nothing else.
//!
//! Four private copies of `record_audit` gave the column four meanings at once:
//! a real username, the numeric `claims.sub`, an empty string, and — from
//! `orgs.rs`, with a comment saying so — the name of the *organization* being
//! acted on. `/admin/audit` renders it as `{username} (#{user_id})`, so one list
//! read `alice (#7)`, `7 (#7)`, a blank actor, and `acme-corp (#3)`
//! (card_fcc07f8d1505, card_51f6f3a99003).
//!
//! `audit_writer_guard` holds the structural half — one writer, and an actor
//! that has no constructor taking a name. This file holds the behavioural half,
//! and it is not redundant with it: the guard cannot tell whether the *right*
//! account was looked up, only that a name could not be invented. So every
//! assertion below goes through HTTP and reads the journal back through
//! `GET /admin/audit/logs`, the same door an operator uses.
//!
//! Two coverage cards are asserted here too, because "the column is right" is
//! worth nothing for an action that writes no row at all: the four team
//! mutations that hand out access to private repositories (card_cb0d1ca78d57)
//! and the package deletions that are the registry's only irreversible
//! operation (card_6baa3e341bf3).

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

/// Every audit row the instance holds, newest first.
async fn journal(base: &str, admin_token: &str) -> Vec<serde_json::Value> {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/admin/audit/logs"))
        .query(&[("per_page", "100")])
        .bearer_auth(admin_token)
        .send()
        .await
        .expect("read the audit journal");
    let status = response.status();
    let body = response.text().await.expect("journal body");
    assert_eq!(status, 200, "reading the journal failed: {body}");
    serde_json::from_str::<serde_json::Value>(&body).expect("journal is JSON")["logs"]
        .as_array()
        .expect("`logs` array")
        .clone()
}

/// The single row for `action`, or a readable failure naming what was there.
async fn one_entry(base: &str, admin_token: &str, action: &str) -> serde_json::Value {
    let rows = journal(base, admin_token).await;
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| row["action"] == action)
        .cloned()
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one `{action}` row, found {}: the journal holds {:?}",
        matching.len(),
        rows.iter()
            .map(|row| row["action"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    matching.into_iter().next().unwrap()
}

/// The assertion the whole phase is about.
fn assert_actor_is(entry: &serde_json::Value, username: &str, user_id: i64) {
    assert_eq!(
        entry["username"], username,
        "the actor column of `{}` does not name the account that acted: {entry}",
        entry["action"]
    );
    assert_eq!(
        entry["user_id"], user_id,
        "the actor id of `{}` is not the account that acted: {entry}",
        entry["action"]
    );
}

/// `repo.create` / `repo.delete` / `repo.fork` / `repo.transfer` — the four the
/// card names, in one run so the journal is read once with all of them in it.
#[tokio::test]
async fn every_repository_action_names_the_account_that_performed_it() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "act-owner", "act-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let (other_token, other_id) = register_full(&base, "act-other", "act-other@example.com").await;

    let client = reqwest::Client::new();
    create_repo(&base, &token, "kept").await;
    create_repo(&base, &token, "doomed").await;
    create_repo(&base, &token, "moving").await;

    assert_eq!(
        client
            .delete(format!("{base}/api/v1/repos/act-owner/doomed"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete the repository")
            .status(),
        200
    );

    // The fork is performed by the *other* account, which is what separates
    // "the actor" from "whoever owns the thing being acted on".
    let forked = client
        .post(format!("{base}/api/v1/repos/act-owner/kept/fork"))
        .bearer_auth(&other_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("fork the repository");
    assert_eq!(
        forked.status(),
        201,
        "fork failed: {}",
        forked.text().await.unwrap_or_default()
    );

    // A transfer may only land where the caller could have created the
    // repository themselves (card_934b6037bcda), so the destination is an
    // organization this account owns rather than the other account.
    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "act-org"}))
            .send()
            .await
            .expect("create the destination organization")
            .status(),
        201
    );
    let transferred = client
        .post(format!("{base}/api/v1/repos/act-owner/moving/transfer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"new_owner": "act-org"}))
        .send()
        .await
        .expect("transfer the repository");
    assert_eq!(
        transferred.status(),
        200,
        "transfer failed: {}",
        transferred.text().await.unwrap_or_default()
    );

    let rows = journal(&base, &token).await;
    let by_action = |action: &str| {
        rows.iter()
            .find(|row| row["action"] == action)
            .unwrap_or_else(|| panic!("no `{action}` row in the journal"))
            .clone()
    };

    assert_actor_is(&by_action("repo.create"), "act-owner", actor_id);
    assert_actor_is(&by_action("repo.delete"), "act-owner", actor_id);
    assert_actor_is(&by_action("repo.transfer"), "act-owner", actor_id);
    assert_actor_is(&by_action("repo.fork"), "act-other", other_id);
}

/// The organization is the resource. It used to be the actor.
#[tokio::test]
async fn an_organization_action_names_the_person_and_not_the_organization() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "org-actor", "org-actor@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let (_, member_id) = register_full(&base, "org-member", "org-member@example.com").await;

    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "acme-corp"}))
        .send()
        .await
        .expect("create the organization");
    assert_eq!(
        created.status(),
        201,
        "org create failed: {}",
        created.text().await.unwrap_or_default()
    );

    // A second admin who does *not* own the organization. Without this the
    // acting account and the org's owner are the same row, and the test cannot
    // tell "the actor" from "whoever owns the thing being acted on" — which is
    // exactly the confusion this column had.
    let (deputy_token, deputy_id) =
        register_full(&base, "org-deputy", "org-deputy@example.com").await;
    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs/acme-corp/members"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"user_id": deputy_id, "role": "admin"}))
            .send()
            .await
            .expect("promote a second org admin")
            .status(),
        201
    );
    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs/acme-corp/members"))
            .bearer_auth(&deputy_token)
            .json(&serde_json::json!({"user_id": member_id, "role": "member"}))
            .send()
            .await
            .expect("add an org member as the deputy")
            .status(),
        201
    );

    let create = one_entry(&base, &token, "org.create").await;
    assert_actor_is(&create, "org-actor", actor_id);
    assert_ne!(
        create["username"], "acme-corp",
        "the organization is being recorded as the account that acted"
    );
    // …and the org's name is still recorded, in the column that means it.
    assert_eq!(create["resource_name"], "acme-corp");

    // Two `org.add_member` rows now: the owner promoting the deputy, then the
    // deputy adding the member. The *second* is the one that separates the
    // actor from the owner, so it is the one asserted.
    let grants: Vec<_> = journal(&base, &token)
        .await
        .into_iter()
        .filter(|row| row["action"] == "org.add_member")
        .collect();
    assert_eq!(
        grants.len(),
        2,
        "expected both membership grants: {grants:?}"
    );
    let by_deputy = grants
        .iter()
        .find(|row| row["user_id"] == deputy_id)
        .unwrap_or_else(|| {
            panic!("no `org.add_member` row names the deputy who performed it: {grants:?}")
        });
    assert_actor_is(by_deputy, "org-deputy", deputy_id);
    assert_ne!(
        by_deputy["user_id"], actor_id,
        "the organization's owner was recorded as the actor for something the deputy did"
    );
}

/// card_cb0d1ca78d57: the team surface grants access to private repositories,
/// and it wrote nothing at all — `org.add_member` was journalled while the
/// operation that actually confers the repository permission was not.
#[tokio::test]
async fn every_team_mutation_leaves_a_row_naming_who_granted_the_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "team-owner", "team-owner@example.com").await;
    promote_user_to_admin(&db, owner_id).await;
    let (_, member_id) = register_full(&base, "team-member", "team-member@example.com").await;

    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({"name": "team-org"}))
            .send()
            .await
            .expect("create the organization")
            .status(),
        201
    );

    // Every team mutation below is performed by an org admin who does not own
    // the organization, so "the actor" and "the org's owner" are different rows
    // and the assertions can tell them apart.
    let (token, actor_id) = register_full(&base, "team-actor", "team-actor@example.com").await;
    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs/team-org/members"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({"user_id": actor_id, "role": "admin"}))
            .send()
            .await
            .expect("promote the acting account to org admin")
            .status(),
        201
    );

    // Baseline: the refusal has to be earned by the writes below, not by a
    // journal that happens to be empty of everything.
    assert!(
        journal(&base, &owner_token)
            .await
            .iter()
            .all(|row| row["action"] != "team.create"),
        "a `team.create` row existed before any team was created"
    );

    let created = client
        .post(format!("{base}/api/v1/orgs/team-org/teams"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "reviewers", "permission": "write"}))
        .send()
        .await
        .expect("create the team");
    assert_eq!(
        created.status(),
        201,
        "team create failed: {}",
        created.text().await.unwrap_or_default()
    );
    let team_id = created.json::<serde_json::Value>().await.expect("body")["id"]
        .as_i64()
        .expect("the new team's id");

    assert_eq!(
        client
            .post(format!(
                "{base}/api/v1/orgs/team-org/teams/{team_id}/members"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"user_id": member_id}))
            .send()
            .await
            .expect("add a team member")
            .status(),
        201
    );
    assert_eq!(
        client
            .delete(format!(
                "{base}/api/v1/orgs/team-org/teams/{team_id}/members/{member_id}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .expect("remove the team member")
            .status(),
        200
    );
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/orgs/team-org/teams/{team_id}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete the team")
            .status(),
        200
    );

    for action in [
        "team.create",
        "team.add_member",
        "team.remove_member",
        "team.delete",
    ] {
        let entry = one_entry(&base, &owner_token, action).await;
        assert_actor_is(&entry, "team-actor", actor_id);
        assert_ne!(
            entry["user_id"], owner_id,
            "`{action}` recorded the organization's owner instead of the admin who acted: {entry}"
        );
        assert_eq!(
            entry["resource_type"], "team",
            "`{action}` is not filed against the team it changed: {entry}"
        );
        assert_eq!(entry["resource_name"], "reviewers");
    }

    // The membership grant has to say *who* was granted access, or the row
    // answers "somebody changed a team" rather than "this account got in".
    let granted = one_entry(&base, &owner_token, "team.add_member").await;
    let details: serde_json::Value =
        serde_json::from_str(granted["details"].as_str().expect("details are recorded"))
            .expect("details are JSON");
    assert_eq!(details["member_user_id"], member_id);
    assert_eq!(details["team_permission"], "write");
}

/// An action no account performed is `null`, never `""`.
///
/// The distinction is the whole reason the column is nullable: a blank string
/// renders as a blank actor, which is what a name that failed to load also looks
/// like. One of those is a fact about the action; the other is a fact about the
/// database being unwell.
#[tokio::test]
async fn an_action_with_no_account_comes_back_as_null_and_not_an_empty_string() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = register_full(&base, "null-actor", "null-actor@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    rg_core::audit::record(
        &db,
        &rg_core::audit::AuditActor::none(),
        "repo.delete",
        Some("repo"),
        Some(1),
        Some("swept/by-a-scheduler"),
        None,
        None,
    )
    .await;

    let entry = journal(&base, &token)
        .await
        .into_iter()
        .find(|row| row["resource_name"] == "swept/by-a-scheduler")
        .expect("the system-written row");
    assert!(
        entry["username"].is_null(),
        "an action with no account came back as {} rather than null",
        entry["username"]
    );
    assert!(entry["user_id"].is_null(), "{entry}");
}

/// card_86f40189bc71 / card_e2bd7026c87d: `admin.unlock_user` is the one place
/// in the workspace that resolved its actor with
/// `.ok().flatten().unwrap_or_default()` — folding "the query failed" and "no
/// such account" into `""`, and then resetting the target's login failures
/// anyway. The record of who unlocked an account carried a blank author and
/// looked routine.
///
/// What is asserted here is the healthy path and the invariant. The *failure*
/// path is asserted in `rg_core::audit`'s own tests, and deliberately so: an
/// end-to-end "the actor SELECT failed" case is not constructible against this
/// route, because `InstanceAdmin` reads the same `users` table before the
/// handler body runs — a users-table outage is turned away by the gate and
/// never reaches the actor lookup at all. Faking one here would be a test of
/// the fixture, not of the route.
#[tokio::test]
async fn unlocking_an_account_records_the_admin_who_did_it() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = register_full(&base, "unlock-admin", "unlock-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (_, locked_id) = register_full(&base, "locked-out", "locked-out@example.com").await;

    // Something for the unlock to actually undo, so a `200` is not vacuous.
    for _ in 0..3 {
        rg_db::ops::user_ops::record_failed_login(&db, locked_id, 10)
            .await
            .expect("record a failed login");
    }
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, locked_id)
            .await
            .expect("read the locked account")
            .expect("the locked account")
            .login_attempts
            > 0,
        "the fixture did not lock anything, so the unlock below proves nothing"
    );

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/admin/users/{locked_id}/unlock"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("unlock the account");
    assert_eq!(
        response.status(),
        200,
        "unlock failed: {}",
        response.text().await.unwrap_or_default()
    );

    let entry = one_entry(&base, &token, "admin.unlock_user").await;
    assert_actor_is(&entry, "unlock-admin", admin_id);
    assert_eq!(
        entry["resource_name"], "locked-out",
        "the row does not say whose account was unlocked: {entry}"
    );

    // The invariant, over the whole journal rather than this one row: an entry
    // that knows the actor's id must not be missing the actor's name. That is
    // the shape `unwrap_or_default()` produced, and it is what a reader cannot
    // tell from a name that failed to load.
    for row in journal(&base, &token).await {
        if row["user_id"].is_null() {
            continue;
        }
        assert!(
            !row["username"].is_null() && row["username"] != "",
            "an audit row names an actor id with no name: {row}"
        );
    }
}

/// card_6baa3e341bf3: publishing a package is attributed forever — `author_id`
/// sits on the version row — and deleting one removed that row and left nothing
/// behind. The registry's only irreversible operation was its only unattributed
/// one.
#[tokio::test]
async fn deleting_and_yanking_a_package_version_records_who_did_it() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "pkg-actor", "pkg-actor@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    create_repo(&base, &token, "registry").await;

    let client = reqwest::Client::new();
    let publish = |version: &str| {
        let url = format!(
            "{base}/api/v1/repos/pkg-actor/registry/packages/generic/publish?name=sample&version={version}"
        );
        let token = token.clone();
        let client = client.clone();
        async move {
            let response = client
                .post(url)
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"sample.bin\"",
                )
                .body("payload")
                .send()
                .await
                .expect("publish the package version");
            let status = response.status();
            assert!(
                status == 201 || status == 200,
                "publish failed: {status} {}",
                response.text().await.unwrap_or_default()
            );
        }
    };
    publish("1.0.0").await;
    publish("2.0.0").await;

    // Baseline: publishing alone writes no `package.delete`, so the assertions
    // below are earned by the delete and not by a journal full of everything.
    assert!(
        journal(&base, &token)
            .await
            .iter()
            .all(|row| row["action"] != "package.delete"),
        "a `package.delete` row existed before anything was deleted"
    );

    assert_eq!(
        client
            .patch(format!(
                "{base}/api/v1/repos/pkg-actor/registry/packages/generic/sample/2.0.0/yank"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"yank": true}))
            .send()
            .await
            .expect("yank the version")
            .status(),
        200
    );
    assert_eq!(
        client
            .delete(format!(
                "{base}/api/v1/repos/pkg-actor/registry/packages/generic/sample/1.0.0"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete the version")
            .status(),
        204
    );

    for (action, version) in [("package.delete", "1.0.0"), ("package.yank", "2.0.0")] {
        let entry = one_entry(&base, &token, action).await;
        assert_actor_is(&entry, "pkg-actor", actor_id);
        assert_eq!(entry["resource_type"], "package_version");
        assert_eq!(
            entry["resource_name"],
            format!("pkg-actor/registry/sample@{version}"),
            "`{action}` does not name the version it acted on: {entry}"
        );
        let details: serde_json::Value =
            serde_json::from_str(entry["details"].as_str().expect("details are recorded"))
                .expect("details are JSON");
        assert_eq!(details["pkg_type"], "generic");
        assert_eq!(details["version"], version);
    }
}
