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
//! Four coverage cards are asserted here too, because "the column is right" is
//! worth nothing for an action that writes no row at all: the four team
//! mutations that hand out access to private repositories (card_cb0d1ca78d57),
//! the package deletions that are the registry's only irreversible operation
//! (card_6baa3e341bf3), the five endpoints that hand out access to a
//! *repository*, which wrote nothing whatsoever (card_06393f036456), and the
//! long-lived credentials an account carries, which wrote nothing either — and
//! whose entries carry the one risk none of the others do, of being written
//! with the secret in them (card_4a8cb474a877).

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

/// The five endpoints that grant access to a repository, in one run.
///
/// `access_grant_audit_guard` holds the structural half — an endpoint reaching
/// a grant write has to journal — and, as with the writer guard above, it
/// cannot see what the row says. This is the half that reads the entries back
/// and asks the question an incident review brings to them: **who was let in**.
/// An entry saying `added_user_id: 3` fails that question exactly the way the
/// form asking for "User ID" failed the owner in card_cb9f71672b11, which is
/// why every assertion below is on a name.
#[tokio::test]
async fn every_repository_access_grant_names_who_was_let_in() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "grant-owner", "grant-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let (_, grantee_id) = register_full(&base, "grantee", "grantee@example.com").await;
    create_repo(&base, &token, "granted").await;
    let client = reqwest::Client::new();
    let repo = format!("{base}/api/v1/repos/grant-owner/granted");

    // Every grant below is made *by name*, which is what the rest of this phase
    // fixed; the journal has to carry the name back out again.
    let added = client
        .post(format!("{repo}/collaborators"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"username": "grantee", "permission": "read"}))
        .send()
        .await
        .expect("add a collaborator");
    assert_eq!(added.status(), 201, "{}", added.text().await.unwrap());
    let membership_id = added.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("the membership row id");

    let promoted = client
        .patch(format!("{repo}/collaborators/{membership_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"permission": "write"}))
        .send()
        .await
        .expect("change a collaborator's permission");
    assert_eq!(promoted.status(), 200, "{}", promoted.text().await.unwrap());

    let protected_branch = client
        .post(format!("{repo}/branches/protection"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "allowed_push_users": ["grantee"],
        }))
        .send()
        .await
        .expect("protect a branch");
    assert_eq!(
        protected_branch.status(),
        201,
        "{}",
        protected_branch.text().await.unwrap()
    );

    let protected_tag = client
        .post(format!("{repo}/tags/protection"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"pattern": "v*", "allowed_users": ["grantee"]}))
        .send()
        .await
        .expect("protect a tag pattern");
    assert_eq!(
        protected_tag.status(),
        201,
        "{}",
        protected_tag.text().await.unwrap()
    );

    let environment = client
        .post(format!("{repo}/actions/environments"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approvers": ["grantee"],
        }))
        .send()
        .await
        .expect("create a protected environment");
    assert_eq!(
        environment.status(),
        201,
        "{}",
        environment.text().await.unwrap()
    );

    let removed = client
        .delete(format!("{repo}/collaborators/{grantee_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("remove the collaborator");
    assert_eq!(removed.status(), 204, "{}", removed.text().await.unwrap());

    async fn details(base: &str, token: &str, action: &str, actor_id: i64) -> serde_json::Value {
        let entry = one_entry(base, token, action).await;
        assert_actor_is(&entry, "grant-owner", actor_id);
        assert_eq!(
            entry["resource_name"], "grant-owner/granted",
            "`{action}` must name the repository it changed, owner and all: {entry}"
        );
        serde_json::from_str(
            entry["details"]
                .as_str()
                .unwrap_or_else(|| panic!("`{action}` recorded no details: {entry}")),
        )
        .expect("details are JSON")
    }

    let add = details(&base, &token, "repo.add_collaborator", actor_id).await;
    assert_eq!(add["added_username"], "grantee");
    assert_eq!(add["permission"], "read");

    let update = details(&base, &token, "repo.update_collaborator", actor_id).await;
    assert_eq!(update["username"], "grantee");
    assert_eq!(update["permission"], "write");

    // The allow-lists go in whole and by name — the question is "who could push
    // to `main` on the 14th", and a delta only answers it after every earlier
    // entry has been replayed.
    let branch = details(&base, &token, "repo.branch_protection_create", actor_id).await;
    assert_eq!(branch["branch"], "main");
    assert_eq!(branch["allowed_push_users"], serde_json::json!(["grantee"]));

    let tag = details(&base, &token, "repo.tag_protection_create", actor_id).await;
    assert_eq!(tag["pattern"], "v*");
    assert_eq!(tag["allowed_users"], serde_json::json!(["grantee"]));

    let deployment = details(&base, &token, "repo.environment_create", actor_id).await;
    assert_eq!(deployment["environment"], "production");
    assert_eq!(
        deployment["allowed_approvers"],
        serde_json::json!(["grantee"])
    );

    // Revocation is the half a review needs most, and it is also the half that
    // cannot name anybody after the fact: this path takes a `users.id` and the
    // membership row is gone by the time the entry is written.
    let removal = details(&base, &token, "repo.remove_collaborator", actor_id).await;
    assert_eq!(removal["removed_username"], "grantee");
    assert_eq!(removal["removed_user_id"], grantee_id);
}

/// A deploy key is the sixth way to hand out access to a repository, and the
/// journal has to say which way it was (card_2a9beaf7b207).
///
/// `read_only` is the assertion that matters. A key added with `read_only:
/// false` is push access for whoever holds the private half — `rg-ssh` reads
/// that column directly and lets `git-receive-pack` through on it — so an entry
/// that named only the key would answer "a key was added" and not "somebody can
/// now push", which is the question an incident review is asking.
///
/// The fingerprint for the same reason it is in the SSH-key entry beside this
/// one: the title is chosen by whoever adds the key and identifies nothing.
#[tokio::test]
async fn a_deploy_key_says_in_the_journal_whether_it_can_push() {
    const WRITABLE_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH2wYCBhBIcRlmB0kBQzXlqDQzXK5tYqMxV0kM6yYbP0 deploy";

    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "key-owner", "key-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    create_repo(&base, &token, "keyed").await;
    let client = reqwest::Client::new();
    let keys = format!("{base}/api/v1/repos/key-owner/keyed/keys");

    let added = client
        .post(&keys)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "deploy bot",
            "public_key": WRITABLE_KEY,
            "read_only": false,
        }))
        .send()
        .await
        .expect("add a deploy key");
    assert_eq!(added.status(), 201, "{}", added.text().await.unwrap());
    let added: serde_json::Value = added.json().await.expect("deploy key body");
    let key_id = added["id"].as_i64().expect("the deploy key id");
    let fingerprint = added["fingerprint"]
        .as_str()
        .expect("the deploy key fingerprint")
        .to_owned();

    let grant = one_entry(&base, &token, "repo.add_deploy_key").await;
    assert_actor_is(&grant, "key-owner", actor_id);
    assert_eq!(
        grant["resource_name"], "key-owner/keyed",
        "the entry must name the repository the key opens, owner and all: {grant}"
    );
    let details: serde_json::Value = serde_json::from_str(
        grant["details"]
            .as_str()
            .expect("`repo.add_deploy_key` recorded no details"),
    )
    .expect("details are JSON");
    assert_eq!(details["title"], "deploy bot");
    assert_eq!(details["fingerprint"], fingerprint);
    assert_eq!(
        details["read_only"], false,
        "without `read_only` the entry does not say whether this key can push: {details}"
    );

    let revoked = client
        .delete(format!("{keys}/{key_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("revoke the deploy key");
    assert_eq!(revoked.status(), 204, "{}", revoked.text().await.unwrap());

    // The revocation names what stopped working, read off the row before it was
    // deleted — "deploy key #4 was revoked" tells a review nothing.
    let removal = one_entry(&base, &token, "repo.remove_deploy_key").await;
    assert_actor_is(&removal, "key-owner", actor_id);
    let details: serde_json::Value = serde_json::from_str(
        removal["details"]
            .as_str()
            .expect("`repo.remove_deploy_key` recorded no details"),
    )
    .expect("details are JSON");
    assert_eq!(details["title"], "deploy bot");
    assert_eq!(details["fingerprint"], fingerprint);
    assert_eq!(details["read_only"], false);
}

/// Every credential an account can mint or revoke, in one run — and the
/// assertion that none of the entries carries the credential itself.
///
/// The journal is read by operators and served over an admin API. A token, its
/// hash, or a secret's ciphertext in `details` would turn it into a second
/// credential store, which is a strictly worse outcome than the silence this
/// card was about. So the coverage assertions and the leak assertion live in
/// one test: they are two halves of the same requirement.
#[tokio::test]
async fn credential_events_are_journalled_and_the_credential_itself_is_not() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "cred-owner", "cred-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    create_repo(&base, &token, "vault").await;
    let client = reqwest::Client::new();

    // 1. A personal access token. The response carries the only copy of the
    //    raw value, which is exactly what must not reach the journal.
    let minted = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "ci-bot", "scopes": "repo"}))
        .send()
        .await
        .expect("mint a token");
    assert_eq!(minted.status(), 201);
    let minted: serde_json::Value = minted.json().await.expect("token body");
    let raw_token = minted["token"].as_str().expect("the raw token").to_owned();
    let token_id = minted["id"].as_i64().expect("the token id");
    // Read while the row still exists: `token_hash` is the value the server
    // authenticates by, so it is the one field whose presence in the journal
    // would be worse than the silence this card was about — and after the
    // revocation below there is nothing left to compare against.
    let token_hash = rg_db::ops::token_ops::find_by_id(&db, token_id)
        .await
        .expect("read the token row")
        .expect("the token was just minted")
        .token_hash;

    // 2. An SSH key: the account's push credential from a given machine.
    const PUBLIC_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ4Wt1kZbmC9C8VJK9ay6PQBQqTPRfN6Cv6vd8gGz8xR audit";
    let added = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "laptop", "public_key": PUBLIC_KEY}))
        .send()
        .await
        .expect("add an SSH key");
    assert_eq!(added.status(), 201, "{}", added.text().await.unwrap());
    let added: serde_json::Value = added.json().await.expect("ssh key body");
    let key_id = added["id"].as_i64().expect("the key id");
    let fingerprint = added["fingerprint"]
        .as_str()
        .expect("the fingerprint")
        .to_owned();

    // 3. A repository CI secret: a value every job of the repository reads.
    const SECRET_VALUE: &str = "s3cr3t-deploy-token-value";
    let stored = client
        .put(format!(
            "{base}/api/v1/repos/cred-owner/vault/actions/secrets/DEPLOY_TOKEN"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"value": SECRET_VALUE}))
        .send()
        .await
        .expect("store a CI secret");
    assert_eq!(stored.status(), 201, "{}", stored.text().await.unwrap());

    for (method, url) in [
        ("token", format!("{base}/api/v1/users/tokens/{token_id}")),
        ("ssh key", format!("{base}/api/v1/users/ssh-keys/{key_id}")),
        (
            "ci secret",
            format!("{base}/api/v1/repos/cred-owner/vault/actions/secrets/DEPLOY_TOKEN"),
        ),
    ] {
        let revoked = client
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("revoke a credential");
        assert_eq!(revoked.status(), 204, "revoking the {method} failed");
    }

    let rows = journal(&base, &token).await;
    let entry = |action: &str| -> serde_json::Value {
        rows.iter()
            .find(|row| row["action"] == action)
            .unwrap_or_else(|| {
                panic!(
                    "no `{action}` row; the journal holds {:?}",
                    rows.iter()
                        .map(|row| row["action"].as_str().unwrap_or("?"))
                        .collect::<Vec<_>>()
                )
            })
            .clone()
    };
    let details = |action: &str| -> serde_json::Value {
        let row = entry(action);
        assert_actor_is(&row, "cred-owner", actor_id);
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("`{action}` recorded no details: {row}")),
        )
        .expect("details are JSON")
    };

    // The name and the scopes are what a review needs: they say what the
    // credential could reach, which "token #4" does not.
    let created = details("user.create_token");
    assert_eq!(created["token_name"], "ci-bot");
    assert_eq!(created["scopes"], "repo");
    let revoked = details("user.revoke_token");
    assert_eq!(revoked["token_name"], "ci-bot");

    // The title is chosen by whoever adds the key and identifies nothing; the
    // fingerprint is what the SSH server matches against.
    for action in ["user.add_ssh_key", "user.remove_ssh_key"] {
        let key = details(action);
        assert_eq!(key["title"], "laptop");
        assert_eq!(key["fingerprint"], fingerprint);
    }

    for action in ["repo.set_ci_secret", "repo.remove_ci_secret"] {
        let secret = details(action);
        assert_eq!(secret["secret"], "DEPLOY_TOKEN");
        assert_eq!(
            entry(action)["resource_name"],
            "cred-owner/vault",
            "a repository-scoped secret is journalled against the repository"
        );
    }

    // The half that makes the other half safe. Asserted over the whole journal
    // rather than over the six rows above, because a leak that appears in some
    // seventh entry is the same leak.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    for (what, secret) in [
        ("the raw personal access token", raw_token.as_str()),
        ("its stored hash", token_hash.as_str()),
        ("the CI secret value", SECRET_VALUE),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; the journal is read by operators and served over the              admin API, so a credential in it is a second credential store"
        );
    }
}

/// The password itself: the link that lets somebody back in, and the moment the
/// main secret of the account is replaced (card_80f1b25cf114).
///
/// Sharper than the credentials above for the same reason the second factor is.
/// A takeover left `user.login` on one side and `user.create_token` on the
/// other, and nothing in between — so the step that made both possible, "whoever
/// held the link from the mail replaced the password", was the one an incident
/// review had to infer. Two halves here, and the second is why the first is not
/// simply "always write a row": `POST /users/forgot-password` answers an address
/// nobody holds exactly as it answers one that exists, so a row written on the
/// silent branch would say out loud what the response refuses to.
#[tokio::test]
async fn a_password_reset_is_journalled_without_becoming_an_account_oracle() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "reset-owner", "reset-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let client = reqwest::Client::new();

    // The unknown address goes FIRST on purpose. Nothing can prove the absence
    // of a detached write by waiting; what can be proved is that this request
    // had at least as long as the one below, whose row is waited for.
    let unknown = client
        .post(format!("{base}/api/v1/users/forgot-password"))
        .json(&serde_json::json!({"email": "nobody@example.com"}))
        .send()
        .await
        .expect("ask for a reset of an address nobody holds");
    assert_eq!(unknown.status(), 200);

    let asked = client
        .post(format!("{base}/api/v1/users/forgot-password"))
        .json(&serde_json::json!({"email": "reset-owner@example.com"}))
        .send()
        .await
        .expect("ask for a reset");
    assert_eq!(asked.status(), 200);

    // The write is detached — it must not extend the response of the branch
    // that issues a token, or the endpoint's timing budget stops hiding which
    // branch ran — so the row is waited for rather than assumed to be there.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let requests = loop {
        let rows = journal(&base, &token).await;
        let requests: Vec<_> = rows
            .iter()
            .filter(|row| row["action"] == "user.request_password_reset")
            .cloned()
            .collect();
        if !requests.is_empty() {
            break requests;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no `user.request_password_reset` row appeared; the journal holds {:?}",
            rows.iter()
                .map(|row| row["action"].as_str().unwrap_or("?").to_owned())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "an address nobody holds got a row of its own, which answers `does this account exist` \
         more plainly than the endpoint ever could: {requests:?}"
    );
    assert_actor_is(&requests[0], "reset-owner", actor_id);

    // The link only ever leaves the server by mail, so the test plants its own
    // with the same hash the service stores.
    const RAW_RESET_TOKEN: &str = "reset-link-token-the-journal-must-never-hold";
    use sha2::Digest;
    rg_db::ops::password_reset_token_ops::create(
        &db,
        actor_id,
        &hex::encode(sha2::Sha256::digest(RAW_RESET_TOKEN.as_bytes())),
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .expect("plant a reset token");

    const NEW_PASSWORD: &str = "Rr1!replaced-by-the-link";
    let reset = client
        .post(format!("{base}/api/v1/users/reset-password"))
        .json(&serde_json::json!({"token": RAW_RESET_TOKEN, "new_password": NEW_PASSWORD}))
        .send()
        .await
        .expect("spend the reset link");
    let status = reset.status();
    let body = reset.text().await.expect("reset body");
    assert_eq!(status, 200, "{body}");
    // The reset ends every session minted against the password it replaced
    // (card_fcab45f42a02), including the one this test was reading the journal
    // with. The session it hands back is the same account, still an admin.
    let token: String = serde_json::from_str::<serde_json::Value>(&body)
        .expect("reset body is JSON")["token"]
        .as_str()
        .expect("the reset handed back a session")
        .to_owned();

    let replaced = one_entry(&base, &token, "user.reset_password").await;
    assert_actor_is(&replaced, "reset-owner", actor_id);
    let details: serde_json::Value = serde_json::from_str(
        replaced["details"]
            .as_str()
            .expect("`user.reset_password` recorded no details"),
    )
    .expect("details are JSON");
    assert_eq!(
        details["mfa_required"], false,
        "which way the reset ended is what a review asks next: this one handed back a session"
    );

    // The half that makes the other half safe, over the whole journal rather
    // than over the two rows above — a leak in some later entry is the same
    // leak. The reset link is the single copy of a way into the account.
    let whole =
        serde_json::to_string(&journal(&base, &token).await).expect("the journal serializes");
    for (what, secret) in [
        ("the reset link's token", RAW_RESET_TOKEN),
        ("the new password", NEW_PASSWORD),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; the journal is read by operators and served over the \
             admin API, so a credential in it is a second credential store"
        );
    }
}

/// The webhook's signing key as it is stored: AES-GCM ciphertext under the
/// instance key, which is the form a leak into the journal would take.
async fn stored_webhook_secret(db: &rg_db::DatabaseConnection, hook_id: i64) -> String {
    use sea_orm::EntityTrait;
    rg_db::entities::webhook::Entity::find_by_id(hook_id)
        .one(db)
        .await
        .expect("read the webhook row")
        .expect("the webhook still exists")
        .secret_encrypted
        .expect("the webhook was created with a signing key")
}

/// The secrets this server holds on somebody else's behalf: the key it signs
/// outgoing deliveries with, and the credential it pulls a mirror with
/// (card_c0a0339b7191).
///
/// Neither opens this instance, which is why they are not `high` — and why they
/// belong here anyway: the question the phase asks is which long-lived secrets
/// exist and when they appeared, and half of those are pointed outward. The
/// rotation is the sharp one. A receiver goes on verifying signatures either
/// way, so a webhook that starts being signed with a different key looks,
/// from outside, exactly like one whose description was edited — which is why
/// the entries below have to separate "the secret was replaced" from "something
/// else about this webhook changed".
#[tokio::test]
async fn an_outbound_secret_is_journalled_when_it_appears_changes_and_goes() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "hook-owner", "hook-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let repo_id = create_repo(&base, &token, "outbound").await;
    let client = reqwest::Client::new();
    let hooks = format!("{base}/api/v1/repos/hook-owner/outbound/hooks");
    let mirror = format!("{base}/api/v1/repos/hook-owner/outbound/mirror");

    const FIRST_SIGNING_KEY: &str = "hmac-key-the-journal-must-never-hold";
    const ROTATED_SIGNING_KEY: &str = "hmac-key-after-the-quiet-rotation";
    const MIRROR_PASSWORD: &str = "mirror-pull-token-that-belongs-to-somebody-else";

    let created = client
        .post(&hooks)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "https://receiver.example.com/hook",
            "secret": FIRST_SIGNING_KEY,
            "events": ["push"],
        }))
        .send()
        .await
        .expect("create a webhook");
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
    let created: serde_json::Value = created.json().await.expect("webhook body");
    let hook_id = created["id"].as_i64().expect("the webhook id");

    // Read while the rows still exist. The ciphertext is the secret to anyone
    // holding the instance at-rest key, so it is the one value whose presence
    // in the journal would be worse than the silence this card was about — and
    // after the removals below there is nothing left to compare against. It
    // also differs on every write of the same plaintext, which is why the two
    // webhook keys are read separately rather than derived.
    let first_ciphertext = stored_webhook_secret(&db, hook_id).await;

    // The rotation, with nothing else touched.
    let rotated = client
        .patch(format!("{hooks}/{hook_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "secret": ROTATED_SIGNING_KEY }))
        .send()
        .await
        .expect("rotate the signing key");
    assert_eq!(rotated.status(), 200, "{}", rotated.text().await.unwrap());

    let rotated_ciphertext = stored_webhook_secret(&db, hook_id).await;

    let stored = client
        .post(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "https://git.example.com/upstream/repo.git",
            "username": "puller",
            "password": MIRROR_PASSWORD,
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("configure a mirror");
    assert_eq!(stored.status(), 201, "{}", stored.text().await.unwrap());

    let mirror_ciphertext = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("read the mirror row")
        .expect("the mirror was just configured")
        .password_encrypted
        .expect("the mirror stores a credential");

    // An edit that leaves the credential alone — the other half of the pair the
    // entries have to tell apart.
    let retimed = client
        .patch(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "sync_interval_seconds": 7200 }))
        .send()
        .await
        .expect("change the sync interval");
    assert_eq!(retimed.status(), 200, "{}", retimed.text().await.unwrap());

    for (what, response) in [
        (
            "the mirror",
            client.delete(&mirror).bearer_auth(&token).send().await,
        ),
        (
            "the webhook",
            client
                .delete(format!("{hooks}/{hook_id}"))
                .bearer_auth(&token)
                .send()
                .await,
        ),
    ] {
        let response = response.unwrap_or_else(|error| panic!("remove {what}: {error}"));
        assert!(
            response.status().is_success(),
            "removing {what} failed: {}",
            response.text().await.unwrap()
        );
    }

    let rows = journal(&base, &token).await;
    let details = |action: &str| -> serde_json::Value {
        let row = rows
            .iter()
            .find(|row| row["action"] == action)
            .unwrap_or_else(|| {
                panic!(
                    "no `{action}` row; the journal holds {:?}",
                    rows.iter()
                        .map(|row| row["action"].as_str().unwrap_or("?"))
                        .collect::<Vec<_>>()
                )
            });
        assert_actor_is(row, "hook-owner", actor_id);
        assert_eq!(
            row["resource_name"], "hook-owner/outbound",
            "`{action}` is repository-scoped and the journal is read per repository"
        );
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("`{action}` recorded no details: {row}")),
        )
        .expect("details are JSON")
    };

    let born = details("repo.webhook_create");
    assert_eq!(born["webhook_url"], "https://receiver.example.com/hook");
    assert_eq!(born["has_secret"], true);

    let changed = details("repo.webhook_update");
    assert_eq!(
        changed["secret"], "replaced",
        "a rotation the journal cannot tell from an edit is the defect this row exists for"
    );

    let gone = details("repo.webhook_delete");
    assert_eq!(
        gone["webhook_url"], "https://receiver.example.com/hook",
        "read off the row before it went — `webhook #4 was removed` names nothing"
    );

    let pulled = details("repo.mirror_create");
    assert_eq!(
        pulled["source_url"],
        "https://git.example.com/upstream/repo.git"
    );
    assert_eq!(pulled["has_credential"], true);

    let edited = details("repo.mirror_update");
    assert_eq!(
        edited["credential"], "unchanged",
        "an edit that left the credential alone must not read as a rotation"
    );
    assert_eq!(edited["has_credential"], true);

    let dropped = details("repo.mirror_delete");
    assert_eq!(dropped["has_credential"], true);

    // Over the whole journal, because a leak in some later entry is the same
    // leak — and over the ciphertexts as well as the plaintexts, because a row
    // carrying `secret_encrypted` leaks to exactly the reader who can act on it.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    for (what, secret) in [
        ("the webhook's first signing key", FIRST_SIGNING_KEY),
        ("the key it was rotated to", ROTATED_SIGNING_KEY),
        ("the mirror's stored credential", MIRROR_PASSWORD),
        (
            "the first signing key's ciphertext",
            first_ciphertext.as_str(),
        ),
        ("the rotated key's ciphertext", rotated_ciphertext.as_str()),
        (
            "the mirror credential's ciphertext",
            mirror_ciphertext.as_str(),
        ),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; the journal is read by operators and served over the \
             admin API, so a credential in it is a second credential store"
        );
    }
}

/// The authenticator's side of the TOTP handshake, for the current step.
fn current_totp_code(secret: &str) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.to_string())
        .to_bytes()
        .expect("the secret the server handed out is not base32");
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        None,
        String::new(),
    )
    .expect("build the authenticator side of the handshake")
    .generate_current()
    .expect("read the current time step")
}

/// The second factor: armed, re-issued, dropped — and a passkey removed — with
/// none of the material in the journal (card_7aa2870dc1e0).
///
/// Sharper than the credentials above, and for the opposite reason. A token or
/// an SSH key GRANTS access; the second factor is what access is PROTECTED by,
/// so the interesting row is the one that takes it away. The classic takeover
/// runs stolen password → `disable_mfa` → everything else, and the journal used
/// to hold the login and then nothing until the damage.
///
/// The leak half is harder here than anywhere else in this file: the TOTP
/// secret is the factor itself, and every backup code is a single-use password
/// for the account. A hash is no better than the code — the server verifies
/// them one at a time, so a column of hashes is a dictionary — which is why the
/// assertion below searches the whole journal for the codes themselves.
///
/// What this cannot drive: `POST /users/passkeys/register/finish` needs a
/// verified attestation, and no soft authenticator is a dependency of this
/// workspace (see `passkey_ceremony_single_use_tests`). Its journal call is
/// held by `credential_audit_guard` instead, which reads the handler's source —
/// and that guard is mutation-proven on both halves. The removal below is
/// driven for real, against a row seeded the way the ceremony would leave it.
#[tokio::test]
async fn second_factor_events_are_journalled_and_the_factor_itself_is_not() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "mfa-owner", "mfa-owner@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let client = reqwest::Client::new();

    // 1. Enrol. `setup` hands back the TOTP secret in the same base32 the
    //    authenticator app would scan, and it is the one value that must never
    //    reappear in a journal entry.
    let setup: serde_json::Value = client
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("start MFA setup")
        .json()
        .await
        .expect("setup body");
    let totp_secret = setup["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_owned();

    let enabled = client
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "code": current_totp_code(&totp_secret) }))
        .send()
        .await
        .expect("enable MFA");
    assert_eq!(enabled.status(), 200, "{}", enabled.text().await.unwrap());
    let enabled: serde_json::Value = enabled.json().await.expect("enable body");
    let first_codes: Vec<String> = enabled["backup_codes"]
        .as_array()
        .expect("enrolment issued no backup codes")
        .iter()
        .map(|code| code.as_str().expect("a backup code is a string").to_owned())
        .collect();
    assert!(!first_codes.is_empty());

    // 2. Re-issue. What makes this an event is that every code from step 1
    //    stopped working, which no row in `mfa_backup_codes` records.
    let reissued = client
        .post(format!("{base}/api/v1/users/mfa/backup/regenerate"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "password": "Qz7$wRtm" }))
        .send()
        .await
        .expect("re-issue backup codes");
    assert_eq!(reissued.status(), 200, "{}", reissued.text().await.unwrap());
    let reissued: serde_json::Value = reissued.json().await.expect("re-issue body");
    let second_codes: Vec<String> = reissued["backup_codes"]
        .as_array()
        .expect("the re-issue returned no codes")
        .iter()
        .map(|code| code.as_str().expect("a backup code is a string").to_owned())
        .collect();

    // 3. A passkey, seeded the way a completed ceremony leaves it, then removed
    //    over the real endpoint.
    const CREDENTIAL_ID: &str = "AAECAwQFBgcICQoLDA0ODxAREhM";
    let passkey = rg_db::ops::passkey_credential_ops::create(
        &db,
        actor_id,
        CREDENTIAL_ID,
        r#"{"cred":"opaque"}"#,
        "laptop",
        "localhost",
    )
    .await
    .expect("seed a registered passkey");

    let removed = client
        .delete(format!("{base}/api/v1/users/passkeys/{}", passkey.id))
        .bearer_auth(&token)
        .send()
        .await
        .expect("remove the passkey");
    assert_eq!(removed.status(), 204, "{}", removed.text().await.unwrap());

    // 4. Drop the factor. The row this whole test exists for.
    let disabled = client
        .post(format!("{base}/api/v1/users/mfa/disable"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "password": "Qz7$wRtm" }))
        .send()
        .await
        .expect("disable MFA");
    assert_eq!(disabled.status(), 200, "{}", disabled.text().await.unwrap());

    let rows = journal(&base, &token).await;
    let details = |action: &str| -> serde_json::Value {
        let row = rows
            .iter()
            .find(|row| row["action"] == action)
            .unwrap_or_else(|| {
                panic!(
                    "no `{action}` row; the journal holds {:?}",
                    rows.iter()
                        .map(|row| row["action"].as_str().unwrap_or("?"))
                        .collect::<Vec<_>>()
                )
            })
            .clone();
        assert_actor_is(&row, "mfa-owner", actor_id);
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("`{action}` recorded no details: {row}")),
        )
        .expect("details are JSON")
    };

    let armed = details("user.enable_mfa");
    assert_eq!(armed["method"], "totp");
    assert_eq!(
        armed["backup_codes_issued"],
        first_codes.len(),
        "the count is what tells a later backup-code login apart from a spent set"
    );

    let rotated = details("user.regenerate_backup_codes");
    assert_eq!(rotated["backup_codes_issued"], second_codes.len());

    assert_eq!(details("user.disable_mfa")["method"], "totp");

    // The removal names the authenticator, not just its id: after the delete
    // the row is gone, and `#4` answers nobody's question about which key
    // stopped working.
    let dropped = details("user.remove_passkey");
    assert_eq!(dropped["name"], "laptop");
    assert_eq!(dropped["credential_id_prefix"], &CREDENTIAL_ID[..12]);
    assert_ne!(
        dropped["credential_id_prefix"], CREDENTIAL_ID,
        "the entry carries the head of the credential id, not the whole thing"
    );

    // The half that makes the other half safe, over the whole journal rather
    // than the four rows above: a leak in some fifth entry is the same leak.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    assert!(
        !whole.contains(&totp_secret),
        "the TOTP secret reached `audit_log`; it IS the second factor, and the journal is served \
         over the admin API"
    );
    for code in first_codes.iter().chain(second_codes.iter()) {
        assert!(
            !whole.contains(code),
            "a backup code reached `audit_log`; each one is a single-use password for this account"
        );
    }
}

/// The runner token: issued, used to deregister, and revoked — with the token
/// itself nowhere in the journal (card_2e514de7eefa).
///
/// The widest long-lived secret this instance issues, and the one that had no
/// journal at all. A runner polls the queue, takes a job from any repository
/// whose labels it covers, and `poll_job` decrypts that repository's CI secrets
/// into the job's environment — so this credential is read access to the
/// secrets of every repository whose work the machine can claim. `register`
/// returns the token once and stores only its hash, which is what makes the
/// leak assertion below the load-bearing half of the requirement.
#[tokio::test]
async fn runner_token_events_are_journalled_and_the_token_itself_is_not() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "runner-admin", "runner-admin@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let client = reqwest::Client::new();

    let issue = |name: &'static str| {
        let client = client.clone();
        let base = base.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/runners/register"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "name": name,
                    "labels": ["linux", "docker"],
                    "version": "1.2.3",
                    "os": "linux",
                    "arch": "x86_64",
                }))
                .send()
                .await
                .expect("register a runner");
            let status = response.status();
            let body: serde_json::Value = response.json().await.expect("registration body");
            assert_eq!(status, 201, "registering a runner failed: {body}");
            (
                body["id"].as_i64().expect("the runner id"),
                body["token"].as_str().expect("the runner token").to_owned(),
            )
        }
    };

    let (revoked_id, revoked_token) = issue("build-box").await;
    let (self_removed_id, self_removed_token) = issue("scratch-box").await;

    // The admin's revocation.
    let deleted = client
        .delete(format!("{base}/api/v1/admin/runners/{revoked_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("revoke the runner");
    assert_eq!(deleted.status(), 204, "{}", deleted.text().await.unwrap());

    // And the runner's own orderly exit, which revokes the same credential
    // through a different door — authenticated by the runner token itself, so
    // no account performed it.
    let left = client
        .post(format!(
            "{base}/api/v1/runners/{self_removed_id}/deregister"
        ))
        .bearer_auth(&self_removed_token)
        .send()
        .await
        .expect("deregister the runner");
    assert_eq!(left.status(), 200, "{}", left.text().await.unwrap());

    let rows = journal(&base, &token).await;
    let details_of = |action: &str, resource_name: &str| -> serde_json::Value {
        let row = rows
            .iter()
            .find(|row| row["action"] == action && row["resource_name"] == resource_name)
            .unwrap_or_else(|| {
                panic!(
                    "no `{action}` row for `{resource_name}`; the journal holds {:?}",
                    rows.iter()
                        .map(|row| row["action"].as_str().unwrap_or("?"))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            row["resource_type"], "runner",
            "a runner token belongs to the instance, not to an account or a repository: {row}"
        );
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("`{action}` recorded no details: {row}")),
        )
        .expect("details are JSON")
    };

    // The labels are what makes the entry worth reading: they decide whose jobs
    // this machine may take, and therefore whose secrets it is handed.
    for (action, name) in [
        ("admin.register_runner", "build-box"),
        ("admin.delete_runner", "build-box"),
        ("runner.deregister", "scratch-box"),
    ] {
        let details = details_of(action, name);
        assert_eq!(details["name"], name);
        assert_eq!(
            details["labels"],
            serde_json::json!(["linux", "docker"]),
            "`{action}` must say which work this machine could take"
        );
        assert_eq!(details["os"], "linux");
        assert_eq!(details["arch"], "x86_64");
    }

    // The admin's two entries name the admin; the runner's own exit names
    // nobody, because nobody performed it — a blank actor is the honest answer
    // there, and naming whoever registered the machine months ago would not be.
    let row_of = |action: &str, resource_name: &str| -> serde_json::Value {
        rows.iter()
            .find(|row| row["action"] == action && row["resource_name"] == resource_name)
            .unwrap_or_else(|| panic!("no `{action}` row for `{resource_name}`"))
            .clone()
    };
    for (action, name) in [
        ("admin.register_runner", "build-box"),
        ("admin.register_runner", "scratch-box"),
        ("admin.delete_runner", "build-box"),
    ] {
        assert_actor_is(&row_of(action, name), "runner-admin", actor_id);
    }
    let self_exit = row_of("runner.deregister", "scratch-box");
    assert!(
        self_exit["username"].is_null() && self_exit["user_id"].is_null(),
        "the runner authenticated as itself, so no account may be named: {self_exit}"
    );

    // The half that makes the other half safe, asserted over the whole journal:
    // a token that surfaces in some other entry is the same leak.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    for (what, secret) in [
        ("the revoked runner's token", revoked_token.as_str()),
        (
            "the self-removed runner's token",
            self_removed_token.as_str(),
        ),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; `register` hands out the only copy there is, and a \
             journal served over the admin API must not become the second one"
        );
    }
}

/// The provider's client secret as it is stored: AES-GCM ciphertext under the
/// instance key, which is the form a leak into the journal would take.
async fn stored_sso_client_secret(db: &rg_db::DatabaseConnection, provider_id: i64) -> String {
    use sea_orm::EntityTrait;
    rg_db::entities::sso_provider::Entity::find_by_id(provider_id)
        .one(db)
        .await
        .expect("read the SSO provider row")
        .expect("the provider still exists")
        .client_secret_enc
        .expect("the provider was created with a client secret")
}

/// The same for the LDAP bind password — a read account in somebody else's
/// directory, and the one secret here that is not even about this instance.
async fn stored_sso_ldap_password(db: &rg_db::DatabaseConnection, provider_id: i64) -> String {
    use sea_orm::EntityTrait;
    rg_db::entities::sso_provider::Entity::find_by_id(provider_id)
        .one(db)
        .await
        .expect("read the SSO provider row")
        .expect("the provider still exists")
        .ldap_bind_password_enc
        .expect("the provider was created with a bind password")
}

/// The widest long-lived secret on the instance: the door every account logs in
/// through (card_d03f5f4b6fc2).
///
/// Wider than the runner token beside it. Whoever holds an SSO provider's
/// `client_secret` holds the way in for *everyone*, and `ldap_bind_password` is
/// a service account in a directory this instance does not even own — and the
/// three admin endpoints that mint, replace and destroy them wrote nothing.
/// The secrets were already judged valuable enough to be encrypted at rest;
/// their appearance was not an event.
///
/// The leak assertion at the bottom is the other half of the same requirement,
/// and it is the half a coverage assertion cannot stand in for: an entry that
/// carried the secret would be strictly worse than the silence.
#[tokio::test]
async fn sso_provider_secret_events_are_journalled_and_the_secrets_are_not() {
    const CLIENT_SECRET: &str = "oidc-client-secret-as-first-stored";
    const ROTATED_CLIENT_SECRET: &str = "oidc-client-secret-after-rotation";
    const LDAP_BIND_PASSWORD: &str = "ldap-service-account-password";

    let (base, db) = spawn_test_app_with_db().await;
    let (token, actor_id) = register_full(&base, "sso-admin", "sso-admin@example.com").await;
    promote_user_to_admin(&db, actor_id).await;
    let client = reqwest::Client::new();

    let create = |body: serde_json::Value| {
        let client = client.clone();
        let base = base.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/admin/sso/providers"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .expect("create an SSO provider");
            let status = response.status();
            let text = response.text().await.expect("create body");
            assert_eq!(status, 201, "creating the provider failed: {text}");
            serde_json::from_str::<serde_json::Value>(&text).expect("create body is JSON")
        }
    };

    let oidc = create(serde_json::json!({
        "name": "Corporate Login",
        "slug": "corp-login",
        "provider_type": "oidc",
        "enabled": true,
        "client_id": "corp-client",
        "client_secret": CLIENT_SECRET,
        "discovery_url": "https://idp.example.com/.well-known/openid-configuration",
    }))
    .await;
    let oidc_id = oidc["id"].as_i64().expect("the new provider has an id");

    let ldap = create(serde_json::json!({
        "name": "Directory",
        "slug": "corp-directory",
        "provider_type": "ldap",
        "enabled": true,
        "ldap_host": "ldap.example.com",
        "ldap_port": 636,
        "ldap_bind_dn": "cn=forgekeep,ou=services,dc=example,dc=com",
        "ldap_bind_password": LDAP_BIND_PASSWORD,
        "ldap_base_dn": "ou=people,dc=example,dc=com",
    }))
    .await;
    let ldap_id = ldap["id"].as_i64().expect("the new provider has an id");

    let patch = |id: i64, body: serde_json::Value| {
        let client = client.clone();
        let base = base.clone();
        let token = token.clone();
        async move {
            let response = client
                .patch(format!("{base}/api/v1/admin/sso/providers/{id}"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .expect("update an SSO provider");
            let status = response.status();
            let text = response.text().await.expect("update body");
            assert_eq!(status, 200, "updating the provider failed: {text}");
        }
    };

    // The rotation: the door is re-keyed, and from outside nothing about the
    // provider looks different.
    patch(
        oidc_id,
        serde_json::json!({
            "name": "Corporate Login",
            "slug": "corp-login",
            "provider_type": "oidc",
            "enabled": true,
            "client_id": "corp-client",
            "client_secret": ROTATED_CLIENT_SECRET,
            "discovery_url": "https://idp.example.com/.well-known/openid-configuration",
        }),
    )
    .await;

    // And an edit that leaves the secret alone, so the row above has something
    // to be told apart from.
    patch(
        oidc_id,
        serde_json::json!({
            "name": "Corporate Login (renamed)",
            "slug": "corp-login",
            "provider_type": "oidc",
            "enabled": true,
            "client_id": "corp-client",
            "discovery_url": "https://idp.example.com/.well-known/openid-configuration",
        }),
    )
    .await;

    // Read while the rows are alive: after the deletes below there is nothing
    // left to compare the journal against.
    let oidc_ciphertext = stored_sso_client_secret(&db, oidc_id).await;
    let ldap_ciphertext = stored_sso_ldap_password(&db, ldap_id).await;

    for id in [oidc_id, ldap_id] {
        let response = client
            .delete(format!("{base}/api/v1/admin/sso/providers/{id}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete an SSO provider");
        let status = response.status();
        let text = response.text().await.expect("delete body");
        assert_eq!(status, 200, "deleting the provider failed: {text}");
    }

    let rows = journal(&base, &token).await;
    let entries = |action: &str| -> Vec<serde_json::Value> {
        rows.iter()
            .filter(|row| row["action"] == action)
            .cloned()
            .collect()
    };
    let details = |row: &serde_json::Value| -> serde_json::Value {
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("no details on {row}")),
        )
        .expect("details are JSON")
    };
    let for_slug = |action: &str, slug: &str| -> serde_json::Value {
        entries(action)
            .into_iter()
            .find(|row| row["resource_name"] == slug)
            .unwrap_or_else(|| {
                panic!(
                    "no `{action}` row for `{slug}`; the journal holds {:?}",
                    rows.iter()
                        .map(|row| row["action"].as_str().unwrap_or("?"))
                        .collect::<Vec<_>>()
                )
            })
    };

    let born = for_slug("admin.create_sso_provider", "corp-login");
    assert_actor_is(&born, "sso-admin", actor_id);
    assert_eq!(
        born["resource_type"], "sso_provider",
        "the login door belongs to the instance, not to the admin who wired it"
    );
    let born_details = details(&born);
    assert_eq!(born_details["provider_type"], "oidc");
    assert_eq!(born_details["enabled"], true);
    assert_eq!(born_details["has_client_secret"], true);

    let directory = details(&for_slug("admin.create_sso_provider", "corp-directory"));
    assert_eq!(directory["has_ldap_bind_password"], true);

    // Newest first, so the rename is the row the rotation has to be told from.
    let updates = entries("admin.update_sso_provider");
    assert_eq!(
        updates.len(),
        2,
        "both updates must be journalled, not only the last one"
    );
    let renamed = details(&updates[0]);
    let rotated = details(&updates[1]);
    assert_eq!(renamed["name"], "Corporate Login (renamed)");
    assert_eq!(
        renamed["client_secret"], "unchanged",
        "an edit that left the secret alone must not read as a rotation"
    );
    assert_eq!(
        rotated["client_secret"], "replaced",
        "a rotation the journal cannot tell from an edit is the defect this row exists for"
    );
    assert_eq!(rotated["has_client_secret"], true);

    let gone = details(&for_slug("admin.delete_sso_provider", "corp-login"));
    assert_eq!(
        gone["provider_type"], "oidc",
        "read off the row before it went — `sso_provider #4 was removed` names nothing"
    );
    assert_eq!(gone["has_client_secret"], true);
    let directory_gone = details(&for_slug("admin.delete_sso_provider", "corp-directory"));
    assert_eq!(directory_gone["has_ldap_bind_password"], true);

    // Over the whole journal and over the ciphertexts as well as the
    // plaintexts: a row carrying `client_secret_enc` leaks to exactly the
    // reader who holds the instance key and can act on it.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    for (what, secret) in [
        ("the provider's first client secret", CLIENT_SECRET),
        ("the secret it was rotated to", ROTATED_CLIENT_SECRET),
        ("the LDAP bind password", LDAP_BIND_PASSWORD),
        ("the client secret's ciphertext", oidc_ciphertext.as_str()),
        ("the bind password's ciphertext", ldap_ciphertext.as_str()),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; the journal is read by operators and served over the \
             admin API, so a credential in it is a second credential store"
        );
    }
}
