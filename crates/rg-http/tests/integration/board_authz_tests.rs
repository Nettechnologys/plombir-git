//! Authorization regressions for the Board / Column / Card endpoints.
//!
//! Two separate failures used to live here, and both are covered below:
//!
//!   1. No handler resolved `owner/name` into a repository, so nothing checked
//!      the caller against it. `GET .../boards/{id}` did not even take a token —
//!      the board of a private repository was readable anonymously — and the
//!      mutating handlers stopped at `extract_bearer_claims`, which made any
//!      account able to edit and delete the boards of every repository.
//!
//!   2. Board, column, card and issue ids are global primary keys, so even with
//!      a permission check in place, write access to one repository would still
//!      reach the boards of another until each object is matched against the
//!      repository the check was about.

use crate::common::{create_issue, create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

// ── helpers ──────────────────────────────────────────────────────────────────

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

async fn create_board(base: &str, token: &str, owner: &str, repo: &str, name: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/boards"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_board '{name}' failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// First column of a board, read back through the owner's own token.
async fn first_column(base: &str, token: &str, owner: &str, repo: &str, board_id: i64) -> i64 {
    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/boards/{board_id}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "get_board failed for the owner");
    resp.json::<serde_json::Value>().await.unwrap()["columns"][0]["column"]["id"]
        .as_i64()
        .unwrap()
}

async fn create_card(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    board_id: i64,
    col_id: i64,
    note: &str,
) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/boards/{board_id}/columns/{col_id}/cards"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"note": note}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_card '{note}' failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// One request against the board API, optionally authenticated.
async fn call(
    method: reqwest::Method,
    url: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> reqwest::StatusCode {
    let mut req = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    req.send().await.unwrap().status()
}

/// Every board endpoint of one board, as `(method, url, body)` triples.
fn all_endpoints(
    base: &str,
    owner: &str,
    repo: &str,
    board_id: i64,
    col_id: i64,
    card_id: i64,
) -> Vec<(reqwest::Method, String, Option<serde_json::Value>)> {
    use reqwest::Method;
    let root = format!("{base}/api/v1/repos/{owner}/{repo}/boards");
    vec![
        (Method::GET, root.clone(), None),
        (
            Method::POST,
            root.clone(),
            Some(serde_json::json!({"name": "intruder"})),
        ),
        (Method::GET, format!("{root}/{board_id}"), None),
        (
            Method::PATCH,
            format!("{root}/{board_id}"),
            Some(serde_json::json!({"name": "hijacked"})),
        ),
        (Method::DELETE, format!("{root}/{board_id}"), None),
        (
            Method::POST,
            format!("{root}/{board_id}/columns"),
            Some(serde_json::json!({"name": "intruder"})),
        ),
        (
            Method::PATCH,
            format!("{root}/{board_id}/columns/{col_id}"),
            Some(serde_json::json!({"name": "hijacked"})),
        ),
        (
            Method::DELETE,
            format!("{root}/{board_id}/columns/{col_id}"),
            None,
        ),
        (
            Method::POST,
            format!("{root}/{board_id}/columns/{col_id}/cards"),
            Some(serde_json::json!({"note": "intruder"})),
        ),
        (
            Method::PATCH,
            format!("{root}/{board_id}/cards/{card_id}"),
            Some(serde_json::json!({"note": "hijacked"})),
        ),
        (
            Method::POST,
            format!("{root}/{board_id}/cards/{card_id}/move"),
            Some(serde_json::json!({"column_id": col_id, "position": 0})),
        ),
        (
            Method::POST,
            format!("{root}/{board_id}/cards/reorder"),
            Some(serde_json::json!({"positions": [[card_id, 5]]})),
        ),
        (
            Method::DELETE,
            format!("{root}/{board_id}/cards/{card_id}"),
            None,
        ),
    ]
}

// ── tests ────────────────────────────────────────────────────────────────────

/// A private repository's board is closed to anonymous callers (401) and to
/// authenticated outsiders (403) on every endpoint — reads included.
#[tokio::test]
async fn private_board_endpoints_reject_anonymous_and_outsiders() {
    let base = spawn_test_app().await;
    let owner = "boardauthzowner";
    let outsider = "boardauthzoutsider";
    let repo = "boardauthzrepo";
    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let outsider_token =
        register_user(&base, outsider, &format!("{outsider}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;

    let board_id = create_board(&base, &owner_token, owner, repo, "Private plan").await;
    let col_id = first_column(&base, &owner_token, owner, repo, board_id).await;
    let card_id = create_card(
        &base,
        &owner_token,
        owner,
        repo,
        board_id,
        col_id,
        "secret work",
    )
    .await;

    for (method, url, body) in all_endpoints(&base, owner, repo, board_id, col_id, card_id) {
        let anonymous = call(method.clone(), &url, None, body.clone()).await;
        assert_eq!(
            anonymous, 401,
            "anonymous {method} {url} answered {anonymous}"
        );

        let stranger = call(method.clone(), &url, Some(&outsider_token), body.clone()).await;
        assert_eq!(stranger, 403, "outsider {method} {url} answered {stranger}");
    }

    // Nothing above went through: the board still carries its original name and
    // its card is untouched.
    let board: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/boards/{board_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(board["board"]["name"], "Private plan");
    assert_eq!(board["columns"][0]["cards"][0]["note"], "secret work");
}

/// Write access to one repository does not reach another repository's board,
/// columns or cards — not through the path, and not through an id in the body.
#[tokio::test]
async fn board_children_stay_inside_their_repository() {
    let base = spawn_test_app().await;
    let victim = "boardscopevictim";
    let attacker = "boardscopeattacker";
    let victim_repo = "victimrepo";
    let attacker_repo = "attackerrepo";
    let victim_token = register_user(&base, victim, &format!("{victim}@example.com"), PW).await;
    let attacker_token =
        register_user(&base, attacker, &format!("{attacker}@example.com"), PW).await;

    create_private_repo(&base, &victim_token, victim_repo).await;
    create_repo(&base, &attacker_token, attacker_repo).await;

    let victim_board = create_board(&base, &victim_token, victim, victim_repo, "Victim").await;
    let victim_col = first_column(&base, &victim_token, victim, victim_repo, victim_board).await;
    let victim_card = create_card(
        &base,
        &victim_token,
        victim,
        victim_repo,
        victim_board,
        victim_col,
        "confidential",
    )
    .await;
    let (victim_issue, _) =
        create_issue(&base, &victim_token, victim, victim_repo, "Embargoed").await;

    let attacker_board =
        create_board(&base, &attacker_token, attacker, attacker_repo, "Attacker").await;
    let attacker_col = first_column(
        &base,
        &attacker_token,
        attacker,
        attacker_repo,
        attacker_board,
    )
    .await;

    let root = format!("{base}/api/v1/repos/{attacker}/{attacker_repo}/boards");
    let token = Some(attacker_token.as_str());
    use reqwest::Method;

    // The victim's board id under a path the attacker may write to.
    for (method, url, body) in [
        (
            Method::PATCH,
            format!("{root}/{victim_board}"),
            Some(serde_json::json!({"name": "hijacked"})),
        ),
        (Method::DELETE, format!("{root}/{victim_board}"), None),
        (Method::GET, format!("{root}/{victim_board}"), None),
        (
            Method::POST,
            format!("{root}/{victim_board}/columns"),
            Some(serde_json::json!({"name": "hijacked"})),
        ),
    ] {
        let status = call(method.clone(), &url, token, body).await;
        assert_eq!(
            status, 404,
            "cross-repo board {method} {url} answered {status}"
        );
    }

    // The victim's column and card ids hung off the attacker's own board.
    for (method, url, body) in [
        (
            Method::PATCH,
            format!("{root}/{attacker_board}/columns/{victim_col}"),
            Some(serde_json::json!({"name": "hijacked"})),
        ),
        (
            Method::DELETE,
            format!("{root}/{attacker_board}/columns/{victim_col}"),
            None,
        ),
        (
            Method::POST,
            format!("{root}/{attacker_board}/columns/{victim_col}/cards"),
            Some(serde_json::json!({"note": "hijacked"})),
        ),
        (
            Method::PATCH,
            format!("{root}/{attacker_board}/cards/{victim_card}"),
            Some(serde_json::json!({"note": "hijacked"})),
        ),
        (
            Method::DELETE,
            format!("{root}/{attacker_board}/cards/{victim_card}"),
            None,
        ),
        (
            Method::POST,
            format!("{root}/{attacker_board}/cards/{victim_card}/move"),
            Some(serde_json::json!({"column_id": attacker_col, "position": 0})),
        ),
        // Ids that arrive in the body, not the path: the move destination and
        // the reorder batch.
        (
            Method::POST,
            format!("{root}/{attacker_board}/cards/reorder"),
            Some(serde_json::json!({"positions": [[victim_card, 9]]})),
        ),
    ] {
        let status = call(method.clone(), &url, token, body).await;
        assert_eq!(
            status, 404,
            "cross-repo child {method} {url} answered {status}"
        );
    }

    // Moving one's own card onto the victim's column is the same class, and it
    // has to fail before the card is written.
    let own_card = create_card(
        &base,
        &attacker_token,
        attacker,
        attacker_repo,
        attacker_board,
        attacker_col,
        "own",
    )
    .await;
    let status = call(
        Method::POST,
        &format!("{root}/{attacker_board}/cards/{own_card}/move"),
        token,
        Some(serde_json::json!({"column_id": victim_col, "position": 0})),
    )
    .await;
    assert_eq!(status, 404, "move onto a foreign column answered {status}");

    // `get_board` embeds the linked issue, so an unchecked `issue_id` would
    // turn the attacker's own board into a reader for a private repository's
    // issues.
    for (method, url) in [
        (
            Method::POST,
            format!("{root}/{attacker_board}/columns/{attacker_col}/cards"),
        ),
        (
            Method::PATCH,
            format!("{root}/{attacker_board}/cards/{own_card}"),
        ),
    ] {
        let status = call(
            method.clone(),
            &url,
            token,
            Some(serde_json::json!({"issue_id": victim_issue})),
        )
        .await;
        assert_eq!(
            status, 404,
            "foreign issue link {method} {url} answered {status}"
        );
    }

    // The victim's board came through all of it unchanged.
    let board: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/boards/{victim_board}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(board["board"]["name"], "Victim");
    let cards = board["columns"][0]["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1, "the victim's card was removed or moved");
    assert_eq!(cards[0]["note"], "confidential");
    assert_eq!(cards[0]["position"], 0, "the victim's card was reordered");
    assert!(
        cards[0]["issue"].is_null(),
        "the victim's card gained an issue link"
    );
}
