//! Authorization regressions for the webhook endpoints.
//!
//! All seven verbs under `.../hooks` used to stop at
//! `extract_user_id(...).is_none()` and drop the result into `let _user_id`.
//! That is authentication, not authorization: any account with a valid token
//! could list, read, rewrite and delete the webhooks of any repository —
//! private ones included — and the reply serialized the `webhooks` row
//! wholesale, so it handed over the `secret` too. With that secret an outsider
//! can forge `X-Hub-Signature-256` on deliveries; with the write verbs it can
//! point a private repo's future push/issue events at a host of its choosing.
//!
//! Webhooks are repository *administration*, like deploy keys and CI secrets,
//! so the gate is `require_admin` — on the reads as well: the target URL of a
//! delivery is configuration, not repository content.

use crate::common::{create_repo, register_full, spawn_test_app};

// ── helpers ──────────────────────────────────────────────────────────────────

const HOOK_URL: &str = "https://hooks.example.com/incoming";
const HOOK_SECRET: &str = "s3cr3t-hmac-key";

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

async fn add_collaborator(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    username: &str,
    permission: &str,
) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": permission}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_collaborator '{username}' failed");
}

/// Register a webhook through a token that is allowed to, and return its id.
async fn create_hook(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/hooks"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "url": HOOK_URL,
            "secret": HOOK_SECRET,
            "events": ["push"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_hook failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Every request the webhook surface accepts, in one list, so a new verb cannot
/// quietly join the router without an authorization test.
///
/// `token = None` is the anonymous caller.
async fn probe_all_seven(
    base: &str,
    token: Option<&str>,
    owner: &str,
    repo: &str,
    hook_id: i64,
    expected: u16,
) {
    let client = reqwest::Client::new();
    let hooks = format!("{base}/api/v1/repos/{owner}/{repo}/hooks");
    let one = format!("{hooks}/{hook_id}");
    // A *well-formed* body on purpose: axum runs the `Json` extractor before the
    // handler body, so a malformed one would answer 422 and never reach the
    // authorization gate this probe is about.
    let body = serde_json::json!({"url": "https://evil.example.com/exfil", "events": ["push"]});

    let requests = vec![
        ("GET   /hooks", client.get(&hooks)),
        ("POST  /hooks", client.post(&hooks).json(&body)),
        ("GET   /hooks/{id}", client.get(&one)),
        ("PATCH /hooks/{id}", client.patch(&one).json(&body)),
        // The two rows below spell their route out beside the verb instead of
        // reusing `one`. `docs/ui-inventory.json` can only credit a route a test
        // writes as one string next to the method it sends, and a URL assembled
        // from a variable reads there as a route no test touches at all
        // (card_d482cf7e098e).
        (
            "DEL   /hooks/{id}",
            client.delete(format!(
                "{base}/api/v1/repos/{owner}/{repo}/hooks/{hook_id}"
            )),
        ),
        (
            "GET   /hooks/{id}/deliveries",
            client.get(format!(
                "{base}/api/v1/repos/{owner}/{repo}/hooks/{hook_id}/deliveries"
            )),
        ),
        (
            "POST  /hooks/{id}/deliveries/{d}/redeliver",
            client.post(format!("{one}/deliveries/1/redeliver")),
        ),
    ];

    for (label, request) in requests {
        let request = match token {
            Some(t) => request.bearer_auth(t),
            None => request,
        };
        let resp = request.send().await.unwrap();
        assert_eq!(
            resp.status(),
            expected,
            "{label} answered {} instead of {expected}",
            resp.status()
        );
    }
}

// ── 1. authenticated is not authorized ───────────────────────────────────────

#[tokio::test]
async fn anonymous_is_rejected_on_every_webhook_endpoint() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whanon_owner", "whanon_o@e.com").await;
    create_repo(&base, &owner_token, "proj").await;
    let hook_id = create_hook(&base, &owner_token, "whanon_owner", "proj").await;

    probe_all_seven(&base, None, "whanon_owner", "proj", hook_id, 401).await;
}

/// The leak in its shortest form: a logged-in outsider reading (and rewriting)
/// the webhooks of a private repository it has nothing to do with.
#[tokio::test]
async fn outsider_is_rejected_on_every_webhook_endpoint() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whout_owner", "whout_o@e.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "whout_outsider", "whout_x@e.com").await;
    create_private_repo(&base, &owner_token, "secret").await;
    let hook_id = create_hook(&base, &owner_token, "whout_owner", "secret").await;

    probe_all_seven(
        &base,
        Some(&outsider_token),
        "whout_owner",
        "secret",
        hook_id,
        403,
    )
    .await;

    // And the refusal is real, not just a status code: the hook is untouched.
    let hooks: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/whout_owner/secret/hooks"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hooks.len(), 1, "the webhook list was modified");
    assert_eq!(
        hooks[0]["url"], HOOK_URL,
        "the webhook target was rewritten"
    );
}

/// A public repository is not an exception: the delivery target and the fact a
/// signing key exists are configuration, and reading them is still admin-only.
#[tokio::test]
async fn outsider_cannot_read_webhooks_of_a_public_repo() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whpub_owner", "whpub_o@e.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "whpub_outsider", "whpub_x@e.com").await;
    create_repo(&base, &owner_token, "open").await;
    create_hook(&base, &owner_token, "whpub_owner", "open").await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/whpub_owner/open/hooks"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "any logged-in user read the webhook configuration of a public repo"
    );
}

/// Write access to the *contents* of a repository is not the right to
/// reconfigure where its events are shipped. Same boundary the collaborator
/// endpoints draw.
#[tokio::test]
async fn write_collaborator_cannot_manage_webhooks() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whwri_owner", "whwri_o@e.com").await;
    let (writer_token, _writer_id) = register_full(&base, "whwri_writer", "whwri_w@e.com").await;
    create_repo(&base, &owner_token, "proj").await;
    add_collaborator(
        &base,
        &owner_token,
        "whwri_owner",
        "proj",
        "whwri_writer",
        "write",
    )
    .await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/whwri_owner/proj/hooks"))
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({"url": HOOK_URL, "events": ["push"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "a write collaborator registered a webhook"
    );
}

// ── 2. the secret never leaves the server ────────────────────────────────────

/// Even the owner does not get the raw HMAC key back — knowing one is stored is
/// all the settings form needs, and a reply that carries it turns every log,
/// proxy and browser cache into a copy of the signing key.
#[tokio::test]
async fn the_hmac_secret_is_never_returned() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whsec_owner", "whsec_o@e.com").await;
    create_repo(&base, &owner_token, "proj").await;

    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos/whsec_owner/proj/hooks"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "url": HOOK_URL,
            "secret": HOOK_SECRET,
            "events": ["push"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created_body = created.text().await.unwrap();
    assert!(
        !created_body.contains(HOOK_SECRET),
        "POST /hooks echoed the secret back: {created_body}"
    );
    let created_json: serde_json::Value = serde_json::from_str(&created_body).unwrap();
    assert_eq!(created_json["has_secret"], true);
    assert!(created_json.get("secret").is_none());
    let hook_id = created_json["id"].as_i64().unwrap();

    for path in [
        format!("{base}/api/v1/repos/whsec_owner/proj/hooks"),
        format!("{base}/api/v1/repos/whsec_owner/proj/hooks/{hook_id}"),
    ] {
        let body = client
            .get(&path)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            !body.contains(HOOK_SECRET),
            "{path} leaked the secret: {body}"
        );
    }

    // A hook without a secret reports that honestly rather than claiming one.
    let plain = client
        .post(format!("{base}/api/v1/repos/whsec_owner/proj/hooks"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"url": HOOK_URL, "events": ["push"]}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(plain["has_secret"], false);
}

// ── 3. the id is global — scope it to the repo that was authorized ───────────

/// Being an admin of one repository is not being an admin of every repository
/// whose hook ids you can guess: `{id}` is a global `webhooks` primary key.
#[tokio::test]
async fn repo_admin_cannot_reach_another_repos_webhook() {
    let base = spawn_test_app().await;
    let (a_token, _a_id) = register_full(&base, "whx_a_owner", "whx_a@e.com").await;
    let (b_token, _b_id) = register_full(&base, "whx_b_owner", "whx_b@e.com").await;

    create_repo(&base, &a_token, "repo_a").await;
    create_private_repo(&base, &b_token, "repo_b").await;
    let victim_hook = create_hook(&base, &b_token, "whx_b_owner", "repo_b").await;

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/whx_a_owner/repo_a/hooks/{victim_hook}"
        ))
        .bearer_auth(&a_token)
        .json(&serde_json::json!({"url": "https://evil.example.com/exfil"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "the owner of repo A rewrote a webhook of repo B"
    );

    let hooks: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/whx_b_owner/repo_b/hooks"))
        .bearer_auth(&b_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hooks[0]["url"], HOOK_URL, "repo B's webhook was retargeted");
}

// ── 4. the legitimate paths still work ───────────────────────────────────────

#[tokio::test]
async fn owner_and_repo_admin_can_manage_webhooks() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whok_owner", "whok_o@e.com").await;
    let (admin_token, _admin_id) = register_full(&base, "whok_admin", "whok_a@e.com").await;
    create_repo(&base, &owner_token, "proj").await;
    add_collaborator(
        &base,
        &owner_token,
        "whok_owner",
        "proj",
        "whok_admin",
        "admin",
    )
    .await;

    // The promoted administrator runs the whole cycle on its own token.
    let hook_id = create_hook(&base, &admin_token, "whok_owner", "proj").await;
    let client = reqwest::Client::new();
    let one = format!("{base}/api/v1/repos/whok_owner/proj/hooks/{hook_id}");

    let get = client
        .get(&one)
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 200, "a repo admin could not read a webhook");

    let patch = client
        .patch(&one)
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        patch.status(),
        200,
        "a repo admin could not update a webhook"
    );
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert_eq!(patched["active"], false);
    assert_eq!(
        patched["has_secret"], true,
        "an update that did not mention the secret dropped it"
    );

    let deliveries = client
        .get(format!(
            "{base}/api/v1/repos/whok_owner/proj/hooks/{hook_id}/deliveries"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deliveries.status(), 200);

    let delete = client
        .delete(format!(
            "{base}/api/v1/repos/whok_owner/proj/hooks/{hook_id}"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        delete.status(),
        200,
        "a repo admin could not delete a webhook"
    );
}

/// A hook id that never existed and one that belongs elsewhere have to be the
/// same answer, otherwise the id space becomes an existence oracle.
#[tokio::test]
async fn unknown_webhook_is_not_found() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) = register_full(&base, "whnf_owner", "whnf_o@e.com").await;
    create_repo(&base, &owner_token, "proj").await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/whnf_owner/proj/hooks/424242"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["message"], "webhook not found");
}
