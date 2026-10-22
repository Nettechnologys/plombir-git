//! GitHub REST API v3 client for data migration import.
//!
//! Provides typed API calls to fetch repository data (issues, PRs,
//! labels, milestones, releases) from GitHub.com or GitHub Enterprise Server
//! instances — and from Gitea and Forgejo, whose `/api/v1` answers the same
//! calls in the same shapes, with the differences [`ApiDialect`] names
//! (card_64ae115d5bd6).
//!
//! A repository's wiki is deliberately absent: GitHub serves no page content
//! over this API, only the `has_wiki` feature flag — which is on by default and
//! says nothing about whether any page was ever written. The wiki is a second
//! git repository, and `import::service::import_wiki_pages` clones it.

use anyhow::{Context, Result};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};

/// GitHub API client.
/// Which server answers: the calls and response shapes are shared, the
/// spelling of a few of them is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiDialect {
    /// GitHub.com and GitHub Enterprise Server.
    GitHub,
    /// Gitea and Forgejo: `?limit=` instead of `?per_page=`, a page of at most
    /// 50 items by default (`[api] MAX_RESPONSE_ITEMS`), no `sort` on the
    /// issue and pull request lists, and `Authorization: token …`.
    Gitea,
}

impl ApiDialect {
    /// The query parameter asking for a full page.
    fn page_size(self) -> &'static str {
        match self {
            Self::GitHub => "per_page=100",
            Self::Gitea => "limit=50",
        }
    }

    /// Oldest first where the server lets the order be asked for; Gitea lists
    /// in its own order, which is harmless now that every object keeps its
    /// source number.
    fn oldest_first(self) -> &'static str {
        match self {
            Self::GitHub => "&sort=created&direction=asc",
            Self::Gitea => "",
        }
    }
}

/// A list field Gitea sends as `null` when it is empty — `assignees` above all,
/// which its converter fills only when there is one (`convert::ToAPIIssue`).
fn null_as_empty<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

pub struct GitHubClient {
    client: Client,
    base_url: String,
    dialect: ApiDialect,
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
    #[serde(default, deserialize_with = "null_as_empty")]
    pub labels: Vec<GitHubLabel>,
    pub milestone: Option<GitHubMilestone>,
    pub user: Option<GitHubUser>,
    #[serde(default, deserialize_with = "null_as_empty")]
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
    #[serde(default, deserialize_with = "null_as_empty")]
    pub labels: Vec<GitHubLabel>,
    pub milestone: Option<GitHubMilestone>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub assignees: Vec<GitHubUser>,
    /// The people whose review is still asked for; one who has reviewed is
    /// a review, not a request.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub requested_reviewers: Vec<GitHubUser>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// The commit a merged pull request became on its base: the merge commit,
    /// the squash, or the last rebased commit. For one that is not merged GitHub
    /// fills it with a test merge nobody made, which is why the import reads it
    /// only when `merged` is true.
    #[serde(default)]
    pub merge_commit_sha: Option<String>,
    /// Gitea's record of where the pull request branched off its base. Its
    /// `base.sha` is the base branch's tip *now*, which after a merge holds
    /// the head — so for a diff this is the base to read from. GitHub sends no
    /// such field.
    #[serde(default)]
    pub merge_base: Option<String>,
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
    /// GitHub's milestone number; Gitea has only the `id`.
    #[serde(alias = "id")]
    pub number: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub due_on: Option<String>,
    pub created_at: String,
    /// `null` on a Gitea milestone that was never edited.
    #[serde(default)]
    pub updated_at: Option<String>,
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
    /// Who published the release on GitHub.
    #[serde(default)]
    pub author: Option<GitHubUser>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub assets: Vec<GitHubAsset>,
}

/// Release asset from GitHub API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubAsset {
    pub id: i64,
    pub name: String,
    /// Absent from a Gitea attachment.
    #[serde(default)]
    pub content_type: String,
    pub size: i64,
    pub download_count: i64,
    pub browser_download_url: String,
    pub created_at: String,
    /// `sha256:<hex>` of the file, which GitHub lists for an asset; absent from
    /// older GitHub answers and from Gitea. Checked against what is downloaded
    /// when present.
    #[serde(default)]
    pub digest: Option<String>,
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

/// A comment on a line of a pull request's diff (card_f6c67a05c6b0).
///
/// GitHub (`/pulls/{n}/comments`) and Gitea (`/pulls/{n}/reviews/{id}/comments`)
/// answer different shapes for the same thing, so every field either one
/// leaves out defaults: GitHub places a comment by `line` / `side` on
/// `commit_id` — or, once outdated, by `original_line` on `original_commit_id`
/// — while Gitea sends the new-file line as `position` and the old-file one as
/// `original_position`, the other of the pair `0`. [`Self::placement`] reads
/// either.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubReviewComment {
    pub id: i64,
    #[serde(default)]
    pub pull_request_review_id: Option<i64>,
    pub user: Option<GitHubUser>,
    pub body: Option<String>,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub original_line: Option<i64>,
    #[serde(default)]
    pub start_line: Option<i64>,
    #[serde(default)]
    pub original_start_line: Option<i64>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub start_side: Option<String>,
    #[serde(default)]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub original_commit_id: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default)]
    pub original_position: Option<i64>,
    #[serde(default)]
    pub in_reply_to_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Where a [`GitHubReviewComment`] sits: the line it is on, which side of the
/// diff, the start of a multi-line range, and the commit the line is counted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlinePlacement {
    pub line: Option<i64>,
    pub side: &'static str,
    pub start_line: Option<i64>,
    pub start_side: Option<&'static str>,
    pub commit_id: Option<String>,
}

fn diff_side(side: Option<&str>) -> &'static str {
    match side {
        Some("LEFT") => "LEFT",
        _ => "RIGHT",
    }
}

impl GitHubReviewComment {
    /// Where the comment sits, in either dialect's spelling.
    pub fn placement(&self, dialect: ApiDialect) -> InlinePlacement {
        let commit = |sha: &Option<String>| sha.clone().filter(|sha| !sha.is_empty());
        match dialect {
            ApiDialect::GitHub => {
                // A comment whose line is gone from the current diff keeps the
                // line it was written on, in the commit it was written on.
                let current = self.line.is_some();
                InlinePlacement {
                    line: self.line.or(self.original_line),
                    side: diff_side(self.side.as_deref()),
                    start_line: if current {
                        self.start_line
                    } else {
                        self.original_start_line
                    },
                    start_side: self.start_side.as_deref().map(|side| diff_side(Some(side))),
                    commit_id: if current {
                        commit(&self.commit_id)
                    } else {
                        commit(&self.original_commit_id).or_else(|| commit(&self.commit_id))
                    },
                }
            }
            ApiDialect::Gitea => {
                let new_line = self.position.filter(|line| *line > 0);
                let old_line = self.original_position.filter(|line| *line > 0);
                InlinePlacement {
                    line: new_line.or(old_line),
                    side: if new_line.is_none() && old_line.is_some() {
                        "LEFT"
                    } else {
                        "RIGHT"
                    },
                    start_line: None,
                    start_side: None,
                    commit_id: commit(&self.commit_id).or_else(|| commit(&self.original_commit_id)),
                }
            }
        }
    }
}

impl GitHubClient {
    /// Create a new GitHub API client.
    ///
    /// `destination` owns both the API base URL and the reachability-aware
    /// builder issued for that exact URL by [`super::trust::TrustedImportOrigins`].
    /// This makes it impossible to validate one hostname and connect the PAT
    /// client through a fresh resolver for another.
    pub fn new(token: String, destination: super::trust::ImportApiDestination) -> Result<Self> {
        Self::with_dialect(token, destination, ApiDialect::GitHub)
    }

    /// A client for a Gitea or Forgejo server's `/api/v1`.
    pub fn new_gitea(
        token: String,
        destination: super::trust::ImportApiDestination,
    ) -> Result<Self> {
        Self::with_dialect(token, destination, ApiDialect::Gitea)
    }

    fn with_dialect(
        token: String,
        destination: super::trust::ImportApiDestination,
        dialect: ApiDialect,
    ) -> Result<Self> {
        let (base_url, builder) = destination.into_parts();
        let mut headers = header::HeaderMap::new();
        // Gitea documents `token …` for an access token; GitHub `Bearer …`.
        let scheme = match dialect {
            ApiDialect::GitHub => "Bearer",
            ApiDialect::Gitea => "token",
        };
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("{scheme} {token}"))
                .context("invalid import auth token: not a valid HTTP header value")?,
        );
        match dialect {
            ApiDialect::GitHub => {
                headers.insert(
                    header::ACCEPT,
                    header::HeaderValue::from_static("application/vnd.github.v3+json"),
                );
                headers.insert(
                    "X-GitHub-Api-Version",
                    header::HeaderValue::from_static("2022-11-28"),
                );
            }
            ApiDialect::Gitea => {
                headers.insert(
                    header::ACCEPT,
                    header::HeaderValue::from_static("application/json"),
                );
            }
        }

        // Reuse the shared outbound builder so the import client inherits the
        // request + connect timeout — a slow/hanging import source (e.g. a
        // self-hosted GHES `base_url`) can't pin the import worker forever.
        // Same-origin redirects remain enabled (API hosts legitimately 3xx on
        // a renamed repo), but a redirect may not move this credential to a
        // different scheme, host, or port.
        let client = builder
            .default_headers(headers)
            .redirect(crate::net::same_origin_redirect_policy())
            .user_agent("PlombirGit/0.1")
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self {
            client,
            base_url,
            dialect,
            token,
        })
    }

    #[cfg(test)]
    fn new_for_trusted_test(token: String, base_url: String) -> Result<Self> {
        let destination =
            super::trust::TrustedImportOrigins::trusted_api_destination_for_test(&base_url)?;
        Self::new(token, destination)
    }

    #[cfg(test)]
    fn new_gitea_for_trusted_test(token: String, base_url: String) -> Result<Self> {
        let destination =
            super::trust::TrustedImportOrigins::trusted_api_destination_for_test(&base_url)?;
        Self::new_gitea(token, destination)
    }

    /// Get repository metadata.
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<GitHubRepo> {
        let url = format!("{}/repos/{}/{}", self.base_url, owner, repo);
        let resp = self.client.get(&url).send().await.context("get repo")?;
        Self::handle_response(resp).await
    }

    /// List all labels for a repository.
    pub async fn list_labels(&self, owner: &str, repo: &str) -> Result<Vec<GitHubLabel>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/labels?{}",
            self.base_url,
            owner,
            repo,
            self.dialect.page_size()
        ))
        .await
    }

    /// List milestones for a repository.
    pub async fn list_milestones(&self, owner: &str, repo: &str) -> Result<Vec<GitHubMilestone>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/milestones?state=all&{}",
            self.base_url,
            owner,
            repo,
            self.dialect.page_size()
        ))
        .await
    }

    /// List all issues (excluding pull requests) for a repository.
    pub async fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<GitHubIssue>> {
        // GitHub's /issues endpoint returns both issues and PRs.
        // We filter out PRs on our side since PRs are fetched separately.
        //
        // Oldest first, said out loud: GitHub's default is newest first. The
        // import writes each issue under its own number either way, but the
        // order it writes them in is the order the progress line counts and
        // the local ids follow.
        let raw: Vec<GitHubIssue> = self
            .paginate_all(&format!(
                "{}/repos/{}/{}/issues?state=all&type=issues{}&{}",
                self.base_url,
                owner,
                repo,
                self.dialect.oldest_first(),
                self.dialect.page_size()
            ))
            .await?;
        // Filter out pull requests (they have a pull_request field)
        Ok(raw
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .collect())
    }

    /// List pull requests for a repository.
    pub async fn list_pull_requests(&self, owner: &str, repo: &str) -> Result<Vec<GitHubPR>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/pulls?state=all{}&{}",
            self.base_url,
            owner,
            repo,
            self.dialect.oldest_first(),
            self.dialect.page_size()
        ))
        .await
    }

    /// List comments for an issue.
    pub async fn list_issue_comments(
        &self,
        owner: &str,
        repo: &str,
        issue_number: i64,
    ) -> Result<Vec<GitHubComment>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/issues/{}/comments?{}",
            self.base_url,
            owner,
            repo,
            issue_number,
            self.dialect.page_size()
        ))
        .await
    }

    /// List reviews for a pull request.
    pub async fn list_pr_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<GitHubReview>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/pulls/{}/reviews?{}",
            self.base_url,
            owner,
            repo,
            pr_number,
            self.dialect.page_size()
        ))
        .await
    }

    /// List the comments on lines of a pull request's diff.
    ///
    /// GitHub lists them for the whole pull request; Gitea has no such route
    /// (`/pulls/{n}/comments` answers `404`) and lists them per review, so
    /// there they are read review by review from `reviews`. A review that is
    /// only a request for one (`REQUEST_REVIEW`) or an unsubmitted draft holds
    /// none worth asking for.
    pub async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: i64,
        reviews: &[GitHubReview],
    ) -> Result<Vec<GitHubReviewComment>> {
        match self.dialect {
            ApiDialect::GitHub => {
                self.paginate_all(&format!(
                    "{}/repos/{}/{}/pulls/{}/comments?{}{}",
                    self.base_url,
                    owner,
                    repo,
                    pr_number,
                    self.dialect.page_size(),
                    self.dialect.oldest_first()
                ))
                .await
            }
            ApiDialect::Gitea => {
                let mut comments = Vec::new();
                for review in reviews
                    .iter()
                    .filter(|review| !matches!(review.state.as_str(), "PENDING" | "REQUEST_REVIEW"))
                {
                    let mut listed: Vec<GitHubReviewComment> = self
                        .paginate_all(&format!(
                            "{}/repos/{}/{}/pulls/{}/reviews/{}/comments?{}",
                            self.base_url,
                            owner,
                            repo,
                            pr_number,
                            review.id,
                            self.dialect.page_size()
                        ))
                        .await?;
                    for comment in &mut listed {
                        comment.pull_request_review_id.get_or_insert(review.id);
                    }
                    comments.extend(listed);
                }
                comments.sort_by_key(|comment| comment.id);
                Ok(comments)
            }
        }
    }

    /// List releases for a repository.
    pub async fn list_releases(&self, owner: &str, repo: &str) -> Result<Vec<GitHubRelease>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/releases?{}",
            self.base_url,
            owner,
            repo,
            self.dialect.page_size()
        ))
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
            Err(super::source_api_refusal("GitHub", status, &body))
        }
    }

    /// Fetch all pages of a paginated GitHub API endpoint.
    async fn paginate_all<T: serde::de::DeserializeOwned>(
        &self,
        initial_url: &str,
    ) -> Result<Vec<T>> {
        let mut results = Vec::new();
        let mut url = initial_url.to_string();

        loop {
            let resp = self.client.get(&url).send().await?;
            let status = resp.status();

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(super::source_api_refusal("GitHub", status, &body));
            }

            // Extract Link header BEFORE consuming resp
            let link_header = link_header_from(resp.headers(), &url)?;
            let next_url = next_page_url(&self.base_url, &link_header)?;

            let page: Vec<T> = resp.json().await?;
            results.extend(page);

            let Some(next_url) = next_url else {
                break;
            };
            url = next_url;
        }

        Ok(results)
    }
}

/// Read the next page URL and keep the PAT on the API origin that owns it.
///
/// A GitHub `Link` is a new direct request, not a redirect, so reqwest's
/// redirect policy never sees it. The check must therefore happen before the
/// next request builder is created; operator trust for another origin is not
/// authority to move this API origin's credential there.
fn next_page_url(api_base_url: &str, link_header: &str) -> Result<Option<String>> {
    if !link_header.contains("rel=\"next\"") {
        return Ok(None);
    }
    let next_url = extract_next_link(link_header)
        .ok_or_else(|| anyhow::anyhow!("next page link not found"))?;
    super::trust::require_same_origin(api_base_url, &next_url)
        .context("GitHub pagination link changed credential origin")?;
    Ok(Some(next_url))
}

/// The `Link` header as text, or an error if one was sent that cannot be read.
///
/// card_47f5114a92bc: the last page of a GitHub collection carries no `rel=
/// "next"`, and often no `Link` header at all, so an absent header genuinely
/// means "stop here". A header that is *present* and not valid text is the
/// source or a proxy in between failing its own protocol — and reading it as
/// `""` made `has_next` false, which is the same signal as "last page". The
/// import then finished successfully having fetched exactly one page of a
/// collection of unknown size.
fn link_header_from(headers: &reqwest::header::HeaderMap, url: &str) -> Result<String> {
    let Some(value) = headers.get(header::LINK) else {
        return Ok(String::new());
    };
    value.to_str().map(str::to_string).map_err(|_| {
        anyhow::anyhow!("GitHub sent a `Link` header that is not valid text for {url}")
    })
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
mod pagination_tests {
    use super::*;
    use crate::import::pagination_test_server::{respond, serve};
    use tokio::net::TcpListener;

    /// card_47f5114a92bc: a `Link` header that is present but undecodable used
    /// to collapse into `""`, which reads exactly like the last page. The
    /// import then finished successfully with page one of a collection whose
    /// real size nobody ever learned.
    ///
    /// The bad byte is written straight onto the socket because that is the
    /// only way to produce the state: `HeaderValue` accepts obs-text, and
    /// `to_str` is what refuses it.
    #[tokio::test]
    async fn an_undecodable_link_header_fails_the_import_instead_of_ending_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(serve(
            listener,
            vec![respond(
                b"Link: <http://\xff\xfe/next>; rel=\"next\"\r\n",
                "[]",
            )],
        ));

        let client =
            GitHubClient::new_for_trusted_test("token".to_owned(), format!("http://{addr}"))
                .expect("build client");
        let error = client
            .list_releases("team", "widgets")
            .await
            .expect_err("an unreadable Link header must not read as the last page");

        assert!(
            format!("{error:#}").contains("Link"),
            "the failure must name the header it could not read: {error:#}"
        );
        assert_eq!(server.await.expect("server").len(), 1);
    }

    /// Absence is still the last page — GitHub sends no `Link` at all for a
    /// collection that fits in one response, and that must keep working.
    #[tokio::test]
    async fn an_absent_link_header_is_still_a_complete_single_page() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(serve(listener, vec![respond(b"", RELEASE_PAGE)]));

        let client =
            GitHubClient::new_for_trusted_test("token".to_owned(), format!("http://{addr}"))
                .expect("build client");
        let releases = client
            .list_releases("team", "widgets")
            .await
            .expect("a single-page collection still imports");

        assert_eq!(releases.len(), 1);
        assert_eq!(server.await.expect("server").len(), 1);
    }

    /// And the discrimination: a valid `Link` chain is still followed, each
    /// page exactly once.
    #[tokio::test]
    async fn a_valid_link_chain_follows_every_page_exactly_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let next = format!("Link: <http://{addr}/page-two>; rel=\"next\"\r\n");
        let server = tokio::spawn(serve(
            listener,
            vec![
                respond(next.as_bytes(), RELEASE_PAGE),
                respond(b"", RELEASE_PAGE),
            ],
        ));

        let client =
            GitHubClient::new_for_trusted_test("token".to_owned(), format!("http://{addr}"))
                .expect("build client");
        let releases = client
            .list_releases("team", "widgets")
            .await
            .expect("a two-page collection imports whole");

        assert_eq!(releases.len(), 2, "both pages must reach the caller");
        let requests = server.await.expect("server");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].contains("/page-two"),
            "the second request must follow the advertised link: {}",
            requests[1]
        );
    }

    /// `Link` is not a redirect: a direct request through this client's
    /// default headers would attach the PAT again. Reject the URL before even
    /// connecting to the authority named by the source.
    #[tokio::test]
    async fn a_cross_origin_next_link_is_rejected_before_the_pat_client_connects() {
        let sink = TcpListener::bind("127.0.0.1:0").await.expect("bind sink");
        let sink_addr = sink.local_addr().expect("sink address");
        let source = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let source_addr = source.local_addr().expect("source address");
        let next = format!("Link: <http://{sink_addr}/page-two>; rel=\"next\"\r\n");
        let source_task = tokio::spawn(serve(source, vec![respond(next.as_bytes(), RELEASE_PAGE)]));

        let client = GitHubClient::new_for_trusted_test(
            "private-import-token".to_owned(),
            format!("http://{source_addr}"),
        )
        .expect("build client");
        let request = client.list_releases("team", "widgets");
        tokio::pin!(request);

        let error = tokio::select! {
            result = &mut request => {
                result.expect_err("a cross-origin pagination URL must fail the import")
            }
            accepted = sink.accept() => {
                let (_, peer) = accepted.expect("accept sink request");
                panic!("pagination connected to the cross-origin sink from {peer}");
            }
        };

        assert!(
            format!("{error:#}").contains("credential origin"),
            "the refusal must name the credential boundary: {error:#}"
        );
        let source_requests = source_task.await.expect("source task");
        assert_eq!(source_requests.len(), 1);
        assert!(
            source_requests[0].contains("Authorization: Bearer private-import-token")
                || source_requests[0].contains("authorization: Bearer private-import-token"),
            "baseline: the initiating API origin did not receive its PAT: {}",
            source_requests[0]
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), sink.accept())
                .await
                .is_err(),
            "the cross-origin sink received a connection after the refusal"
        );
    }

    #[test]
    fn pagination_origin_includes_scheme_host_and_effective_port() {
        let api = "https://api.example.test/v3";
        assert_eq!(
            next_page_url(api, "<https://api.example.test/repos?page=2>; rel=\"next\"",)
                .expect("same-origin next page"),
            Some("https://api.example.test/repos?page=2".to_owned())
        );

        for candidate in [
            "http://api.example.test:443/repos?page=2",
            "https://other.example.test/repos?page=2",
            "https://api.example.test:8443/repos?page=2",
        ] {
            let link = format!("<{candidate}>; rel=\"next\"");
            assert!(
                next_page_url(api, &link).is_err(),
                "pagination accepted a changed credential origin: {candidate}"
            );
        }
    }

    const RELEASE_PAGE: &str = r#"[{"id":1,"tag_name":"v1.0.0","name":null,"body":null,
        "prerelease":false,"draft":false,"created_at":"2026-01-01T00:00:00Z",
        "published_at":null}]"#;
}

#[cfg(test)]
mod redirect_tests {
    use super::*;
    use crate::import::api_client_test_support::SequencedResolver;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
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

        let client = GitHubClient::new_for_trusted_test(
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

    #[tokio::test]
    async fn same_origin_redirect_rechecks_dns_inside_the_connector() {
        let public_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind public stand-in");
        let port = public_listener.local_addr().expect("public address").port();
        let rebound_ip = Ipv4Addr::new(127, 0, 0, 2);
        let rebound_listener = TcpListener::bind((rebound_ip, port))
            .await
            .expect("bind private rebound sink");

        let source_task = tokio::spawn(async move {
            let (mut stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(1), public_listener.accept())
                    .await
                    .expect("the checked public answer was not consumed")
                    .expect("accept checked request");
            let request = read_headers(&mut stream).await;
            write_response(
                &mut stream,
                "302 Found",
                &format!("Location: http://rebind.test:{port}/renamed\r\n"),
            )
            .await;
            request
        });

        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = SequencedResolver::new(
            vec![
                vec![SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)],
                vec![SocketAddr::new(rebound_ip.into(), 0)],
            ],
            Arc::clone(&calls),
        );
        // Loopback is a reachable stand-in for the first public answer. The
        // production classifier is covered in `net`; this live test isolates
        // whether both clients keep that classifier inside every connection.
        let destination = crate::import::trust::TrustedImportOrigins::default()
            .api_destination_with_resolver(&format!("http://rebind.test:{port}"), resolver, |ip| {
                ip == IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))
            })
            .expect("guarded API destination");
        let client = GitHubClient::new("private-import-token".to_owned(), destination)
            .expect("build guarded client");

        client
            .get_repo("team", "widgets")
            .await
            .expect_err("the rebound answer must fail before a second connect");
        let source_request = source_task.await.expect("source task");
        assert!(
            source_request.contains("Authorization: Bearer private-import-token")
                || source_request.contains("authorization: Bearer private-import-token"),
            "baseline: the checked origin did not receive its token: {source_request}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(250),
                rebound_listener.accept(),
            )
            .await
            .is_err(),
            "the rebound sink received a request and could observe the bearer token"
        );
    }
}

/// card_64ae115d5bd6: Gitea and Forgejo through the GitHub client. The response
/// samples follow Gitea's own `modules/structs` — the fields it sends, and the
/// `null` it sends for an empty list.
#[cfg(test)]
mod gitea_tests {
    use super::*;
    use crate::import::pagination_test_server::{respond, serve};
    use tokio::net::TcpListener;

    const USER: &str = r#"{"id":3,"login":"alice","login_name":"","source_id":0,
        "full_name":"Alice","email":"alice@example.com","avatar_url":"https://gitea.example.com/avatars/3",
        "html_url":"https://gitea.example.com/alice","is_admin":false}"#;

    fn issue_page(number: i64) -> String {
        format!(
            r#"[{{"id":{id},"number":{number},"user":{USER},"original_author":"","original_author_id":0,
            "title":"filed on Gitea","body":"body","ref":"","assets":[],"labels":null,
            "milestone":{{"id":4,"title":"v1","description":"","state":"open","open_issues":1,
              "closed_issues":0,"created_at":"2026-01-01T00:00:00Z","updated_at":null,
              "closed_at":null,"due_on":null}},
            "assignee":null,"assignees":null,"state":"open","is_locked":false,"comments":2,
            "created_at":"2026-01-02T00:00:00Z","updated_at":"2026-01-03T00:00:00Z",
            "closed_at":null,"due_date":null,"pull_request":null,
            "repository":{{"id":1,"name":"widgets","owner":"team","full_name":"team/widgets"}}}}]"#,
            id = number + 100,
        )
    }

    /// The dialect's own spelling — `token`, `limit`, `type=issues` — and a
    /// `Link` chain followed page by page, with every `null` list read as an
    /// empty one rather than failing the import.
    #[tokio::test]
    async fn a_gitea_issue_list_is_read_in_gitea_s_spelling() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let next = format!("Link: <http://{addr}/api/v1/page-two>; rel=\"next\"\r\n");
        let first = issue_page(1);
        let second = issue_page(2);
        let server = tokio::spawn(serve(
            listener,
            vec![respond(next.as_bytes(), &first), respond(b"", &second)],
        ));

        let client = GitHubClient::new_gitea_for_trusted_test(
            "gitea-token".to_owned(),
            format!("http://{addr}/api/v1"),
        )
        .expect("build client");
        let issues = client
            .list_issues("team", "widgets")
            .await
            .expect("Gitea's issue list imports");

        assert_eq!(
            issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(issues[0].assignees.is_empty() && issues[0].labels.is_empty());
        let milestone = issues[0].milestone.as_ref().expect("the milestone");
        assert_eq!(
            milestone.number, 4,
            "a Gitea milestone is numbered by its id"
        );
        assert_eq!(milestone.updated_at, None);
        assert_eq!(issues[0].user.as_ref().unwrap().login, "alice");

        let requests = server.await.expect("server");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].starts_with(
                "GET /api/v1/repos/team/widgets/issues?state=all&type=issues&limit=50 "
            ),
            "{}",
            requests[0]
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: token gitea-token"),
            "{}",
            requests[0]
        );
        assert!(
            !requests[0]
                .to_ascii_lowercase()
                .contains("x-github-api-version"),
            "{}",
            requests[0]
        );
        assert!(requests[1].contains("/api/v1/page-two"), "{}", requests[1]);
    }

    /// A merged pull request, its reviews in Gitea's state names, and a
    /// release whose attachment carries no `content_type` — each as Gitea
    /// sends it.
    #[test]
    fn gitea_pull_requests_reviews_and_releases_deserialize() {
        let repo = format!(
            r#"{{"id":1,"owner":{USER},"name":"widgets","full_name":"team/widgets","description":"",
            "empty":false,"private":false,"fork":false,"html_url":"https://gitea.example.com/team/widgets",
            "ssh_url":"git@gitea.example.com:team/widgets.git",
            "clone_url":"https://gitea.example.com/team/widgets.git","default_branch":"main"}}"#
        );
        let pr: GitHubPR = serde_json::from_str(&format!(
            r#"{{"id":20,"number":8,"user":{USER},"title":"merged on Gitea","body":"",
            "labels":null,"milestone":null,"assignee":null,"assignees":null,
            "requested_reviewers":null,"state":"closed","draft":false,"is_locked":false,
            "comments":0,"mergeable":false,"merged":true,"merged_at":"2026-01-05T00:00:00Z",
            "merge_commit_sha":"3333333333333333333333333333333333333333","merged_by":{USER},
            "base":{{"label":"main","ref":"main","sha":"2222222222222222222222222222222222222222","repo_id":1,"repo":{repo}}},
            "head":{{"label":"feature","ref":"feature","sha":"1111111111111111111111111111111111111111","repo_id":1,"repo":null}},
            "merge_base":"0000000000000000000000000000000000000000","due_date":null,
            "created_at":"2026-01-04T00:00:00Z","updated_at":"2026-01-05T00:00:00Z",
            "closed_at":"2026-01-05T00:00:00Z","pin_order":0}}"#
        ))
        .expect("a Gitea pull request deserializes");
        assert_eq!(pr.merged, Some(true));
        assert!(pr.labels.is_empty());
        assert_eq!(pr.head.sha, "1111111111111111111111111111111111111111");
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("3333333333333333333333333333333333333333")
        );

        let reviews: Vec<GitHubReview> = serde_json::from_str(&format!(
            r#"[{{"id":5,"user":{USER},"team":null,"state":"REQUEST_CHANGES","body":"no",
              "commit_id":"1111111111111111111111111111111111111111","stale":false,"official":true,
              "dismissed":false,"comments_count":0,"submitted_at":"2026-01-04T12:00:00Z"}},
             {{"id":6,"user":{USER},"team":null,"state":"REQUEST_REVIEW","body":"",
              "commit_id":"","stale":false,"official":false,"dismissed":false,
              "comments_count":0,"submitted_at":"0001-01-01T00:00:00Z"}}]"#
        ))
        .expect("Gitea reviews deserialize");
        assert_eq!(reviews[0].state, "REQUEST_CHANGES");

        let releases: Vec<GitHubRelease> = serde_json::from_str(&format!(
            r#"[{{"id":9,"tag_name":"v1","target_commitish":"main","name":"v1","body":"",
              "draft":false,"prerelease":false,"created_at":"2026-01-06T00:00:00Z",
              "published_at":"2026-01-06T00:00:00Z","author":{USER},
              "assets":[{{"id":1,"name":"widgets.tgz","size":12,"download_count":0,
                "created_at":"2026-01-06T00:00:00Z","uuid":"0b3e",
                "browser_download_url":"https://gitea.example.com/attachments/0b3e"}}]}}]"#
        ))
        .expect("a Gitea release deserializes");
        assert_eq!(releases[0].assets[0].content_type, "");
        assert_eq!(releases[0].author.as_ref().unwrap().id, 3);
    }
}
