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
    /// `destination` owns both the API base URL and the reachability-aware
    /// builder issued for that exact URL by [`super::trust::TrustedImportOrigins`].
    /// This makes it impossible to validate one hostname and connect the PAT
    /// client through a fresh resolver for another.
    pub fn new(token: String, destination: super::trust::ImportApiDestination) -> Result<Self> {
        let (base_url, builder) = destination.into_parts();
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
        let client = builder
            .default_headers(headers)
            .redirect(crate::net::same_origin_redirect_policy())
            .user_agent("PlombirGit/0.1")
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self {
            client,
            base_url,
            token,
        })
    }

    #[cfg(test)]
    fn new_for_trusted_test(token: String, base_url: String) -> Result<Self> {
        let destination =
            super::trust::TrustedImportOrigins::trusted_api_destination_for_test(&base_url)?;
        Self::new(token, destination)
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
            "{}/repos/{}/{}/labels?per_page=100",
            self.base_url, owner, repo
        ))
        .await
    }

    /// List milestones for a repository.
    pub async fn list_milestones(&self, owner: &str, repo: &str) -> Result<Vec<GitHubMilestone>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/milestones?state=all&per_page=100",
            self.base_url, owner, repo
        ))
        .await
    }

    /// List all issues (excluding pull requests) for a repository.
    pub async fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<GitHubIssue>> {
        // GitHub's /issues endpoint returns both issues and PRs.
        // We filter out PRs on our side since PRs are fetched separately.
        let raw: Vec<GitHubIssue> = self
            .paginate_all(&format!(
                "{}/repos/{}/{}/issues?state=all&per_page=100",
                self.base_url, owner, repo
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
            "{}/repos/{}/{}/pulls?state=all&per_page=100",
            self.base_url, owner, repo
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
            "{}/repos/{}/{}/issues/{}/comments?per_page=100",
            self.base_url, owner, repo, issue_number
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
            "{}/repos/{}/{}/pulls/{}/reviews?per_page=100",
            self.base_url, owner, repo, pr_number
        ))
        .await
    }

    /// List releases for a repository.
    pub async fn list_releases(&self, owner: &str, repo: &str) -> Result<Vec<GitHubRelease>> {
        self.paginate_all(&format!(
            "{}/repos/{}/{}/releases?per_page=100",
            self.base_url, owner, repo
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
