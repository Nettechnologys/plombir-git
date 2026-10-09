//! A label or board-column colour must be a colour, not a fragment of CSS.
//!
//! Both services accepted anything shaped like `#` plus six characters —
//! `#0;x:1;` passed label validation, and board columns stored whatever they
//! were sent. The web client writes the stored value into a `style` attribute
//! (`background-color: {color}`) and the app's CSP allows `style-src
//! 'unsafe-inline'`, so a malformed value closes its declaration and the rest
//! of the string becomes CSS on every reader's page.
//!
//! `rg_core::validate_hex_color` is the rule now, and these tests hold the four
//! write routes to it: create and update, labels and columns. A valid colour
//! still goes through — and comes back normalised lowercase, so the stored
//! value is exactly `#rrggbb`.

use crate::common::{create_repo, register_user, spawn_test_app};
use reqwest::StatusCode;

const PW: &str = "Qz7$wRtm";
const ESCAPING_COLOR: &str = "#0;x:1;";

struct Fixture {
    base: String,
    token: String,
    owner: String,
    repo: String,
    client: reqwest::Client,
}

impl Fixture {
    async fn new(owner: &str, repo: &str) -> Self {
        let base = spawn_test_app().await;
        let token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
        create_repo(&base, &token, repo).await;
        Self {
            base,
            token,
            owner: owner.to_string(),
            repo: repo.to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn api(&self, path: &str) -> String {
        format!(
            "{}/api/v1/repos/{}/{}{}",
            self.base, self.owner, self.repo, path
        )
    }

    async fn create_board(&self, name: &str) -> i64 {
        let response = self
            .client
            .post(self.api("/boards"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .expect("create board");
        assert_eq!(response.status(), StatusCode::CREATED, "create board");
        response.json::<serde_json::Value>().await.expect("json")["id"]
            .as_i64()
            .expect("board id")
    }

    async fn board_full(&self, board_id: i64) -> serde_json::Value {
        let response = self
            .client
            .get(self.api(&format!("/boards/{board_id}")))
            .send()
            .await
            .expect("get board");
        assert_eq!(response.status(), StatusCode::OK, "get board");
        response.json().await.expect("json")
    }
}

/// The headline: what the old `starts_with('#') && len() == 7` rule let through
/// is refused on create, for both the column and the label route.
#[tokio::test]
async fn an_escaping_colour_is_refused_when_a_column_or_label_is_created() {
    let fixture = Fixture::new("color-create-owner", "color-create-repo").await;
    let board_id = fixture.create_board("Board").await;

    let column = fixture
        .client
        .post(fixture.api(&format!("/boards/{board_id}/columns")))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "Todo", "color": ESCAPING_COLOR }))
        .send()
        .await
        .expect("create column");
    let status = column.status();
    let body = column.text().await.unwrap_or_default();
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body.contains("hex string"),
        "the refusal must state the rule: {body}"
    );

    let label = fixture
        .client
        .post(fixture.api("/labels"))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "bug", "color": ESCAPING_COLOR }))
        .send()
        .await
        .expect("create label");
    let status = label.status();
    let body = label.text().await.unwrap_or_default();
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body.contains("hex string"),
        "the refusal must state the rule: {body}"
    );
}

/// The update half. A refused PATCH must leave the stored colour exactly as it
/// was — validation before the write, not a filter applied to the response.
#[tokio::test]
async fn an_escaping_colour_is_refused_on_update_and_changes_nothing() {
    let fixture = Fixture::new("color-update-owner", "color-update-repo").await;
    let board_id = fixture.create_board("Board").await;
    let board = fixture.board_full(board_id).await;
    let column_id = board["columns"][0]["column"]["id"]
        .as_i64()
        .expect("default column id");

    let refused = fixture
        .client
        .patch(fixture.api(&format!("/boards/{board_id}/columns/{column_id}")))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "color": ESCAPING_COLOR }))
        .send()
        .await
        .expect("update column");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    let label = fixture
        .client
        .post(fixture.api("/labels"))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "bug", "color": "#ff0000" }))
        .send()
        .await
        .expect("create label");
    assert_eq!(label.status(), StatusCode::CREATED);
    let label_id = label.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("label id");

    let refused = fixture
        .client
        .patch(fixture.api(&format!("/labels/{label_id}")))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "color": ESCAPING_COLOR }))
        .send()
        .await
        .expect("update label");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    let board = fixture.board_full(board_id).await;
    let stored = board["columns"][0]["column"]["color"]
        .as_str()
        .expect("default column colour");
    assert_ne!(stored, ESCAPING_COLOR, "the refused value was written");
    assert!(
        stored.starts_with('#') && stored.len() == 7,
        "the stored colour must keep its old shape, got {stored:?}"
    );
    let labels = fixture
        .client
        .get(fixture.api("/labels"))
        .bearer_auth(&fixture.token)
        .send()
        .await
        .expect("list labels");
    let labels: serde_json::Value = labels.json().await.expect("json");
    assert_eq!(
        labels[0]["color"], "#ff0000",
        "the refused label colour was written: {labels}"
    );
}

/// A valid colour still passes, and the stored value is the normalised
/// lowercase form — the rule is one shape, not "anything that looks like it".
#[tokio::test]
async fn a_valid_colour_is_stored_lowercase_on_both_routes() {
    let fixture = Fixture::new("color-valid-owner", "color-valid-repo").await;
    let board_id = fixture.create_board("Board").await;

    let column = fixture
        .client
        .post(fixture.api(&format!("/boards/{board_id}/columns")))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "Review", "color": "#AABBCC" }))
        .send()
        .await
        .expect("create column");
    assert_eq!(column.status(), StatusCode::CREATED);
    let column: serde_json::Value = column.json().await.expect("json");
    assert_eq!(column["color"], "#aabbcc");

    let label = fixture
        .client
        .post(fixture.api("/labels"))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "bug", "color": "#FF0000" }))
        .send()
        .await
        .expect("create label");
    assert_eq!(label.status(), StatusCode::CREATED);
    let label: serde_json::Value = label.json().await.expect("json");
    assert_eq!(label["color"], "#ff0000");

    let updated = fixture
        .client
        .patch(fixture.api(&format!("/labels/{}", label["id"])))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "color": "#00FF00" }))
        .send()
        .await
        .expect("update label");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: serde_json::Value = updated.json().await.expect("json");
    assert_eq!(updated["color"], "#00ff00");
}
