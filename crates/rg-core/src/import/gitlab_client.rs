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
    /// `base_url` must be `https://gitlab.com/api/v4` for GitLab.com
    /// or `https://<hostname>/api/v4` for self-hosted instances. Requiring it
    /// keeps the source repository and the authenticated API host coupled; the
    /// import service derives it from the source URL.
    pub fn new(token: String, base_url: String) -> Result<Self> {
        Self::with_builder(token, base_url, crate::net::outbound_client_builder())
    }

    fn with_builder(
        token: String,
        base_url: String,
        builder: reqwest::ClientBuilder,
    ) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "PRIVATE-TOKEN",
            header::HeaderValue::from_str(&token)
                .context("invalid import auth token: not a valid HTTP header value")?,
        );

        // Reuse the shared outbound builder so the import client inherits the
        // request + connect timeout — a slow/hanging import source (e.g. a
        // self-hosted GitLab `base_url`) can't pin the import worker forever.
        // Same-origin redirects remain enabled, but a redirect may not move
        // PRIVATE-TOKEN to a different scheme, host, or port.
        let client = builder
            .default_headers(headers)
            .redirect(crate::net::same_origin_redirect_policy())
            .user_agent("ForgeKeep/0.1")
            .build()
            .context("failed to build GitLab HTTP client")?;

        Ok(Self { client, base_url })
    }

    #[cfg(test)]
    fn accepting_invalid_certificates_for_test(token: String, base_url: String) -> Result<Self> {
        Self::with_builder(
            token,
            base_url,
            crate::net::outbound_client_builder().danger_accept_invalid_certs(true),
        )
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
            let total_pages = total_pages_from(resp.headers(), &url)?;

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

/// How many pages GitLab says the collection has, or an error if it said
/// something this client cannot read.
///
/// card_47f5114a92bc: absent and unreadable are different answers and only one
/// of them means "one page". GitLab legitimately omits `x-total-pages` — it
/// stops counting past the offset-pagination limit — so an absent header has to
/// go on meaning a single page. A header that is *present* and cannot be
/// decoded or parsed is the source or a proxy in between failing its own
/// protocol, and reading it as `1` used to end the import there: every later
/// page silently missing from a job that reported success, which is exactly the
/// state an importer must never reach.
///
/// A present-but-empty value is read as absence rather than as a failure. It
/// carries no count either way, and refusing it would turn a header nobody
/// promised into an import that cannot run at all.
fn total_pages_from(headers: &reqwest::header::HeaderMap, url: &str) -> Result<i64> {
    let Some(value) = headers.get("x-total-pages") else {
        return Ok(1);
    };
    let value = value.to_str().map_err(|_| {
        anyhow::anyhow!("GitLab sent an `x-total-pages` header that is not valid text for {url}")
    })?;
    if value.trim().is_empty() {
        return Ok(1);
    }
    value.trim().parse().map_err(|_| {
        anyhow::anyhow!("GitLab sent an unreadable `x-total-pages` header for {url}: {value:?}")
    })
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

#[cfg(test)]
mod pagination_tests {
    use super::*;
    use crate::import::pagination_test_server::{respond, serve};
    use tokio::net::TcpListener;

    const MEMBER_PAGE: &str = r#"[{"id":1,"username":"ada","name":"Ada","email":null,
        "avatar_url":null}]"#;

    /// card_47f5114a92bc: `x-total-pages` used to reach `1` through two `.ok()`
    /// conversions, so a value the client could not read was indistinguishable
    /// from a collection that genuinely has one page. The import then stopped
    /// after page one and reported success.
    ///
    /// Both unreadable shapes are covered: bytes that are not text at all
    /// (written onto the socket by hand, since `to_str` is what refuses them)
    /// and text that is not a number.
    #[tokio::test]
    async fn an_unreadable_total_pages_header_fails_the_import_instead_of_truncating_it() {
        for (label, header) in [
            ("not text", b"x-total-pages: \xff\xfe\r\n".to_vec()),
            ("not a number", b"x-total-pages: many\r\n".to_vec()),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("address");
            let server = tokio::spawn(serve(listener, vec![respond(&header, MEMBER_PAGE)]));

            let client = GitLabClient::new("token".to_owned(), format!("http://{addr}"))
                .expect("build client");
            let error = client
                .list_members("group/widgets")
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!("{label}: an unreadable count must not read as a complete single page")
                });

            assert!(
                format!("{error:#}").contains("x-total-pages"),
                "{label}: the failure must name the header it could not read: {error:#}"
            );
            assert_eq!(server.await.expect("server").len(), 1, "{label}");
        }
    }

    /// Absence still means one page — GitLab stops sending the header past its
    /// offset-pagination limit, so refusing an absent one would break imports
    /// that work today.
    #[tokio::test]
    async fn an_absent_total_pages_header_is_still_a_complete_single_page() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(serve(listener, vec![respond(b"", MEMBER_PAGE)]));

        let client =
            GitLabClient::new("token".to_owned(), format!("http://{addr}")).expect("build client");
        let members = client
            .list_members("group/widgets")
            .await
            .expect("a single-page collection still imports");

        assert_eq!(members.len(), 1);
        assert_eq!(server.await.expect("server").len(), 1);
    }

    /// And the discrimination: a valid count is still walked to the end, each
    /// page requested exactly once.
    #[tokio::test]
    async fn a_valid_total_pages_header_walks_every_page_exactly_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(serve(
            listener,
            vec![
                respond(b"x-total-pages: 2\r\n", MEMBER_PAGE),
                respond(b"x-total-pages: 2\r\n", MEMBER_PAGE),
            ],
        ));

        let client =
            GitLabClient::new("token".to_owned(), format!("http://{addr}")).expect("build client");
        let members = client
            .list_members("group/widgets")
            .await
            .expect("a two-page collection imports whole");

        assert_eq!(members.len(), 2, "both pages must reach the caller");
        let requests = server.await.expect("server");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].contains("page=1") && requests[1].contains("page=2"),
            "each page must be asked for once: {requests:?}"
        );
    }
}

#[cfg(test)]
mod redirect_tests {
    use super::*;
    use base64::Engine as _;
    use std::sync::Arc;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::{
        rustls::{
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
            ServerConfig,
        },
        TlsAcceptor,
    };

    const TEST_CERTIFICATE_DER: &str = "MIIBPjCB8aADAgECAhRtMm+cbTzk/EnFuG0wcMOmUIVQHTAFBgMrZXAwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDkwMTExMTEzNVoYDzIxMjYwODA4MTExMTM1WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwKjAFBgMrZXADIQCP6dgCDi/yS8ie1VW6fCETD657m92riUJGlhMVl+YS0aNTMFEwHQYDVR0OBBYEFGW4YP4Wu0nfURLiSO7uZQvPaf3iMB8GA1UdIwQYMBaAFGW4YP4Wu0nfURLiSO7uZQvPaf3iMA8GA1UdEwEB/wQFMAMBAf8wBQYDK2VwA0EAnfukFtSVZOo2eFHJxA/ejsZqGnY+d4/G6GvjQCw3/xKC9AEw604e63tdQGWsQtFQoenIhD+tkZjIGsji6uYYBw==";
    const TEST_PRIVATE_KEY_DER: &str =
        "MC4CAQAwBQYDK2VwBCIEIBQe5/XwKhnd72CwXARtftqM0AN2H21EiCAEx29TSHh9";

    async fn read_headers(stream: &mut (impl AsyncRead + Unpin)) -> String {
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

    async fn write_response(
        stream: &mut (impl AsyncWrite + Unpin),
        status: &str,
        headers: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    }

    fn test_tls_acceptor() -> TlsAcceptor {
        // The test dependency graph activates both rustls providers through
        // independent crates, so this fixture must select one explicitly.
        drop(tokio_rustls::rustls::crypto::ring::default_provider().install_default());
        let decode = |value| {
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .expect("decode TLS fixture")
        };
        let certificate = CertificateDer::from(decode(TEST_CERTIFICATE_DER));
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(decode(TEST_PRIVATE_KEY_DER)));
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .expect("build TLS fixture");
        TlsAcceptor::from(Arc::new(config))
    }

    fn project_json() -> &'static str {
        r#"{
            "id": 1,
            "name": "widgets",
            "path_with_namespace": "team/widgets",
            "visibility": "private",
            "default_branch": "main",
            "web_url": "https://gitlab.example/team/widgets",
            "http_url_to_repo": "https://gitlab.example/team/widgets.git",
            "namespace": {"id": 2, "name": "team", "path": "team", "kind": "group"}
        }"#
    }

    #[tokio::test]
    async fn cross_origin_redirect_never_receives_the_private_token() {
        let sink = TcpListener::bind("127.0.0.1:0").await.expect("bind sink");
        let sink_addr = sink.local_addr().expect("sink address");
        let source = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let source_addr = source.local_addr().expect("source address");

        let sink_task = tokio::spawn(async move {
            match tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await {
                Ok(Ok((mut stream, _))) => {
                    let request = read_headers(&mut stream).await;
                    write_response(&mut stream, "200 OK", "", project_json()).await;
                    Some(request)
                }
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
                "",
            )
            .await;
            request
        });

        let client = GitLabClient::new(
            "private-import-token".to_owned(),
            format!("http://{source_addr}/api/v4"),
        )
        .expect("build client");
        let error = client
            .get_project("team/widgets")
            .await
            .expect_err("a blocked redirect remains a 302 API response");
        assert!(format!("{error:#}").contains("302"));

        let source_request = source_task.await.expect("source task");
        assert!(
            source_request.contains("PRIVATE-TOKEN: private-import-token")
                || source_request.contains("private-token: private-import-token"),
            "baseline: the configured API origin did not receive its token: {source_request}"
        );
        assert!(
            sink_task.await.expect("sink task").is_none(),
            "the cross-origin redirect was followed and could receive the PAT"
        );
    }

    #[tokio::test]
    async fn same_origin_redirects_still_work_and_keep_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let address = listener.local_addr().expect("source address");
        let tls = test_tls_acceptor();
        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("accept first request");
            let mut first = tls.accept(first).await.expect("accept first TLS request");
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: https://{address}/renamed\r\n"),
                "",
            )
            .await;

            let (second, _) = listener.accept().await.expect("accept redirected request");
            let mut second = tls
                .accept(second)
                .await
                .expect("accept redirected TLS request");
            let second_request = read_headers(&mut second).await;
            write_response(&mut second, "200 OK", "", project_json()).await;
            (first_request, second_request)
        });

        let client = GitLabClient::accepting_invalid_certificates_for_test(
            "private-import-token".to_owned(),
            format!("https://{address}/api/v4"),
        )
        .expect("build test client");
        let project = client
            .get_project("team/widgets")
            .await
            .expect("same-origin redirect");
        assert_eq!(project.path_with_namespace, "team/widgets");

        let (first, second) = server.await.expect("server task");
        for request in [first, second] {
            assert!(
                request.contains("PRIVATE-TOKEN: private-import-token")
                    || request.contains("private-token: private-import-token"),
                "same-origin request lost its token: {request}"
            );
        }
    }
}
