mod common;

use common::{create_repo, register_user, spawn_test_app};
use serde_json::{json, Value};

const PASSWORD: &str = "Qz7$wRtm";

async fn put_file(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
    content: &str,
) {
    let response = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&json!({
            "content": content,
            "message": format!("add {path}")
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "failed to create {path}: {}",
        response.text().await.unwrap()
    );
}

#[tokio::test]
async fn markdown_issue_and_pull_request_templates_are_discovered_from_default_branch() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("tplowner{}", &suffix[..8]);
    let repo = format!("templates{}", &suffix[..8]);
    let token = register_user(&base, &owner, &format!("{owner}@example.com"), PASSWORD).await;
    create_repo(&base, &token, &repo).await;

    put_file(
        &client,
        &base,
        &token,
        &owner,
        &repo,
        ".gitea/ISSUE_TEMPLATE/bug.md",
        "---\nname: Bug report\nabout: Report a reproducible problem\ntitle: '[Bug] '\nlabels: [bug, triage]\nassignees: maintainer\nref: main\n---\n\n## Steps to reproduce\n",
    )
    .await;
    put_file(
        &client,
        &base,
        &token,
        &owner,
        &repo,
        ".github/ISSUE_TEMPLATE/question.md",
        "# Question\n\nWhat do you need help with?\n",
    )
    .await;
    put_file(
        &client,
        &base,
        &token,
        &owner,
        &repo,
        ".gitea/ISSUE_TEMPLATE/config.yml",
        "blank_issues_enabled: false\ncontact_links:\n  - name: Support\n    url: https://example.com/support\n    about: Ask usage questions here\n",
    )
    .await;
    put_file(
        &client,
        &base,
        &token,
        &owner,
        &repo,
        ".github/PULL_REQUEST_TEMPLATE.md",
        "## Summary\n\nDescribe the change.\n\n## Checklist\n- [ ] Tests added\n",
    )
    .await;

    let templates_response = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issue_templates"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(templates_response.status(), reqwest::StatusCode::OK);
    let templates: Vec<Value> = templates_response.json().await.unwrap();
    assert_eq!(templates.len(), 2);

    let bug = templates
        .iter()
        .find(|template| template["name"] == "Bug report")
        .expect("bug template should be discovered");
    assert_eq!(bug["title"], "[Bug] ");
    assert_eq!(bug["labels"], json!(["bug", "triage"]));
    assert_eq!(bug["assignees"], json!(["maintainer"]));
    assert_eq!(bug["ref"], "main");
    assert!(bug["content"]
        .as_str()
        .unwrap()
        .contains("## Steps to reproduce"));

    let fallback = templates
        .iter()
        .find(|template| template["file_name"] == ".github/ISSUE_TEMPLATE/question.md")
        .expect("metadata-free Markdown should use filename fallback");
    assert_eq!(fallback["name"], "question.md");

    let config_response = client
        .get(format!("{base}/api/v1/repos/{owner}/{repo}/issue_config"))
        .send()
        .await
        .unwrap();
    assert_eq!(config_response.status(), reqwest::StatusCode::OK);
    let config: Value = config_response.json().await.unwrap();
    assert_eq!(config["blank_issues_enabled"], false);
    assert_eq!(config["contact_links"][0]["name"], "Support");
    assert_eq!(
        config["contact_links"][0]["url"],
        "https://example.com/support"
    );

    let validation: Value = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issue_config/validate"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(validation["valid"], true);

    let pull_template_response = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pull_request_template"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(pull_template_response.status(), reqwest::StatusCode::OK);
    let pull_template: Value = pull_template_response.json().await.unwrap();
    assert_eq!(
        pull_template["file_name"],
        ".github/PULL_REQUEST_TEMPLATE.md"
    );
    assert!(pull_template["content"]
        .as_str()
        .unwrap()
        .contains("## Checklist"));
}

#[tokio::test]
async fn private_repository_templates_require_read_access() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("privateowner{}", &suffix[..8]);
    let outsider = format!("outsider{}", &suffix[..8]);
    let repo = format!("private{}", &suffix[..8]);
    let owner_token = register_user(&base, &owner, &format!("{owner}@example.com"), PASSWORD).await;
    let outsider_token = register_user(
        &base,
        &outsider,
        &format!("{outsider}@example.com"),
        PASSWORD,
    )
    .await;

    let create_response = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&json!({"name": repo, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(create_response.status(), reqwest::StatusCode::CREATED);

    put_file(
        &client,
        &base,
        &owner_token,
        &owner,
        &repo,
        ".gitea/ISSUE_TEMPLATE/private.md",
        "---\nname: Private issue\nabout: Owners only\n---\nPrivate details\n",
    )
    .await;

    let anonymous = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issue_templates"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let forbidden = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issue_templates"
        ))
        .bearer_auth(outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let owner_response = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issue_templates"
        ))
        .bearer_auth(owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_response.status(), reqwest::StatusCode::OK);
}
