//! Integration tests for Board / Column / Card endpoints.
//!
//! Guards against regressions in the Kanban board API:
//!   POST   /repos/:o/:r/boards            — create board (auto-creates 3 default columns)
//!   GET    /repos/:o/:r/boards            — list boards
//!   GET    /repos/:o/:r/boards/:id        — get board with columns+cards
//!   DELETE /repos/:o/:r/boards/:id        — delete board
//!   POST   /repos/:o/:r/boards/:id/columns               — create column
//!   DELETE /repos/:o/:r/boards/:id/columns/:col_id       — delete column
//!   POST   /repos/:o/:r/boards/:id/columns/:col_id/cards — create card
//!   POST   /repos/:o/:r/boards/:id/cards/:card_id/move   — move card to another column
//!   DELETE /repos/:o/:r/boards/:id/cards/:card_id        — delete card

mod common;

use common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

// ── helpers ──────────────────────────────────────────────────────────────────

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("boarduser{suffix}");
    let email = format!("boarduser{suffix}@example.com");
    let token = register_user(&base, &owner, &email, PW).await;
    let repo = format!("boardrepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

async fn create_board(base: &str, token: &str, owner: &str, repo: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/boards", base, owner, repo))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_board failed: {}", resp.status());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn get_board_full(base: &str, owner: &str, repo: &str, board_id: i64) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/boards/{}",
            base, owner, repo, board_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_board_with_default_columns() {
    let (base, token, owner, repo) = setup("1").await;

    let board_id = create_board(&base, &token, &owner, &repo, "Sprint 1").await;
    let board = get_board_full(&base, &owner, &repo, board_id).await;

    assert_eq!(board["board"]["name"], "Sprint 1");
    let cols = board["columns"].as_array().unwrap();
    // create_board auto-creates "To Do", "In Progress", "Done"
    assert_eq!(
        cols.len(),
        3,
        "expected 3 default columns, got {}",
        cols.len()
    );
    let names: Vec<&str> = cols
        .iter()
        .filter_map(|c| c["column"]["name"].as_str())
        .collect();
    assert!(names.contains(&"To Do"), "missing 'To Do'");
    assert!(names.contains(&"In Progress"), "missing 'In Progress'");
    assert!(names.contains(&"Done"), "missing 'Done'");
}

#[tokio::test]
async fn test_list_boards() {
    let (base, token, owner, repo) = setup("2").await;

    create_board(&base, &token, &owner, &repo, "Board A").await;
    create_board(&base, &token, &owner, &repo, "Board B").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/boards",
            &base, &owner, &repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let boards: serde_json::Value = resp.json().await.unwrap();
    let arr = boards.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 boards, got {}", arr.len());
    let names: Vec<&str> = arr.iter().filter_map(|b| b["name"].as_str()).collect();
    assert!(names.contains(&"Board A") && names.contains(&"Board B"));
}

#[tokio::test]
async fn test_add_card_to_column() {
    let (base, token, owner, repo) = setup("3").await;
    let board_id = create_board(&base, &token, &owner, &repo, "Kanban").await;
    let board = get_board_full(&base, &owner, &repo, board_id).await;

    // Pick the first column
    let col_id = board["columns"][0]["column"]["id"].as_i64().unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/columns/{}/cards",
            &base, &owner, &repo, board_id, col_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"note": "Fix the login bug"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_card failed: {}", resp.status());
    let card: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(card["note"], "Fix the login bug");
    assert_eq!(card["column_id"], col_id);

    // Verify it appears in the board
    let board2 = get_board_full(&base, &owner, &repo, board_id).await;
    let cards = board2["columns"][0]["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["note"], "Fix the login bug");
}

#[tokio::test]
async fn test_move_card_between_columns() {
    let (base, token, owner, repo) = setup("4").await;
    let board_id = create_board(&base, &token, &owner, &repo, "Flow").await;
    let board = get_board_full(&base, &owner, &repo, board_id).await;

    let col0_id = board["columns"][0]["column"]["id"].as_i64().unwrap();
    let col1_id = board["columns"][1]["column"]["id"].as_i64().unwrap();

    let client = reqwest::Client::new();

    // Create a card in column 0
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/columns/{}/cards",
            &base, &owner, &repo, board_id, col0_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"note": "Move me"}))
        .send()
        .await
        .unwrap();
    let card_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // Move to column 1
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/cards/{}/move",
            &base, &owner, &repo, board_id, card_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"column_id": col1_id, "position": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "move_card failed: {}", resp.status());

    // Verify card is now in column 1, not column 0
    let board2 = get_board_full(&base, &owner, &repo, board_id).await;
    let col0_cards = board2["columns"][0]["cards"].as_array().unwrap();
    let col1_cards = board2["columns"][1]["cards"].as_array().unwrap();
    assert_eq!(col0_cards.len(), 0, "card should have left column 0");
    assert_eq!(col1_cards.len(), 1, "card should be in column 1");
    assert_eq!(col1_cards[0]["id"], card_id);
}

#[tokio::test]
async fn test_delete_card() {
    let (base, token, owner, repo) = setup("5").await;
    let board_id = create_board(&base, &token, &owner, &repo, "Del Test").await;
    let board = get_board_full(&base, &owner, &repo, board_id).await;
    let col_id = board["columns"][0]["column"]["id"].as_i64().unwrap();

    let client = reqwest::Client::new();
    let card_id = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/columns/{}/cards",
            &base, &owner, &repo, board_id, col_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"note": "temp"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    // Delete card
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/cards/{}",
            &base, &owner, &repo, board_id, card_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "delete_card failed: {}", resp.status());

    // Verify it's gone
    let board2 = get_board_full(&base, &owner, &repo, board_id).await;
    let cards = board2["columns"][0]["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 0, "card should be deleted");
}

#[tokio::test]
async fn test_add_and_delete_column() {
    let (base, token, owner, repo) = setup("6").await;
    let board_id = create_board(&base, &token, &owner, &repo, "ColTest").await;

    let client = reqwest::Client::new();

    // Add a custom column
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/columns",
            &base, &owner, &repo, board_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "Review", "color": "#f59e0b"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create_column failed: {}",
        resp.status()
    );
    let col_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // Should now have 4 columns (3 default + 1 new)
    let board = get_board_full(&base, &owner, &repo, board_id).await;
    assert_eq!(board["columns"].as_array().unwrap().len(), 4);

    // Delete the custom column
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/boards/{}/columns/{}",
            &base, &owner, &repo, board_id, col_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "delete_column failed: {}",
        resp.status()
    );

    // Back to 3 columns
    let board2 = get_board_full(&base, &owner, &repo, board_id).await;
    assert_eq!(board2["columns"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_delete_board() {
    let (base, token, owner, repo) = setup("7").await;
    let board_id = create_board(&base, &token, &owner, &repo, "Ephemeral").await;

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/boards/{}",
            &base, &owner, &repo, board_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Board list should now be empty
    let boards: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/boards",
            &base, &owner, &repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(boards.as_array().unwrap().len(), 0);
}
