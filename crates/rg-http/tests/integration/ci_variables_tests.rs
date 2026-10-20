//! card_a6f59b3d45f5: the `vars.*` store on the API — a repository's and an
//! organization's, each held to its administrators, values shown back.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn call(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_repositorys_variables_are_set_read_and_removed_by_its_administrators() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner, _) = register_full(&base, "vars_owner", "vars_owner@example.com").await;
    let (stranger, _) = register_full(&base, "vars_stranger", "vars_stranger@example.com").await;
    create_repo(&base, &owner, "app").await;
    let url = |name: &str| format!("{base}/api/v1/repos/vars_owner/app/actions/variables{name}");
    use reqwest::Method;

    let put = |value: &str| Some(serde_json::json!({ "value": value }));
    assert_eq!(
        call(Method::PUT, url("/REGION"), &owner, put("eu")).await.0,
        201
    );
    assert_eq!(
        call(Method::PUT, url("/REGION"), &owner, put("us")).await.0,
        204
    );
    let (status, body) = call(Method::GET, url("/REGION"), &owner, None).await;
    assert_eq!((status, &body["value"]), (200, &serde_json::json!("us")));
    let (status, body) = call(Method::GET, url(""), &owner, None).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");

    for (name, value) in [("/lower", "x"), ("/GITHUB_X", "x"), ("/OK", "")] {
        assert_eq!(
            call(Method::PUT, url(name), &owner, put(value)).await.0,
            400,
            "{name}={value:?}"
        );
    }
    assert_eq!(
        call(
            Method::PUT,
            url("/BIG"),
            &owner,
            put(&"x".repeat(48 * 1024 + 1))
        )
        .await
        .0,
        400
    );

    // A user who is not an administrator of the repository reads nothing.
    let (status, _) = call(Method::GET, url(""), &stranger, None).await;
    assert!(status == 403 || status == 404, "{status}");
    let (status, _) = call(Method::PUT, url("/REGION"), &stranger, put("evil")).await;
    assert!(status == 403 || status == 404, "{status}");

    assert_eq!(
        call(Method::DELETE, url("/REGION"), &owner, None).await.0,
        204
    );
    assert_eq!(
        call(Method::DELETE, url("/REGION"), &owner, None).await.0,
        404
    );
    assert_eq!(call(Method::GET, url("/REGION"), &owner, None).await.0, 404);
}

#[tokio::test]
async fn an_organizations_variables_are_held_to_its_administrators() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (admin, _) = register_full(&base, "vars_org_admin", "vars_org_admin@example.com").await;
    let (member, member_id) =
        register_full(&base, "vars_org_member", "vars_org_member@example.com").await;
    use reqwest::Method;
    let (status, body) = call(
        Method::POST,
        format!("{base}/api/v1/orgs"),
        &admin,
        Some(serde_json::json!({ "name": "vars-acme" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = call(
        Method::POST,
        format!("{base}/api/v1/orgs/vars-acme/members"),
        &admin,
        Some(serde_json::json!({ "user_id": member_id, "role": "member" })),
    )
    .await;
    assert!((200..300).contains(&status), "{status} {body}");
    let url = |name: &str| format!("{base}/api/v1/orgs/vars-acme/actions/variables{name}");

    assert_eq!(
        call(
            Method::PUT,
            url("/TIER"),
            &admin,
            Some(serde_json::json!({ "value": "gold" }))
        )
        .await
        .0,
        201
    );
    let (status, body) = call(Method::GET, url(""), &admin, None).await;
    assert_eq!(status, 200);
    assert_eq!(body[0]["name"], "TIER");
    assert_eq!(body[0]["value"], "gold");

    // A member sees the organization, and is refused its configuration.
    assert_eq!(call(Method::GET, url(""), &member, None).await.0, 403);
    assert_eq!(
        call(
            Method::PUT,
            url("/TIER"),
            &member,
            Some(serde_json::json!({ "value": "x" }))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(Method::DELETE, url("/TIER"), &admin, None).await.0,
        204
    );
}
