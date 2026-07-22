//! GitHub REST API v3 client for data migration import.
//!
//! Provides typed API calls to fetch repository data (issues, PRs,
//! labels, milestones, releases, wiki) from GitHub.com or GitHub
//! Enterprise Server instances.

use anyhow::{Context, Result};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};

/// GitHub API client.
pub struct GitHubClient {
    client: Client,
    base_url: String,
    #[allow(dead_code)]
    token: String,
}

/// Repository metadata from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRepo {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub html_url: String,
    pub clone_url: String,
    pub owner: GitHubUser,
}

/// User from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub id: i64,
    pub login: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    #[serde(rename = "type")]
    pub user_type: Option<String>,
}

/// Issue from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub labels: Vec<GitHubLabel>,
    pub milestone: Option<GitHubMilestone>,
    pub user: Option<GitHubUser>,
    pub assignees: Vec<GitHubUser>,
    pub comments: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub pull_request: Option<serde_json::Value>, // present if issue is a PR
}

/// Pull Request from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPR {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub merged: Option<bool>,
    pub merged_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    pub user: Option<GitHubUser>,
    pub head: GitHubRef,
    pub base: GitHubRef,
    pub labels: Vec<GitHubLabel>,
    pub milestone: Option<GitHubMilestone>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Git reference in a PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
    pub label: Option<String>,
    pub repo: Option<GitHubRepo>,
}

/// Label from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

/// Milestone from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubMilestone {
    pub number: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub due_on: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Release from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRelease {
    pub id: i64,
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub created_at: String,
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

/// Release asset from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubAsset {
    pub id: i64,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub download_count: i64,
    pub browser_download_url: String,
    pub created_at: String,
}

/// Issue/PR comment from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubComment {
    pub id: i64,
    pub body: Option<String>,
    pub user: Option<GitHubUser>,
    pub created_at: String,
    pub updated_at: String,
}

/// Pull Request review from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubReview {
    pub id: i64,
    pub user: Option<GitHubUser>,
    pub state: String, // "APPROVED", "CHANGES_REQUESTED", "COMMENTED"
    pub body: Option<String>,
    pub submitted_at: Option<String>,
}

impl GitHubClient {
    /// Create a new GitHub API client.
    ///
    /// `base_url` should be `https://api.github.com` for GitHub.com
    /// or `https://<hostname>/api/v3` for GitHub Enterprise Server.
    pub fn new(token: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| "https://api.github.com".to_string());
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}")).expect("invalid token"),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github.v3+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            header::HeaderValue::from_static("2022-11-28"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .user_agent("IronForge/0.1")
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            base_url: base,
            token,
        }
    }

    /// Get repository metadata.
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<GitHubRepo> {
        let url = format!("{}/repos/{}/{}", self.base_url, owner, repo);
        let resp = self.client.get(&url).send().await.context("get repo")?;
        Self::handle_response(resp).await
    }

    /// List all labels for a repository.
    pub async fn list_labels(&self, owner: &str, repo: &str) -> Result<Vec<GitHubLabel>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/labels?per_page=100",
                self.base_url, owner, repo
            ),
        )
        .await
    }

    /// List milestones for a repository.
    pub async fn list_milestones(&self, owner: &str, repo: &str) -> Result<Vec<GitHubMilestone>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/milestones?state=all&per_page=100",
                self.base_url, owner, repo
            ),
        )
        .await
    }

    /// List all issues (excluding pull requests) for a repository.
    pub async fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<GitHubIssue>> {
        // GitHub's /issues endpoint returns both issues and PRs.
        // We filter out PRs on our side since PRs are fetched separately.
        let raw: Vec<GitHubIssue> = Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/issues?state=all&per_page=100",
                self.base_url, owner, repo
            ),
        )
        .await?;
        // Filter out pull requests (they have a pull_request field)
        Ok(raw
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .collect())
    }

    /// List pull requests for a repository.
    pub async fn list_pull_requests(&self, owner: &str, repo: &str) -> Result<Vec<GitHubPR>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/pulls?state=all&per_page=100",
                self.base_url, owner, repo
            ),
        )
        .await
    }

    /// List comments for an issue.
    pub async fn list_issue_comments(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
    ) -> Result<Vec<GitHubComment>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/issues/{}/comments?per_page=100",
                self.base_url, owner, repo, issue_number
            ),
        )
        .await
    }

    /// List reviews for a pull request.
    pub async fn list_pr_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<GitHubReview>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/pulls/{}/reviews?per_page=100",
                self.base_url, owner, repo, pr_number
            ),
        )
        .await
    }

    /// List releases for a repository.
    pub async fn list_releases(&self, owner: &str, repo: &str) -> Result<Vec<GitHubRelease>> {
        Self::paginate_all(
            &self.client,
            &format!(
                "{}/repos/{}/{}/releases?per_page=100",
                self.base_url, owner, repo
            ),
        )
        .await
    }

    /// Check if the repository has a wiki.
    pub async fn has_wiki(&self, owner: &str, repo: &str) -> Result<bool> {
        let url = format!("{}/repos/{}/{}", self.base_url, owner, repo);
        let repo_meta: serde_json::Value = self.client.get(&url).send().await?.json().await?;
        Ok(repo_meta["has_wiki"].as_bool().unwrap_or(false))
    }

    // ── helpers ─────────────────────────────────────────────────────────

    /// Handle a response, returning the parsed body or an error.
    async fn handle_response<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            resp.json().await.context("parse response body")
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error ({}): {}", status, body)
        }
    }

    /// Fetch all pages of a paginated GitHub API endpoint.
    async fn paginate_all<T: serde::de::DeserializeOwned>(
        client: &Client,
        initial_url: &str,
    ) -> Result<Vec<T>> {
        let mut results = Vec::new();
        let mut url = initial_url.to_string();

        loop {
            let resp = client.get(&url).send().await?;
            let status = resp.status();

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("GitHub API error ({}): {}", status, body);
            }

            // Extract Link header BEFORE consuming resp
            let link_header = resp
                .headers()
                .get(header::LINK)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            let has_next = link_header.contains("rel=\"next\"");
            let next_url = if has_next {
                extract_next_link(&link_header)
            } else {
                None
            };

            let page: Vec<T> = resp.json().await?;
            results.extend(page);

            if !has_next {
                break;
            }

            url = next_url.ok_or_else(|| anyhow::anyhow!("next page link not found"))?;
        }

        Ok(results)
    }
}

/// Extract the `rel="next"` URL from a GitHub Link header.
fn extract_next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let trimmed = part.trim();
        if trimmed.contains("rel=\"next\"") {
            if let Some(start) = trimmed.find('<') {
                if let Some(end) = trimmed.find('>') {
                    return Some(trimmed[start + 1..end].to_string());
                }
            }
        }
    }
    None
}
