//! A bot's agent profile over HTTP (ADR-0001, card_d4d689acfa70): only the
//! bot's owner reads or writes it, the model key goes in and never comes out
//! — not in an answer, not in the audit log, not as plaintext in the database
//! — and the repository list takes only repositories the owner can read.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::common::{register_full, spawn_test_app_with_db};

const MODEL_KEY: &str = "sk-ant-profile-test-0123456789";

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    http: reqwest::Client,
    alice: String,
    bob: String,
    bot_id: i64,
}

async fn fixture() -> Fixture {
    let (base, db) = spawn_test_app_with_db().await;
    let http = reqwest::Client::new();
    let (alice, _) = register_full(&base, "alice", "alice@example.test").await;
    let (bob, _) = register_full(&base, "bob", "bob@example.test").await;
    for (token, name) in [(&alice, "app"), (&alice, "site"), (&bob, "secret")] {
        let response = http
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name, "is_private": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "create {name}");
    }
    let response = http
        .post(format!("{base}/api/v1/users/bots"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "username": "alice-agent" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let bot_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    Fixture {
        base,
        db,
        http,
        alice,
        bob,
        bot_id,
    }
}

impl Fixture {
    fn url(&self) -> String {
        format!("{}/api/v1/users/bots/alice-agent/agent", self.base)
    }

    async fn put(&self, token: &str, body: serde_json::Value) -> (u16, String) {
        let response = self
            .http
            .put(self.url())
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }

    async fn get(&self, token: &str) -> (u16, String) {
        let response = self
            .http
            .get(self.url())
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }

    async fn delete(&self, token: &str) -> u16 {
        self.http
            .delete(self.url())
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    async fn stored(&self) -> Option<rg_db::entities::agent_profile::Model> {
        rg_db::ops::agent_profile_ops::find_by_bot(&self.db, self.bot_id)
            .await
            .unwrap()
    }
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| panic!("not JSON: {text}"))
}

fn enabled_claude(repositories: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "enabled": true,
        "preset": "claude_code",
        "model_key": MODEL_KEY,
        "runner_tags": ["docker"],
        "repositories": repositories,
    })
}

#[tokio::test]
async fn the_owner_saves_a_profile_and_the_key_never_comes_back_out() {
    let f = fixture().await;
    let (status, body) = f.put(&f.alice, enabled_claude(&["alice/app"])).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        !body.contains(MODEL_KEY),
        "the answer carries the key: {body}"
    );
    let saved = json(&body);
    assert_eq!(saved["has_model_key"], true);
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["preset"], "claude_code");
    assert_eq!(saved["repositories"], serde_json::json!(["alice/app"]));
    assert_eq!(saved["runner_tags"], serde_json::json!(["docker"]));
    assert_eq!(saved["timeout_secs"], 1800);
    assert_eq!(
        saved["agents_enabled"], false,
        "the harness keeps [agents] off"
    );

    let (status, body) = f.get(&f.alice).await;
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains(MODEL_KEY));
    assert_eq!(json(&body)["has_model_key"], true);

    let sealed = f
        .stored()
        .await
        .and_then(|row| row.model_key_encrypted)
        .expect("the key is stored");
    assert!(
        !sealed.contains(MODEL_KEY),
        "the key is stored in plaintext"
    );

    let audit = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("user.update_bot_agent_profile"))
        .all(&f.db)
        .await
        .unwrap();
    assert_eq!(audit.len(), 1);
    let details = audit[0].details.clone().unwrap_or_default();
    assert!(
        !details.contains(MODEL_KEY),
        "the audit log carries the key"
    );
    assert!(details.contains("\"model_key\":\"set\""), "{details}");
}

#[tokio::test]
async fn an_omitted_key_is_kept_and_clearing_it_is_explicit() {
    let f = fixture().await;
    let (status, body) = f.put(&f.alice, enabled_claude(&["alice/app"])).await;
    assert_eq!(status, 200, "{body}");
    let before = f.stored().await.unwrap().model_key_encrypted;

    // The form that never shows the key saves without it.
    let (status, body) = f
        .put(
            &f.alice,
            serde_json::json!({ "enabled": true, "preset": "claude_code", "repositories": ["alice/site"] }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(f.stored().await.unwrap().model_key_encrypted, before);
    assert_eq!(
        json(&body)["repositories"],
        serde_json::json!(["alice/site"])
    );

    let (status, body) = f
        .put(
            &f.alice,
            serde_json::json!({ "preset": "claude_code", "model_key": MODEL_KEY, "clear_model_key": true }),
        )
        .await;
    assert_eq!(status, 400, "{body}");

    let (status, body) = f
        .put(
            &f.alice,
            serde_json::json!({ "preset": "claude_code", "clear_model_key": true }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["has_model_key"], false);
    assert_eq!(f.stored().await.unwrap().model_key_encrypted, None);

    // Without a key the preset cannot be switched on.
    let (status, body) = f
        .put(
            &f.alice,
            serde_json::json!({ "enabled": true, "preset": "claude_code" }),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("needs a model key"), "{body}");
}

#[tokio::test]
async fn somebody_elses_bot_is_answered_like_no_bot() {
    let f = fixture().await;
    let (status, _) = f.put(&f.alice, enabled_claude(&["alice/app"])).await;
    assert_eq!(status, 200);

    assert_eq!(f.get(&f.bob).await.0, 404);
    assert_eq!(f.put(&f.bob, enabled_claude(&[])).await.0, 404);
    assert_eq!(f.delete(&f.bob).await, 404);
    assert!(
        f.stored().await.is_some(),
        "an outsider removed the profile"
    );
}

#[tokio::test]
async fn only_repositories_the_owner_can_read_go_on_the_list() {
    let f = fixture().await;
    let (status, body) = f.put(&f.alice, enabled_claude(&["bob/secret"])).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("does not exist or is not visible"), "{body}");
    let (unknown_status, unknown_body) = f.put(&f.alice, enabled_claude(&["bob/nothing"])).await;
    assert_eq!(
        (
            unknown_status,
            json(&unknown_body)["error"]["message"].clone()
        ),
        (
            400,
            serde_json::json!("repository 'bob/nothing' does not exist or is not visible to you")
        ),
        "an invisible repository and a missing one must read the same"
    );
    let (status, body) = f.put(&f.alice, enabled_claude(&["app"])).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("owner/name"), "{body}");
    assert!(
        f.stored().await.is_none(),
        "a refused write left a row behind"
    );
}

#[tokio::test]
async fn a_personal_access_token_cannot_change_the_profile() {
    let f = fixture().await;
    let response = f
        .http
        .post(format!("{}/api/v1/users/tokens", f.base))
        .bearer_auth(&f.alice)
        .json(&serde_json::json!({ "name": "automation", "scopes": "user,repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let pat = response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = f.put(&pat, enabled_claude(&["alice/app"])).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("requires a login session"), "{body}");
    assert!(f.stored().await.is_none());
}

#[tokio::test]
async fn deleting_the_profile_or_the_bot_takes_the_key_with_it() {
    let f = fixture().await;
    assert_eq!(f.put(&f.alice, enabled_claude(&["alice/app"])).await.0, 200);
    assert_eq!(f.delete(&f.alice).await, 204);
    assert_eq!(f.get(&f.alice).await.0, 404);
    assert_eq!(f.delete(&f.alice).await, 404);

    assert_eq!(f.put(&f.alice, enabled_claude(&["alice/app"])).await.0, 200);
    let response = f
        .http
        .delete(format!("{}/api/v1/users/bots/alice-agent", f.base))
        .bearer_auth(&f.alice)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert!(
        f.stored().await.is_none(),
        "the bot's profile outlived the bot"
    );
    let rows = rg_db::entities::agent_profile_repository::Entity::find()
        .all(&f.db)
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "the bot's repository list outlived it: {rows:?}"
    );
}

#[tokio::test]
async fn the_instance_says_whether_agents_run_here() {
    let f = fixture().await;
    let response = f
        .http
        .get(format!("{}/api/v1/instance", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["agents_enabled"], false);
}
