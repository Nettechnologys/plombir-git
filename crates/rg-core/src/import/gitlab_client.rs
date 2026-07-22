//! GitLab REST API v4 client for data migration import.
//!
//! Provides typed API calls to fetch repository data (issues, MRs,
//! labels, milestones, releases, wiki) from GitLab.com or self-hosted
//! GitLab instances.

use anyhow::{Context, Result};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};

/// GitLab API client.
pub struct GitLabClient {
    client: Client,
    base_url: String,
}

/// Project metadata from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabProject {
    pub id: i64,
    pub name: String,
    pub path_with_namespace: String,
    pub description: Option<String>,
    pub visibility: String,
    pub default_branch: String,
    pub web_url: String,
    pub http_url_to_repo: String,
    pub owner: Option<GitLabUser>,
    pub namespace: GitLabNamespace,
}

/// Namespace from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNamespace {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub kind: String, // "user" or "group"
}

/// User from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabUser {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

/// Issue from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabIssue {
    pub id: i64,
    pub iid: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub labels: Vec<String>, // GitLab returns labels as strings
    pub milestone: Option<GitLabMilestone>,
    pub author: Option<GitLabUser>,
    pub assignees: Vec<GitLabUser>,
    pub user_notes_count: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merge_request_count: Option<i64>,
    pub has_tasks: Option<bool>,
}

/// Merge Request from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMR {
    pub id: i64,
    pub iid: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub merged_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    pub author: Option<GitLabUser>,
    pub source_branch: String,
    pub target_branch: String,
    pub source_project_id: Option<i64>,
    pub target_project_id: i64,
    pub labels: Vec<String>,
    pub milestone: Option<GitLabMilestone>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Milestone from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMilestone {
    pub id: i64,
    pub iid: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub due_date: Option<String>,
    pub start_date: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Label from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabLabel {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub description: Option<String>,
    pub text_color: Option<String>,
}

/// Release from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub released_at: Option<String>,
    pub assets: GitLabReleaseAssets,
}

/// Release assets from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabReleaseAssets {
    pub count: i64,
    #[serde(default)]
    pub sources: Vec<GitLabReleaseSource>,
    #[serde(default)]
    pub links: Vec<GitLabReleaseLink>,
}

/// Release source from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabReleaseSource {
    pub format: String,
    pub url: String,
}

/// Release link from GitLab API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabReleaseLink {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub link_type: String,
}

/// Note (comment) from GitLab API (for issues and MRs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNote {
    pub id: i64,
    pub body: Option<String>,
    pub author: Option<GitLabUser>,
    pub system: bool, // system-generated note (e.g., "closed this issue")
    pub created_at: String,
    pub updated_at: String,
}

impl GitLabClient {
    /// Create a new GitLab API client.
    ///
    /// `base_url` should be `https://gitlab.com/api/v4` for GitLab.com
    /// or `https://<hostname>/api/v4` for self-hosted instances.
    pub fn new(token: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| "https://gitlab.com/api/v4".to_string());
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "PRIVATE-TOKEN",
            header::HeaderValue::from_str(&token).expect("invalid token"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .user_agent("IronForge/0.1")
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            base_url: base,
        }
    }

    /// Get project metadata.
    pub async fn get_project(&self, project_id: &str) -> Result<GitLabProject> {
        // project_id can be integer ID or URL-encoded path (e.g., "group%2Fproject")
        let url = format!("{}/projects/{}", self.base_url, urlencoding(project_id));
        let resp = self.client.get(&url).send().await.context("get project")?;
        Self::handle_response(resp).await
    }

    /// List labels for a project.
    pub async fn list_labels(&self, project_id: &str) -> Result<Vec<GitLabLabel>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/labels?per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    /// List milestones for a project.
    pub async fn list_milestones(&self, project_id: &str) -> Result<Vec<GitLabMilestone>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/milestones?state=all&per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    /// List issues for a project.
    pub async fn list_issues(&self, project_id: &str) -> Result<Vec<GitLabIssue>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/issues?state=all&scope=all&per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    /// List merge requests for a project.
    pub async fn list_merge_requests(&self, project_id: &str) -> Result<Vec<GitLabMR>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/merge_requests?state=all&scope=all&per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    /// List notes (comments) for an issue.
    pub async fn list_issue_notes(
        &self,
        project_id: &str,
        issue_iid: i64,
    ) -> Result<Vec<GitLabNote>> {
        // Filter out system notes for cleaner import
        let all_notes: Vec<GitLabNote> = Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/issues/{}/notes?per_page=100",
                self.base_url,
                urlencoding(project_id),
                issue_iid
            ),
        )
        .await?;
        Ok(all_notes.into_iter().filter(|n| !n.system).collect())
    }

    /// List notes (comments) for a merge request.
    pub async fn list_mr_notes(&self, project_id: &str, mr_iid: i64) -> Result<Vec<GitLabNote>> {
        let all_notes: Vec<GitLabNote> = Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/merge_requests/{}/notes?per_page=100",
                self.base_url,
                urlencoding(project_id),
                mr_iid
            ),
        )
        .await?;
        Ok(all_notes.into_iter().filter(|n| !n.system).collect())
    }

    /// List releases for a project.
    pub async fn list_releases(&self, project_id: &str) -> Result<Vec<GitLabRelease>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/releases?per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    /// List all project members (for user mapping).
    pub async fn list_members(&self, project_id: &str) -> Result<Vec<GitLabUser>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/projects/{}/members/all?per_page=100",
                self.base_url,
                urlencoding(project_id)
            ),
        )
        .await
    }

    // ── helpers ─────────────────────────────────────────────────────────

    async fn handle_response<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            resp.json().await.context("parse response body")
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab API error ({}): {}", status, body)
        }
    }

    /// Fetch all pages of a paginated GitLab API endpoint.
    async fn paginate_all<T: serde::de::DeserializeOwned>(
        client: &Client,
        initial_url: &str,
    ) -> Result<Vec<T>> {
        let mut results = Vec::new();
        let mut page = 1;

        loop {
            let url = if initial_url.contains('?') {
                format!("{}&page={}", initial_url, page)
            } else {
                format!("{}?page={}", initial_url, page)
            };

            let resp = client.get(&url).send().await?;
            let status = resp.status();

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("GitLab API error ({}): {}", status, body);
            }

            // Extract pagination header BEFORE consuming resp
            let total_pages: i64 = resp
                .headers()
                .get("x-total-pages")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);

            let page_data: Vec<T> = resp.json().await?;
            let is_last = page >= total_pages || page_data.is_empty();
            results.extend(page_data);

            if is_last {
                break;
            }
            page += 1;
        }

        Ok(results)
    }
}

/// URL-encode a project identifier (e.g., "group/project" → "group%2Fproject").
fn urlencoding(s: &str) -> String {
    if s.parse::<i64>().is_ok() {
        // Numeric ID, no encoding needed
        s.to_string()
    } else {
        // Path encoding: replace '/' with '%2F'
        s.replace('/', "%2F")
    }
}
