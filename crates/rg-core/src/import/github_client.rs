//! GitHub REST API v3 client for data migration import.
//!
//! Provides typed API calls to fetch repository data (issues, PRs,
//! labels, milestones, releases) from GitHub.com or GitHub Enterprise Server
//! instances.
//!
//! A repository's wiki is deliberately absent: GitHub serves no page content
//! over this API, only the `has_wiki` feature flag — which is on by default and
//! says nothing about whether any page was ever written. The wiki is a second
//! git repository, and `import::service::import_wiki_pages` clones it.

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
    /// `base_url` must be `https://api.github.com` for GitHub.com
    /// or `https://<hostname>/api/v3` for GitHub Enterprise Server. Requiring
    /// it keeps the source repository and the authenticated API host coupled;
    /// the import service derives it from the source URL.
    pub fn new(token: String, base_url: String) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid import auth token: not a valid HTTP header value")?,
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github.v3+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            header::HeaderValue::from_static("2022-11-28"),
        );

        // Reuse the shared outbound builder so the import client inherits the
        // request + connect timeout — a slow/hanging import source (e.g. a
        // self-hosted GHES `base_url`) can't pin the import worker forever.
        // Same-origin redirects remain enabled (API hosts legitimately 3xx on
        // a renamed repo), but a redirect may not move this credential to a
        // different scheme, host, or port.
        let client = crate::net::outbound_client_builder()
            .default_headers(headers)
            .redirect(super::trust::same_origin_redirect_policy(&base_url)?)
            .user_agent("ForgeKeep/0.1")
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self {
            client,
            base_url,
            token,
        })
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

#[cfg(test)]
mod redirect_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, headers: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    }

    #[tokio::test]
    async fn cross_origin_redirect_never_receives_the_bearer_token() {
        let sink = TcpListener::bind("127.0.0.1:0").await.expect("bind sink");
        let sink_addr = sink.local_addr().expect("sink address");
        let source = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let source_addr = source.local_addr().expect("source address");

        let sink_task = tokio::spawn(async move {
            match tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await {
                Ok(Ok((mut stream, _))) => Some(read_headers(&mut stream).await),
                _ => None,
            }
        });
        let source_task = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.expect("accept source request");
            let request = read_headers(&mut stream).await;
            write_response(
                &mut stream,
                "302 Found",
                &format!("Location: http://{sink_addr}/capture\r\n"),
            )
            .await;
            request
        });

        let client = GitHubClient::new(
            "private-import-token".to_owned(),
            format!("http://{source_addr}"),
        )
        .expect("build client");
        let error = client
            .get_repo("team", "widgets")
            .await
            .expect_err("a blocked redirect remains a 302 API response");
        assert!(format!("{error:#}").contains("302"));

        let source_request = source_task.await.expect("source task");
        assert!(
            source_request.contains("Authorization: Bearer private-import-token")
                || source_request.contains("authorization: Bearer private-import-token"),
            "baseline: the configured API origin did not receive its token: {source_request}"
        );
        assert!(
            sink_task.await.expect("sink task").is_none(),
            "the cross-origin redirect was followed and could receive the PAT"
        );
    }
}
