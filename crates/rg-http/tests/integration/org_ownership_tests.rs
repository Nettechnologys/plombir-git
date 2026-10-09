//! Security audit finding #5: rights over an organization and its repositories
//! come from membership *roles*, never from `organizations.owner_id` or
//! `repositories.owner_id`.
//!
//! Before: an organization's repositories were stored with
//! `owner_id = org.owner_id`, and `can_read_repo` / `can_write_repo` /
//! `can_admin_repo` / `require_owner` all short-circuited on
//! `repo.owner_id == actor`, while `is_org_admin` and `require_org_visible`
//! short-circuited on `org.owner_id`. So the creator kept every right after
//! being removed from the organization — read, write, admin, delete, transfer,
//! the private repository in listings — and the members actually holding the
//! `owner` role got `403` on delete and transfer, because they are not the row's
//! `owner_id`. There was no way to hand an organization over, and the last
//! owner could be removed, leaving an organization nobody could administer.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::entities::{repo_collaborator, team_member};

async fn create_org(base: &str, token: &str, name: &str, visibility: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "visibility": visibility}))
        .send()
        .await
        .expect("create org");
    assert_eq!(resp.status(), 201, "baseline: create_org({name})");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("org id")
}

async fn create_org_repo(base: &str, token: &str, org: &str, name: &str, private: bool) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "org": org, "is_private": private}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "baseline: create repo {org}/{name}");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("repo id")
}

async fn add_member(base: &str, token: &str, org: &str, user_id: i64, role: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/members"))
        .bearer_auth(token)
        .json(&serde_json::json!({"user_id": user_id, "role": role}))
        .send()
        .await
        .expect("add member")
        .status();
    assert_eq!(status, 201, "baseline: add {role} {user_id} to {org}");
}

async fn remove_member(base: &str, token: &str, org: &str, user_id: i64) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!("{base}/api/v1/orgs/{org}/members/{user_id}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("remove member")
}

async fn transfer_ownership(
    base: &str,
    token: Option<&str>,
    org: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/transfer-ownership"))
        .json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send().await.expect("transfer ownership")
}

async fn status(req: reqwest::RequestBuilder) -> u16 {
    req.send().await.expect("request").status().as_u16()
}

/// The names `GET /repos/{org}` shows `token`.
async fn listed_repos(base: &str, token: &str, org: &str) -> Vec<String> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{org}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("list repos")
        .json()
        .await
        .expect("json");
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|repo| repo["name"].as_str().map(str::to_string))
        .collect()
}

/// The reported hole, end to end: once removed from the organization, the
/// creator is a stranger to its private repository on every level — and does
/// not see it listed.
#[tokio::test]
async fn a_removed_creator_is_a_stranger_to_the_organizations_private_repository() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (creator, creator_id) =
        register_full(&base, "own5-creator", "own5-creator@example.com").await;
    let (successor, successor_id) =
        register_full(&base, "own5-successor", "own5-successor@example.com").await;
    create_org(&base, &creator, "own5corp", "private").await;
    create_org_repo(&base, &creator, "own5corp", "secret", true).await;
    add_member(&base, &creator, "own5corp", successor_id, "owner").await;

    // Baseline, before the removal: the creator can do everything.
    let client = reqwest::Client::new();
    let repo = format!("{base}/api/v1/repos/own5corp/secret");
    assert_eq!(status(client.get(&repo).bearer_auth(&creator)).await, 200);
    assert_eq!(
        listed_repos(&base, &creator, "own5corp").await,
        vec!["secret"]
    );

    let removed = remove_member(&base, &successor, "own5corp", creator_id).await;
    assert_eq!(removed.status(), 200, "the other owner removes the creator");

    // Read (RepoRead): a private repository the caller may not read.
    let read = status(client.get(&repo).bearer_auth(&creator)).await;
    assert!(
        matches!(read, 403 | 404),
        "the removed creator still reads the private repository: {read}"
    );
    // Write (RepoWrite).
    assert_eq!(
        status(
            client
                .post(format!("{repo}/labels"))
                .bearer_auth(&creator)
                .json(&serde_json::json!({"name": "intruder", "color": "#ff0000"}))
        )
        .await,
        403,
        "the removed creator still writes to the repository"
    );
    // Admin (RepoAdmin).
    assert_eq!(
        status(
            client
                .post(format!("{repo}/collaborators"))
                .bearer_auth(&creator)
                .json(&serde_json::json!({"user_id": creator_id, "permission": "admin"}))
        )
        .await,
        403,
        "the removed creator still administers the repository"
    );
    // Owner (RepoOwner): transfer, then delete.
    assert_eq!(
        status(
            client
                .post(format!("{repo}/transfer"))
                .bearer_auth(&creator)
                .json(&serde_json::json!({"new_owner": "own5-creator"}))
        )
        .await,
        403,
        "the removed creator still takes the repository with them"
    );
    assert_eq!(
        status(client.delete(&repo).bearer_auth(&creator)).await,
        403,
        "the removed creator still deletes the repository"
    );
    // Listings and the organization itself.
    assert!(
        listed_repos(&base, &creator, "own5corp").await.is_empty(),
        "the private repository is still listed to the removed creator"
    );
    assert_eq!(
        status(
            client
                .get(format!("{base}/api/v1/orgs/own5corp"))
                .bearer_auth(&creator)
        )
        .await,
        404,
        "a private organization must mask itself from the removed creator"
    );
    assert_eq!(
        status(
            client
                .delete(format!("{base}/api/v1/orgs/own5corp"))
                .bearer_auth(&creator)
        )
        .await,
        404,
        "the removed creator can still delete the organization"
    );

    // And the repository is still there for the people who own it.
    assert_eq!(status(client.get(&repo).bearer_auth(&successor)).await, 200);
}

/// The other half of the finding: a member holding the `owner` role owns the
/// organization's repositories — delete and transfer included — while an
/// `admin`-role member administers them without owning them.
#[tokio::test]
async fn owner_role_members_own_the_repositories_admins_do_not() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (creator, _) = register_full(&base, "own5b-creator", "own5b-creator@example.com").await;
    let (owner, owner_id) = register_full(&base, "own5b-owner", "own5b-owner@example.com").await;
    let (admin, admin_id) = register_full(&base, "own5b-admin", "own5b-admin@example.com").await;
    create_org(&base, &creator, "own5bcorp", "public").await;
    create_org_repo(&base, &creator, "own5bcorp", "doomed", false).await;
    create_org_repo(&base, &creator, "own5bcorp", "moving", false).await;
    add_member(&base, &creator, "own5bcorp", owner_id, "owner").await;
    add_member(&base, &creator, "own5bcorp", admin_id, "admin").await;

    let client = reqwest::Client::new();
    // The admin administers (settings) but does not own (delete / transfer).
    assert_eq!(
        status(
            client
                .patch(format!("{base}/api/v1/repos/own5bcorp/doomed"))
                .bearer_auth(&admin)
                .json(&serde_json::json!({"description": "run by an admin"}))
        )
        .await,
        200,
        "an org admin administers the repository"
    );
    assert_eq!(
        status(
            client
                .delete(format!("{base}/api/v1/repos/own5bcorp/doomed"))
                .bearer_auth(&admin)
        )
        .await,
        403,
        "an org admin must not delete the organization's repository"
    );
    assert_eq!(
        status(
            client
                .post(format!("{base}/api/v1/repos/own5bcorp/moving/transfer"))
                .bearer_auth(&admin)
                .json(&serde_json::json!({"new_owner": "own5b-admin"}))
        )
        .await,
        403,
        "an org admin must not take the organization's repository"
    );

    // The owner-role member — not the row's `owner_id` — does both.
    assert_eq!(
        status(
            client
                .post(format!("{base}/api/v1/repos/own5bcorp/moving/transfer"))
                .bearer_auth(&owner)
                .json(&serde_json::json!({"new_owner": "own5b-owner"}))
        )
        .await,
        200,
        "an owner-role member was refused the transfer"
    );
    assert_eq!(
        status(
            client
                .get(format!("{base}/api/v1/repos/own5b-owner/moving"))
                .bearer_auth(&owner)
        )
        .await,
        200,
        "the transfer did not move the repository"
    );
    assert_eq!(
        status(
            client
                .delete(format!("{base}/api/v1/repos/own5bcorp/doomed"))
                .bearer_auth(&owner)
        )
        .await,
        200,
        "an owner-role member was refused the delete"
    );
}

/// An organization always keeps at least one owner: the last one cannot be
/// removed — by anybody, themselves included — until another member holds the
/// role.
#[tokio::test]
async fn the_last_owner_cannot_be_removed() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (creator, creator_id) =
        register_full(&base, "own5c-creator", "own5c-creator@example.com").await;
    let (admin, admin_id) = register_full(&base, "own5c-admin", "own5c-admin@example.com").await;
    create_org(&base, &creator, "own5ccorp", "public").await;
    add_member(&base, &creator, "own5ccorp", admin_id, "admin").await;

    for (label, token) in [("the owner themselves", &creator), ("an admin", &admin)] {
        let refused = remove_member(&base, token, "own5ccorp", creator_id).await;
        assert_eq!(
            refused.status(),
            409,
            "{label} removed the organization's last owner"
        );
        let body = refused.text().await.expect("body");
        assert!(
            body.contains("last owner"),
            "the refusal must say why, got: {body}"
        );
    }

    // The way out: a second owner. Then the first one may go.
    let resp = transfer_ownership(
        &base,
        Some(&creator),
        "own5ccorp",
        serde_json::json!({"username": "own5c-admin"}),
    )
    .await;
    assert_eq!(resp.status(), 200, "transfer to the admin");
    let removed = remove_member(&base, &admin, "own5ccorp", creator_id).await;
    assert_eq!(
        removed.status(),
        200,
        "with a second owner in place the first may be removed"
    );
}

#[tokio::test]
async fn ownership_transfer_hands_over_the_role_the_column_and_the_repositories() {
    let (base, db) = spawn_test_app_with_db().await;
    let (creator, creator_id) =
        register_full(&base, "own5d-creator", "own5d-creator@example.com").await;
    let (member, member_id) =
        register_full(&base, "own5d-member", "own5d-member@example.com").await;
    let (admin, admin_id) = register_full(&base, "own5d-admin", "own5d-admin@example.com").await;
    register_full(&base, "own5d-outsider", "own5d-out@example.com").await;
    let org_id = create_org(&base, &creator, "own5dcorp", "public").await;
    let repo_id = create_org_repo(&base, &creator, "own5dcorp", "ledger", true).await;
    add_member(&base, &creator, "own5dcorp", member_id, "member").await;
    add_member(&base, &creator, "own5dcorp", admin_id, "admin").await;

    // Refusals first, while nothing has moved.
    assert_eq!(
        transfer_ownership(
            &base,
            None,
            "own5dcorp",
            serde_json::json!({"username": "own5d-member"})
        )
        .await
        .status(),
        401,
        "anonymous"
    );
    assert_eq!(
        transfer_ownership(
            &base,
            Some(&admin),
            "own5dcorp",
            serde_json::json!({"username": "own5d-member"})
        )
        .await
        .status(),
        403,
        "an admin-role member is not an owner"
    );
    assert_eq!(
        transfer_ownership(
            &base,
            Some(&creator),
            "own5dcorp",
            serde_json::json!({"username": "own5d-outsider"})
        )
        .await
        .status(),
        409,
        "the target must already be a member"
    );
    assert_eq!(
        transfer_ownership(
            &base,
            Some(&creator),
            "own5dcorp",
            serde_json::json!({"username": "own5d-nobody"})
        )
        .await
        .status(),
        400,
        "an account that does not exist"
    );
    assert_eq!(
        transfer_ownership(
            &base,
            Some(&creator),
            "own5dcorp",
            serde_json::json!({"username": "own5d-creator"})
        )
        .await
        .status(),
        409,
        "the current owner of record"
    );

    // The happy path.
    let resp = transfer_ownership(
        &base,
        Some(&creator),
        "own5dcorp",
        serde_json::json!({"username": "own5d-member"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let org: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(org["owner_id"], serde_json::json!(member_id));

    // The role moved with the column, and the repository row followed.
    let members: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{base}/api/v1/orgs/own5dcorp/members"))
        .bearer_auth(&member)
        .send()
        .await
        .expect("members")
        .json()
        .await
        .expect("json");
    let role_of = |id: i64| {
        members
            .iter()
            .find(|m| m["user_id"] == serde_json::json!(id))
            .map(|m| m["role"].as_str().unwrap_or_default().to_string())
    };
    assert_eq!(role_of(member_id).as_deref(), Some("owner"));
    assert_eq!(
        role_of(creator_id).as_deref(),
        Some("owner"),
        "the previous owner keeps their role; demoting is a separate decision"
    );
    let repo = rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("read repo")
        .expect("repo exists");
    assert_eq!(
        repo.owner_id, member_id,
        "the organization's repository must mirror the new owner, or deleting the previous \
         owner's account cascades onto it"
    );

    // The journal has it.
    let entries = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("org.transfer_ownership"))
        .all(&db)
        .await
        .expect("read audit log");
    assert_eq!(entries.len(), 1, "exactly one transfer was journalled");
    assert_eq!(entries[0].resource_id, Some(org_id));
    assert_eq!(entries[0].user_id, Some(creator_id));
    let details: serde_json::Value =
        serde_json::from_str(entries[0].details.as_deref().unwrap_or("{}")).expect("details");
    assert_eq!(details["new_owner_id"], serde_json::json!(member_id));
    assert_eq!(details["previous_owner_id"], serde_json::json!(creator_id));

    // The new owner now disposes of the organization's repository, and can show
    // the creator the door.
    assert_eq!(
        status(
            reqwest::Client::new()
                .delete(format!("{base}/api/v1/repos/own5dcorp/ledger"))
                .bearer_auth(&member)
        )
        .await,
        200
    );
    assert_eq!(
        remove_member(&base, &member, "own5dcorp", creator_id)
            .await
            .status(),
        200
    );
}

/// Like [`add_member`], but hands the response back so a refusal can be
/// asserted (and its status read) instead of panicking.
async fn add_member_response(
    base: &str,
    token: &str,
    org: &str,
    user_id: i64,
    role: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/members"))
        .bearer_auth(token)
        .json(&serde_json::json!({"user_id": user_id, "role": role}))
        .send()
        .await
        .expect("add member")
}

/// The escalation the first fix left open: `POST /orgs/{name}/members` was
/// gated `OrgAdmin` and passed the body's `role` straight to the insert. An
/// `admin`-role member could therefore add a confederate — or a second account —
/// with `role: "owner"`, and owners are exactly whom `OrgOwner` admits to
/// `DELETE /orgs/{name}` and `POST /orgs/{name}/transfer-ownership`. Membership
/// administration had become ownership capture.
#[tokio::test]
async fn only_an_owner_may_hand_out_the_owner_role() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (creator, _creator_id) =
        register_full(&base, "own5e-creator", "own5e-creator@example.com").await;
    let (admin, admin_id) = register_full(&base, "own5e-admin", "own5e-admin@example.com").await;
    let (_target_admin, target_admin_id) =
        register_full(&base, "own5e-tadmin", "own5e-tadmin@example.com").await;
    let (target_owner, target_owner_id) =
        register_full(&base, "own5e-towner", "own5e-towner@example.com").await;
    let (_unknown, unknown_id) =
        register_full(&base, "own5e-unknown", "own5e-unknown@example.com").await;
    create_org(&base, &creator, "own5ecorp", "public").await;
    add_member(&base, &creator, "own5ecorp", admin_id, "admin").await;

    let client = reqwest::Client::new();
    let org_url = format!("{base}/api/v1/orgs/own5ecorp");

    // An admin still administers membership: `member` and `admin` are theirs.
    assert_eq!(
        add_member_response(&base, &admin, "own5ecorp", target_admin_id, "admin")
            .await
            .status(),
        201,
        "an admin may grant the admin role"
    );
    // `owner` is not theirs to give.
    assert_eq!(
        add_member_response(&base, &admin, "own5ecorp", target_owner_id, "owner")
            .await
            .status(),
        403,
        "an admin granted the owner role"
    );
    // The refusal stored nothing: the target is still no one in this
    // organization, and can neither see it as an admin nor delete it.
    assert_eq!(
        status(client.delete(&org_url).bearer_auth(&target_owner)).await,
        403,
        "the refused target was admitted anyway"
    );
    // Unknown roles are a bad request from either rung, not a stored role.
    assert_eq!(
        add_member_response(&base, &admin, "own5ecorp", unknown_id, "root")
            .await
            .status(),
        400,
        "an unknown org role must be rejected"
    );
    assert_eq!(
        add_member_response(&base, &creator, "own5ecorp", unknown_id, "root")
            .await
            .status(),
        400,
        "an unknown org role must be rejected by the owner too"
    );
    // An owner may hand out the role — and the new owner can dispose of the
    // organization immediately, which is the right the admin could not mint.
    assert_eq!(
        add_member_response(&base, &creator, "own5ecorp", target_owner_id, "owner")
            .await
            .status(),
        201,
        "an owner may grant the owner role"
    );
    assert_eq!(
        status(client.delete(&org_url).bearer_auth(&target_owner)).await,
        200,
        "the newly granted owner cannot delete the organization"
    );
}

/// The other leftover: `remove_org_member` deleted only the membership row. A
/// member who had added themselves (or been given) a direct `repo_collaborators`
/// row on one of the organization's repositories kept access to it after the
/// removal — collaborator rows are consulted before membership — and a
/// `team_members` row stayed behind to silently reactivate on re-add. Both must
/// go with the membership.
#[tokio::test]
async fn removing_a_member_revokes_their_direct_access_rows() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner, _) = register_full(&base, "own5f-owner", "own5f-owner@example.com").await;
    let (intruder, intruder_id) =
        register_full(&base, "own5f-intruder", "own5f-intruder@example.com").await;
    create_org(&base, &owner, "own5fcorp", "private").await;
    let repo_id = create_org_repo(&base, &owner, "own5fcorp", "vault", true).await;
    // Admin is what the `RepoAdmin` gate wants before it lets the intruder
    // write their own collaborator row (`can_admin_repo` admits org admins).
    add_member(&base, &owner, "own5fcorp", intruder_id, "admin").await;

    let client = reqwest::Client::new();
    let repo = format!("{base}/api/v1/repos/own5fcorp/vault");
    // The direct grant that used to survive the removal.
    assert_eq!(
        status(
            client
                .post(format!("{repo}/collaborators"))
                .bearer_auth(&intruder)
                .json(&serde_json::json!({"user_id": intruder_id, "permission": "admin"}))
        )
        .await,
        201,
        "an org admin may grant themselves a collaborator row"
    );
    // And a team membership that used to stay behind.
    let team_id = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/own5fcorp/teams"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"name": "vault-team", "permission": "admin"}))
        .send()
        .await
        .expect("create team")
        .json::<serde_json::Value>()
        .await
        .expect("json")["id"]
        .as_i64()
        .expect("team id");
    assert_eq!(
        status(
            client
                .post(format!(
                    "{base}/api/v1/orgs/own5fcorp/teams/{team_id}/members"
                ))
                .bearer_auth(&owner)
                .json(&serde_json::json!({"user_id": intruder_id, "role": "member"}))
        )
        .await,
        201,
        "baseline: the intruder joins a team"
    );

    // Baseline: the rows added up to real access.
    assert_eq!(status(client.get(&repo).bearer_auth(&intruder)).await, 200);
    assert!(
        listed_repos(&base, &intruder, "own5fcorp")
            .await
            .contains(&"vault".to_string()),
        "baseline: the private repository is listed to its admin"
    );
    let search: serde_json::Value = client
        .get(format!("{base}/api/v1/search?q=vault&type=repos"))
        .bearer_auth(&intruder)
        .send()
        .await
        .expect("search")
        .json()
        .await
        .expect("json");
    assert!(
        search["results"]
            .as_array()
            .is_some_and(|r| r.iter().any(|e| e["repo_name"].as_str() == Some("vault"))),
        "baseline: the private repository is searchable by its admin: {search}"
    );

    let removed = remove_member(&base, &owner, "own5fcorp", intruder_id).await;
    assert_eq!(removed.status(), 200, "the owner removes the intruder");

    // Read, write and admin are all refused now.
    let read = status(client.get(&repo).bearer_auth(&intruder)).await;
    assert!(
        matches!(read, 403 | 404),
        "the removed member still reads through the collaborator row: {read}"
    );
    assert_eq!(
        status(
            client
                .post(format!("{repo}/labels"))
                .bearer_auth(&intruder)
                .json(&serde_json::json!({"name": "intruder", "color": "#ff0000"}))
        )
        .await,
        403,
        "the removed member still writes through the collaborator row"
    );
    assert_eq!(
        status(
            client
                .post(format!("{repo}/collaborators"))
                .bearer_auth(&intruder)
                .json(&serde_json::json!({"user_id": intruder_id, "permission": "admin"}))
        )
        .await,
        403,
        "the removed member still administers through the collaborator row"
    );
    assert!(
        !listed_repos(&base, &intruder, "own5fcorp")
            .await
            .contains(&"vault".to_string()),
        "the private repository is still listed to the removed member"
    );
    let search: serde_json::Value = client
        .get(format!("{base}/api/v1/search?q=vault&type=repos"))
        .bearer_auth(&intruder)
        .send()
        .await
        .expect("search")
        .json()
        .await
        .expect("json");
    assert!(
        !search["results"]
            .as_array()
            .is_some_and(|r| r.iter().any(|e| e["repo_name"].as_str() == Some("vault"))),
        "the private repository is still searchable by the removed member: {search}"
    );

    // Both rows really are gone, not merely inert.
    let grants = repo_collaborator::Entity::find()
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .filter(repo_collaborator::Column::UserId.eq(intruder_id))
        .all(&db)
        .await
        .expect("read collaborator rows");
    assert!(
        grants.is_empty(),
        "the removed member's direct collaborator row survived: {grants:?}"
    );
    let teams = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(intruder_id))
        .all(&db)
        .await
        .expect("read team member rows");
    assert!(
        teams.is_empty(),
        "the removed member's team membership survived: {teams:?}"
    );
}
