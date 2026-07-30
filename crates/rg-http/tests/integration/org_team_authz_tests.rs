//! Regression coverage for card_f77e96550d97: the organization and team
//! endpoints must authenticate *and* authorize their caller, and must honour
//! the organization named in the path.
//!
//! Before this, every `/orgs/{name}/teams/**` handler skipped
//! `extract_user_id` entirely — and nothing above them made up for it, because
//! the only auth layer on `/api/v1` (`pat_auth_middleware`) merely translates a
//! PAT into a JWT and lets a credential-less request through. An anonymous
//! `POST /orgs/{someone-elses-org}/teams` created a team with
//! `permission: admin`, and `add_team_member` then converted that into read
//! access to the organization's private repositories.
//!
//! Two more holes of the same class are covered here: the org-member handlers
//! authenticated but never checked the caller's *role* (any logged-in user
//! could add themselves as `owner` of any organization — which would have made
//! the team fix pointless), and `Path((_name, team_id))` ignored the org in the
//! path, so a team id belonging to another organization was reachable through
//! one's own.

use crate::common::{register_full, spawn_test_app};

/// Create an org owned by `token`; returns nothing but asserts the 201.
async fn create_org(base: &str, token: &str, name: &str, visibility: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "visibility": visibility}))
        .send()
        .await
        .expect("create org");
    assert_eq!(resp.status(), 201, "create_org({name}) should succeed");
}

/// Create a team in `org` as `token`; returns its id.
async fn create_team(base: &str, token: &str, org: &str, team: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/teams"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": team, "permission": "admin"}))
        .send()
        .await
        .expect("create team");
    assert_eq!(resp.status(), 201, "create_team({team}) should succeed");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("team id")
}

#[tokio::test]
async fn anonymous_requests_cannot_manage_teams() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "anon-owner", "anon-owner@example.com").await;
    let (_, outsider_id) = register_full(&base, "anon-bystander", "anon-by@example.com").await;
    create_org(&base, &owner_token, "anon-org", "public").await;
    let team_id = create_team(&base, &owner_token, "anon-org", "devs").await;

    let client = reqwest::Client::new();
    let team_url = format!("{base}/api/v1/orgs/anon-org/teams/{team_id}");

    // Not a single one of these carries an Authorization header.
    let create = client
        .post(format!("{base}/api/v1/orgs/anon-org/teams"))
        .json(&serde_json::json!({"name": "backdoor", "permission": "admin"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        create.status(),
        401,
        "an anonymous caller must not be able to create a team"
    );

    let add_member = client
        .post(format!("{team_url}/members"))
        .json(&serde_json::json!({"user_id": outsider_id, "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        add_member.status(),
        401,
        "an anonymous caller must not be able to grant team membership"
    );

    let remove_member = client
        .delete(format!("{team_url}/members/{outsider_id}"))
        .send()
        .await
        .expect("request");
    assert_eq!(remove_member.status(), 401);

    let delete = client.delete(&team_url).send().await.expect("request");
    assert_eq!(
        delete.status(),
        401,
        "an anonymous caller must not be able to delete a team"
    );

    // …and the team is still there.
    let teams = client
        .get(format!("{base}/api/v1/orgs/anon-org/teams"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(teams.status(), 200);
    let names: Vec<String> = teams
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("json")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        names,
        vec!["devs".to_string()],
        "no anonymous request may have changed the team list"
    );
}

#[tokio::test]
async fn an_authenticated_outsider_is_refused() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "out-owner", "out-owner@example.com").await;
    let (outsider_token, outsider_id) =
        register_full(&base, "out-stranger", "out-stranger@example.com").await;
    create_org(&base, &owner_token, "out-org", "public").await;
    let team_id = create_team(&base, &owner_token, "out-org", "devs").await;

    let client = reqwest::Client::new();
    let team_url = format!("{base}/api/v1/orgs/out-org/teams/{team_id}");

    for (label, response) in [
        (
            "create_team",
            client
                .post(format!("{base}/api/v1/orgs/out-org/teams"))
                .bearer_auth(&outsider_token)
                .json(&serde_json::json!({"name": "backdoor", "permission": "admin"}))
                .send()
                .await
                .expect("request"),
        ),
        (
            "delete_team",
            client
                .delete(&team_url)
                .bearer_auth(&outsider_token)
                .send()
                .await
                .expect("request"),
        ),
        (
            "add_team_member",
            client
                .post(format!("{team_url}/members"))
                .bearer_auth(&outsider_token)
                .json(&serde_json::json!({"user_id": outsider_id, "role": "member"}))
                .send()
                .await
                .expect("request"),
        ),
        (
            // The org-level twin of the same hole: this handler authenticated
            // its caller but never checked their role, so anyone could promote
            // themselves to `owner` and then legitimately do everything above.
            "add_org_member",
            client
                .post(format!("{base}/api/v1/orgs/out-org/members"))
                .bearer_auth(&outsider_token)
                .json(&serde_json::json!({"user_id": outsider_id, "role": "owner"}))
                .send()
                .await
                .expect("request"),
        ),
        (
            "update_org",
            client
                .patch(format!("{base}/api/v1/orgs/out-org"))
                .bearer_auth(&outsider_token)
                .json(&serde_json::json!({"display_name": "Pwned"}))
                .send()
                .await
                .expect("request"),
        ),
    ] {
        assert_eq!(
            response.status(),
            403,
            "{label} must refuse a user with no role in the organization"
        );
    }
}

#[tokio::test]
async fn org_admins_may_manage_teams_but_plain_members_may_not() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "role-owner", "role-owner@example.com").await;
    let (admin_token, admin_id) =
        register_full(&base, "role-admin", "role-admin@example.com").await;
    let (member_token, member_id) =
        register_full(&base, "role-member", "role-member@example.com").await;
    create_org(&base, &owner_token, "role-org", "public").await;

    let client = reqwest::Client::new();
    for (user_id, role) in [(admin_id, "admin"), (member_id, "member")] {
        let resp = client
            .post(format!("{base}/api/v1/orgs/role-org/members"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({"user_id": user_id, "role": role}))
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 201, "the owner may add a {role}");
    }

    // The gate is a role check, not an owner check: an org admin runs the team
    // surface for real, so a fix that only ever compared `org.owner_id` would
    // have broken them.
    let by_admin = client
        .post(format!("{base}/api/v1/orgs/role-org/teams"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"name": "platform", "permission": "write"}))
        .send()
        .await
        .expect("request");
    assert_eq!(by_admin.status(), 201, "an org admin may create a team");
    let team_id = by_admin.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("team id");

    let by_member = client
        .post(format!("{base}/api/v1/orgs/role-org/teams"))
        .bearer_auth(&member_token)
        .json(&serde_json::json!({"name": "shadow", "permission": "admin"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        by_member.status(),
        403,
        "a plain org member may not create a team"
    );

    let deleted_by_member = client
        .delete(format!("{base}/api/v1/orgs/role-org/teams/{team_id}"))
        .bearer_auth(&member_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        deleted_by_member.status(),
        403,
        "a plain org member may not delete a team"
    );

    let deleted_by_admin = client
        .delete(format!("{base}/api/v1/orgs/role-org/teams/{team_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        deleted_by_admin.status(),
        200,
        "an org admin may delete a team"
    );
}

#[tokio::test]
async fn a_team_of_another_org_is_not_reachable_through_my_own_path() {
    let base = spawn_test_app().await;
    let (victim_token, _) = register_full(&base, "idor-victim", "idor-victim@example.com").await;
    let (attacker_token, attacker_id) =
        register_full(&base, "idor-attacker", "idor-attacker@example.com").await;
    create_org(&base, &victim_token, "idor-victim-org", "public").await;
    create_org(&base, &attacker_token, "idor-attacker-org", "public").await;
    let victim_team = create_team(&base, &victim_token, "idor-victim-org", "victim-devs").await;

    let client = reqwest::Client::new();
    // The attacker owns `idor-attacker-org`, so every authorization check on
    // that path passes — only the `team.org_id` comparison stands between them
    // and another organization's team.
    let foreign = format!("{base}/api/v1/orgs/idor-attacker-org/teams/{victim_team}");

    let read = client
        .get(&foreign)
        .bearer_auth(&attacker_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        read.status(),
        404,
        "a team belonging to another org must not be readable through my path"
    );

    let grant = client
        .post(format!("{foreign}/members"))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"user_id": attacker_id, "role": "maintainer"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        grant.status(),
        404,
        "membership in another org's team must not be grantable through my path"
    );

    let delete = client
        .delete(&foreign)
        .bearer_auth(&attacker_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        delete.status(),
        404,
        "a team belonging to another org must not be deletable through my path"
    );

    // The team, and its membership, survived all three.
    let still_there = client
        .get(format!(
            "{base}/api/v1/orgs/idor-victim-org/teams/{victim_team}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .expect("request");
    assert_eq!(still_there.status(), 200, "the victim's team still exists");

    let members = client
        .get(format!(
            "{base}/api/v1/orgs/idor-victim-org/teams/{victim_team}/members"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .expect("request");
    assert_eq!(members.status(), 200);
    let ids: Vec<i64> = members
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("json")
        .iter()
        .filter_map(|m| m["user_id"].as_i64())
        .collect();
    assert!(
        !ids.contains(&attacker_id),
        "the attacker must not have joined the victim's team, got {ids:?}"
    );
}

#[tokio::test]
async fn a_private_org_is_not_readable_from_outside() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "priv-owner", "priv-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "priv-outsider", "priv-out@example.com").await;
    let (member_token, member_id) =
        register_full(&base, "priv-member", "priv-mem@example.com").await;
    create_org(&base, &owner_token, "priv-org", "private").await;
    let team_id = create_team(&base, &owner_token, "priv-org", "secret-devs").await;

    let client = reqwest::Client::new();
    let org_url = format!("{base}/api/v1/orgs/priv-org");
    let urls = [
        org_url.clone(),
        format!("{org_url}/members"),
        format!("{org_url}/teams"),
        format!("{org_url}/teams/{team_id}"),
        format!("{org_url}/teams/{team_id}/members"),
    ];

    for url in &urls {
        let anonymous = client.get(url).send().await.expect("request");
        // 404 rather than 403: a 403 would confirm the organization exists.
        assert_eq!(
            anonymous.status(),
            404,
            "{url} must not answer an anonymous reader for a private org"
        );

        let outsider = client
            .get(url)
            .bearer_auth(&outsider_token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            outsider.status(),
            404,
            "{url} must not answer a non-member for a private org"
        );
    }

    let added = client
        .post(format!("{org_url}/members"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": member_id, "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(added.status(), 201);

    for url in &urls {
        let as_member = client
            .get(url)
            .bearer_auth(&member_token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            as_member.status(),
            200,
            "{url} must stay readable for a member of the private org"
        );
    }
}

/// Send one request, gate first: returns only the status code.
///
/// Every route below takes its access level as a handler argument, so the body
/// is deserialized *after* the access decision — but a well-formed body is sent
/// anyway, so a `400`/`422` can never be mistaken for a denial.
async fn status_of(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::StatusCode {
    let mut req = client.request(method, url).json(body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send().await.expect("request").status()
}

/// Every mutating org route, as `(method, path after the org name, body)`.
///
/// The ids are filled from the fixture, but which org a `team_id` belongs to is
/// deliberately not made to match: the gate answers before any handler reads the
/// id, and a route where the id changes the verdict is a route whose gate runs
/// too late.
fn org_admin_routes(
    team_id: i64,
    user_id: i64,
) -> Vec<(reqwest::Method, String, serde_json::Value)> {
    use reqwest::Method;

    vec![
        (
            Method::PATCH,
            String::new(),
            serde_json::json!({"display_name": "renamed by a stranger"}),
        ),
        (Method::DELETE, String::new(), serde_json::json!({})),
        (
            Method::POST,
            "/members".to_string(),
            serde_json::json!({"user_id": user_id, "role": "owner"}),
        ),
        (
            Method::DELETE,
            format!("/members/{user_id}"),
            serde_json::json!({}),
        ),
        (
            Method::POST,
            "/teams".to_string(),
            serde_json::json!({"name": "backdoor", "permission": "admin"}),
        ),
        (
            Method::DELETE,
            format!("/teams/{team_id}"),
            serde_json::json!({}),
        ),
        (
            Method::POST,
            format!("/teams/{team_id}/members"),
            serde_json::json!({"user_id": user_id, "role": "member"}),
        ),
        (
            Method::DELETE,
            format!("/teams/{team_id}/members/{user_id}"),
            serde_json::json!({}),
        ),
    ]
}

/// card_c46c354ec3ae: the `OrgAdmin` gate must mask a private organization
/// exactly as far as the read gate does.
///
/// `a_private_org_is_not_readable_from_outside` above drives five `GET`s and is
/// satisfied by all of them — while the eight mutating routes on the same paths
/// answered `403` on a private organization and `404` on an unknown one. So the
/// masking the read gate provides came off by changing the verb: `PATCH
/// /orgs/{name}` confirmed by name what `GET /orgs/{name}` refuses to.
///
/// Both codes are measured in one test on purpose. A private org asserted to
/// answer `404` proves nothing on its own — an absent org answers `404` too, and
/// the whole question is whether the two are distinguishable.
#[tokio::test]
async fn the_org_admin_gate_masks_a_private_org_as_far_as_the_read_gate_does() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "mask-owner", "mask-owner@example.com").await;
    let (outsider_token, outsider_id) =
        register_full(&base, "mask-outsider", "mask-out@example.com").await;
    let (member_token, member_id) =
        register_full(&base, "mask-member", "mask-mem@example.com").await;
    create_org(&base, &owner_token, "mask-private", "private").await;
    create_org(&base, &owner_token, "mask-public", "public").await;
    let private_team = create_team(&base, &owner_token, "mask-private", "hidden-devs").await;
    let public_team = create_team(&base, &owner_token, "mask-public", "open-devs").await;

    let client = reqwest::Client::new();
    let orgs = format!("{base}/api/v1/orgs");

    // A plain member of the private org, for the third pass below.
    let added = status_of(
        &client,
        reqwest::Method::POST,
        &format!("{orgs}/mask-private/members"),
        Some(&owner_token),
        &serde_json::json!({"user_id": member_id, "role": "member"}),
    )
    .await;
    assert_eq!(added, 201, "fixture: adding the plain member failed");

    // 1. The outsider: a private org and an org that does not exist must be
    //    indistinguishable — same code, and that code is not `403`.
    for (method, suffix, body) in org_admin_routes(private_team, outsider_id) {
        let private = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-private{suffix}"),
            Some(&outsider_token),
            &body,
        )
        .await;
        let absent = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-nosuchorg{suffix}"),
            Some(&outsider_token),
            &body,
        )
        .await;
        assert_eq!(
            private, absent,
            "{method} /orgs/{{name}}{suffix}: a private org answered {private} and an absent one \
             {absent} — the difference is the existence oracle"
        );
        assert_eq!(
            private, 404,
            "{method} /orgs/{{name}}{suffix}: a private org owes an outsider 404"
        );
    }

    // 2. An anonymous caller is `401` either way — the gate authenticates before
    //    it resolves anything, so it cannot leak what it has not looked up.
    for (method, suffix, body) in org_admin_routes(private_team, outsider_id) {
        let private = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-private{suffix}"),
            None,
            &body,
        )
        .await;
        let absent = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-nosuchorg{suffix}"),
            None,
            &body,
        )
        .await;
        assert_eq!(private, 401, "{method} /orgs/{{name}}{suffix} owes 401");
        assert_eq!(absent, 401, "{method} /orgs/{{name}}{suffix} owes 401");
    }

    // 3. A *public* org keeps answering `403`: its existence is not a secret, so
    //    masking it would only make the refusal harder to read. This is the half
    //    that would break if the visibility check were made unconditional.
    for (method, suffix, body) in org_admin_routes(public_team, outsider_id) {
        let status = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-public{suffix}"),
            Some(&outsider_token),
            &body,
        )
        .await;
        assert_eq!(
            status, 403,
            "{method} /orgs/{{name}}{suffix}: a public org owes an outsider a plain 403"
        );
    }

    // 4. A member of the private org gets `403`, not `404`: they can already see
    //    the organization, so refusing them by permission leaks nothing. The gate
    //    hides the org from strangers, it does not hide it from its own people.
    for (method, suffix, body) in org_admin_routes(private_team, outsider_id) {
        let status = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-private{suffix}"),
            Some(&member_token),
            &body,
        )
        .await;
        assert_eq!(
            status, 403,
            "{method} /orgs/{{name}}{suffix}: a member of the private org owes 403, not 404"
        );
    }

    // 5. Baseline, in the same run: the owner still gets through every one of
    //    them. Without it the four passes above are equally satisfied by a
    //    fixture that never worked. Ordered so the destructive ones come last.
    let baseline: Vec<(reqwest::Method, String, serde_json::Value)> = vec![
        (
            reqwest::Method::PATCH,
            String::new(),
            serde_json::json!({"display_name": "renamed by its owner"}),
        ),
        (
            reqwest::Method::POST,
            format!("/teams/{private_team}/members"),
            serde_json::json!({"user_id": member_id, "role": "member"}),
        ),
        (
            reqwest::Method::DELETE,
            format!("/teams/{private_team}/members/{member_id}"),
            serde_json::json!({}),
        ),
        (
            reqwest::Method::DELETE,
            format!("/teams/{private_team}"),
            serde_json::json!({}),
        ),
        (
            reqwest::Method::DELETE,
            format!("/members/{member_id}"),
            serde_json::json!({}),
        ),
        (
            reqwest::Method::DELETE,
            String::new(),
            serde_json::json!({}),
        ),
    ];
    for (method, suffix, body) in baseline {
        let status = status_of(
            &client,
            method.clone(),
            &format!("{orgs}/mask-private{suffix}"),
            Some(&owner_token),
            &body,
        )
        .await;
        assert!(
            status.is_success(),
            "baseline: the owner must still pass {method} /orgs/{{name}}{suffix}, got {status}"
        );
    }
}
