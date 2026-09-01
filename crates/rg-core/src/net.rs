//! SSRF-hardened outbound HTTP.
//!
//! Anything ForgeKeep POSTs/GETs to a *user-supplied* URL (webhook delivery,
//! and any future user-controlled fetch) must go through here so we get, in
//! one place:
//!
//! - a **single reusable** `reqwest::Client` (not one per request) with a
//!   request + connect timeout, so a slow/hanging peer can't pin a task
//!   forever;
//! - `redirect(Policy::none())` — a `3xx` to `http://169.254.169.254/…`
//!   (cloud metadata) or an internal host is returned verbatim, never
//!   followed;
//! - [`ssrf_safe_outbound_client`] — resolve a webhook host inside reqwest's
//!   connector, reject private / loopback / link-local / ULA / CGNAT answers,
//!   and return those same checked addresses to the connector. A user therefore
//!   cannot point a webhook straight at `127.0.0.1`, `10.0.0.x`,
//!   `169.254.169.254`, `[::1]`, etc., or swap a public DNS answer for an
//!   internal one between a preflight lookup and the TCP connection.
//!
//! The connector-owned resolution plus the redirect ban cover direct internal
//! URLs, redirect-to-internal, and DNS rebinding without a check/use gap.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use rg_git::credentials::OutboundGitInvocation;

/// Default request timeout for outbound HTTP (whole request, including body).
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connect timeout for outbound HTTP (TCP + TLS handshake only).
const OUTBOUND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// An HTTP(S) git remote coupled to the DNS decision that admitted it.
///
/// The URL stays private so a clone/fetch sink cannot accidentally use the raw
/// string while dropping the checked addresses. Exact trusted import origins
/// use the same type and the same binding; their exception changes only which
/// addresses may be admitted, not whether git may resolve the host again.
pub(crate) struct GuardedGitRemote {
    url: String,
    binding: Option<GitHttpBinding>,
}

struct GitHttpBinding {
    host: String,
    port: u16,
    addresses: Vec<IpAddr>,
}

impl GuardedGitRemote {
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Apply the checked destination to the one invocation that consumes it.
    pub(crate) fn bind_invocation(
        &self,
        invocation: OutboundGitInvocation,
    ) -> Result<OutboundGitInvocation> {
        let invocation = invocation.lock_http_destination();
        match &self.binding {
            Some(binding) => {
                invocation.bind_http_host(&binding.host, binding.port, &binding.addresses)
            }
            // Numerical hosts do not perform DNS. The invocation is still
            // locked against redirects and inherited CURLOPT_RESOLVE entries.
            None => Ok(invocation),
        }
    }

    #[cfg(test)]
    pub(crate) fn unbound_for_test(raw: &str) -> Self {
        Self {
            url: raw.to_owned(),
            binding: None,
        }
    }

    /// Local-only source used by the wiki import integration contract.
    ///
    /// This cannot turn a remote into an unbound destination: only an absolute
    /// filesystem path is accepted. Production import paths always use
    /// [`TrustedImportOrigins::git_destination`](crate::import::trust::TrustedImportOrigins::git_destination).
    pub(crate) fn local_path(path: &std::path::Path) -> Result<Self> {
        if !path.is_absolute() {
            anyhow::bail!("local git source must be an absolute filesystem path");
        }
        Ok(Self {
            url: path.to_string_lossy().into_owned(),
            binding: None,
        })
    }
}

/// A parsed HTTP(S) origin: scheme, host, and effective port, with paths and
/// credentials deliberately excluded from its identity.
///
/// Import trust and OIDC plaintext exceptions both need this exact boundary.
/// Keeping the parser here prevents the two security policies from drifting on
/// details such as implicit ports, URL userinfo, paths, or wildcards.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HttpOrigin {
    pub(crate) scheme: String,
    host: String,
    port: u16,
}

impl HttpOrigin {
    pub(crate) fn from_url(url: &reqwest::Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        Some(Self {
            scheme: url.scheme().to_owned(),
            host: url.host_str()?.to_owned(),
            port: url.port_or_known_default()?,
        })
    }

    pub(crate) fn from_target(raw: &str) -> Option<Self> {
        reqwest::Url::parse(raw)
            .ok()
            .as_ref()
            .and_then(Self::from_url)
    }

    pub(crate) fn from_config(raw: &str, setting: &str) -> Result<Self> {
        let trimmed = raw.trim();
        let url = reqwest::Url::parse(trimmed)
            .with_context(|| format!("invalid {setting} '{trimmed}'"))?;
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("{setting} '{trimmed}' must use http or https");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("{setting} '{trimmed}' has no host"))?;
        if host.contains('*') {
            anyhow::bail!("{setting} '{trimmed}' must name one exact host, not a wildcard");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("{setting} '{trimmed}' must not contain user information");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            anyhow::bail!(
                "{setting} '{trimmed}' must contain only scheme, host, and optional port"
            );
        }
        Self::from_url(&url)
            .ok_or_else(|| anyhow::anyhow!("{setting} '{trimmed}' has no effective port"))
    }
}

/// A `reqwest::ClientBuilder` pre-seeded with the outbound `timeout` +
/// `connect_timeout`, so a slow/hanging peer can't pin a task forever.
///
/// This is the shared floor for **any** long-lived outbound client. It sets
/// only the timeouts — no redirect policy, no user-agent, no default headers —
/// so each caller layers on what it needs:
///
/// - [`outbound_client`] adds `redirect(Policy::none())` for operator-controlled
///   identity-provider endpoints, where private addresses are legitimate.
/// - [`ssrf_safe_outbound_client`] adds the same redirect ban plus connector-
///   owned DNS validation for user-supplied webhook targets.
/// - The import clients (`GitHubClient` / `GitLabClient`) add their per-instance
///   auth headers (`Bearer` / `PRIVATE-TOKEN`) + UA. Their redirect policy keeps
///   the default count limit but follows only the API base's exact origin, so a
///   rename can work without moving the PAT to another scheme, host, or port.
///   Private self-hosted origins are admitted separately through the import admin
///   trust policy, not through this generic HTTP builder.
pub fn outbound_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(OUTBOUND_TIMEOUT)
        .connect_timeout(OUTBOUND_CONNECT_TIMEOUT)
}

type DnsError = Box<dyn std::error::Error + Send + Sync>;

/// Tokio-backed DNS lookup used underneath the webhook-only SSRF resolver.
///
/// Keeping the system lookup behind reqwest's [`reqwest::dns::Resolve`] trait is
/// the important boundary: the connector consumes the exact iterator this
/// lookup produced instead of resolving the hostname again after a preflight
/// check.
struct SystemResolver;

impl reqwest::dns::Resolve for SystemResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| Box::new(error) as DnsError)?
                .collect();
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Reject forbidden DNS answers before returning them to reqwest's connector.
///
/// A separate `lookup_host` followed by `Client::send` is not sufficient: an
/// attacker-controlled authoritative server can answer the check with a public
/// address and reqwest's later lookup with an internal one. This resolver owns
/// both halves of the decision. Every address it returns has been checked, and
/// reqwest connects directly to that returned iterator.
struct ForbiddenAddressResolver<R> {
    inner: R,
    is_forbidden: fn(IpAddr) -> bool,
}

impl<R> ForbiddenAddressResolver<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            is_forbidden: is_forbidden_ip,
        }
    }

    #[cfg(test)]
    fn with_classifier(inner: R, is_forbidden: fn(IpAddr) -> bool) -> Self {
        Self {
            inner,
            is_forbidden,
        }
    }
}

impl<R: reqwest::dns::Resolve> reqwest::dns::Resolve for ForbiddenAddressResolver<R> {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let resolving = self.inner.resolve(name);
        let is_forbidden = self.is_forbidden;
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = resolving.await?.collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("outbound host '{host}' did not resolve to any address"),
                )) as DnsError);
            }
            if let Some(addr) = addrs.iter().find(|addr| is_forbidden(addr.ip())) {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "outbound host '{host}' resolves to a forbidden address: {}",
                        addr.ip()
                    ),
                )) as DnsError);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Bind one client builder to the answers returned by `resolver` and bypass
/// system HTTP proxies, which would otherwise resolve the target outside this
/// connector.
fn resolver_bound_client_builder<R: reqwest::dns::Resolve + 'static>(
    builder: reqwest::ClientBuilder,
    resolver: R,
) -> reqwest::ClientBuilder {
    builder
        // A proxy would resolve the target independently and recreate the same
        // check/use gap outside this connector. User-controlled targets
        // therefore connect directly; operator-controlled clients keep normal
        // proxy behaviour through the generic builder.
        .no_proxy()
        .dns_resolver(Arc::new(resolver))
}

/// Outbound builder whose connector rejects every forbidden DNS answer and
/// consumes the remaining checked addresses directly.
///
/// Redirect ownership stays with the caller: webhooks disable redirects,
/// while import API clients permit only same-origin redirects and must apply
/// the same resolver to every new connection in that chain.
pub(crate) fn ssrf_bound_outbound_client_builder() -> reqwest::ClientBuilder {
    resolver_bound_client_builder(
        outbound_client_builder(),
        ForbiddenAddressResolver::new(SystemResolver),
    )
}

#[cfg(test)]
pub(crate) fn ssrf_bound_outbound_client_builder_with_resolver<
    R: reqwest::dns::Resolve + 'static,
>(
    resolver: R,
    is_forbidden: fn(IpAddr) -> bool,
) -> reqwest::ClientBuilder {
    resolver_bound_client_builder(
        outbound_client_builder(),
        ForbiddenAddressResolver::with_classifier(resolver, is_forbidden),
    )
}

fn ssrf_safe_client_builder<R: reqwest::dns::Resolve + 'static>(
    resolver: R,
) -> reqwest::ClientBuilder {
    resolver_bound_client_builder(outbound_client_builder(), resolver)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("ForgeKeep-Webhook/0.1")
}

/// Follow redirects only while they stay on the initiating request's exact
/// HTTP origin (`scheme + host + effective port`).
///
/// Reqwest's default policy strips sensitive headers when host or port changes,
/// but not when only the scheme changes. Stopping before any origin change is
/// therefore the credential boundary; delegating allowed redirects back to the
/// default policy preserves its normal redirect-count limit.
pub fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    let default = reqwest::redirect::Policy::default();
    reqwest::redirect::Policy::custom(move |attempt| {
        let stays_on_origin = attempt.previous().first().is_some_and(|initial| {
            initial.scheme() == attempt.url().scheme()
                && initial.host_str() == attempt.url().host_str()
                && initial.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if stays_on_origin {
            default.redirect(attempt)
        } else {
            attempt.stop()
        }
    })
}

/// Shared outbound HTTP client for operator-controlled provider endpoints.
///
/// Built once and reused: `timeout` + `connect_timeout` bound every request,
/// and redirects are **not** followed (a `3xx` is returned as-is). This client
/// deliberately permits private addresses for self-hosted identity providers;
/// user-supplied targets must use [`ssrf_safe_outbound_client`] instead.
pub fn outbound_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        outbound_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("ForgeKeep-Webhook/0.1")
            .build()
            .expect("failed to build hardened outbound HTTP client")
    })
}

/// Shared outbound client for user-supplied webhook targets.
///
/// DNS classification happens inside reqwest's resolver and the connector uses
/// the returned checked addresses directly. Redirects and proxies are disabled,
/// so neither can introduce a second destination-resolution boundary.
pub fn ssrf_safe_outbound_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        ssrf_safe_client_builder(ForbiddenAddressResolver::new(SystemResolver))
            .build()
            .expect("failed to build SSRF-safe outbound HTTP client")
    })
}

/// Is `ip` one we must never let a user-supplied URL reach?
///
/// Covers loopback, private (RFC 1918), link-local (incl. the
/// `169.254.169.254` cloud-metadata address), unspecified, broadcast,
/// multicast, documentation, CGNAT (RFC 6598) for v4, and loopback /
/// unspecified / multicast / unique-local / link-local for v6 (plus
/// IPv4-mapped v6 folded back to its v4 check).
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => is_forbidden_v4(mapped),
            None => is_forbidden_v6(v6),
        },
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()            // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback()    // 127/8
        || ip.is_link_local()  // 169.254/16 — includes the metadata IP
        || ip.is_unspecified() // 0.0.0.0
        || ip.is_broadcast()   // 255.255.255.255
        || ip.is_multicast()   // 224/4
        || ip.is_documentation()
        || is_shared_cgnat(ip) // 100.64/10 (RFC 6598)
        || ip.octets()[0] == 0 // 0.0.0.0/8 "this network"
}

/// 100.64.0.0/10 — carrier-grade NAT space, treated as internal.
fn is_shared_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 64
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()               // ::1
        || ip.is_unspecified()     // ::
        || ip.is_multicast()       // ff00::/8
        || is_unique_local(ip)     // fc00::/7
        || is_unicast_link_local(ip) // fe80::/10
}

/// fc00::/7 — IPv6 unique-local (the `is_unique_local` std method is unstable).
fn is_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// fe80::/10 — IPv6 link-local unicast (std method unstable).
fn is_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// Cheap, DNS-free pre-check: enforce `http`/`https` and reject an obviously
/// internal **IP-literal** host. Used at webhook create/update time so an
/// operator gets immediate feedback on `http://localhost`-style mistakes
/// without a create-path DNS lookup that a transient outage could fail.
///
/// Every rejection here is about the URL the caller supplied, so each one
/// carries [`crate::error::InvalidRequest`] — a handler funnelling the result
/// through `AppError::from` answers `400` with the rule that was broken, and a
/// storage or database failure sitting next to the check keeps its own 5xx
/// instead of being reported as a malformed URL.
pub fn check_url_static(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| crate::error::invalid_request(format!("invalid outbound URL: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(crate::error::invalid_request(format!(
                "outbound URL scheme '{other}' not allowed (only http/https)"
            )))
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| crate::error::invalid_request("outbound URL has no host"))?;
    // `host_str()` keeps brackets for IPv6 literals (`[::1]`); strip them so
    // the literal parses. A domain name never contains brackets.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(crate::error::invalid_request(format!(
                "outbound URL points at a forbidden (private/loopback/link-local) address: {ip}"
            )));
        }
    }
    Ok(())
}

// ── Git-remote SSRF guard ───────────────────────────────────────────────────
//
// A user-supplied *git remote* (mirror sync, repository import) is reached by
// spawning a `git` subprocess, not by `reqwest`, so the webhook client hardening
// above does not cover it. These helpers are the git twins of
// [`check_url_static`] / [`guard_outbound_url`]: same private-IP classifier
// ([`is_forbidden_ip`]), but a scheme allow-list tuned for git transports and a
// parser that also understands scp-like `host:path` shorthand.

/// URL schemes ForgeKeep will run a `git` subprocess against for a
/// **user-supplied** remote. Everything else — `file://` (local-disk read),
/// `ext::` (arbitrary transport-helper command), `ftp://`, … — is rejected so a
/// remote URL can neither read local files nor execute a helper binary.
///
/// SSH is deliberately absent. Outbound git runs with an isolated `HOME`, so
/// host keys and `~/.ssh` identities are unavailable, but an inherited agent can
/// still answer for any host the user names. SSH can return only with a separate
/// explicitly configured identity and `IdentitiesOnly=yes` contract.
pub const ALLOWED_GIT_URL_SCHEMES: &[&str] = &["https", "http", "git"];

/// Split a git remote into `(scheme, host)`.
///
/// Handles both the allowed URL forms (`https://`, `http://`, `git://`) and SSH
/// forms (`ssh://`, `[user@]host:path`) so the latter receive the same explicit
/// scheme rejection. A bare local path yields no host and is rejected by the
/// caller.
fn split_git_remote(raw: &str) -> Result<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(crate::error::invalid_request("git remote URL is empty"));
    }
    // Standard URL form. scp-like shorthand fails to parse as a URL (the `@` in
    // the authority is not a valid scheme char) and falls through below.
    if let Ok(url) = reqwest::Url::parse(raw) {
        let scheme = url.scheme().to_string();
        let host = url.host_str().unwrap_or("").to_string();
        return Ok((scheme, host));
    }
    // scp-like shorthand `[user@]host:path` → ssh. Guard on the absence of
    // `://` so a real (but unexpectedly unparsed) URL can never be mistaken for
    // scp syntax. The part before the first ':' must be a plain host (no '/'),
    // else it is a bare local path like `/srv/repo.git` with no remote host.
    if !raw.contains("://") {
        if let Some((authority, _path)) = raw.split_once(':') {
            if !authority.is_empty() && !authority.contains('/') {
                let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
                if !host.is_empty() {
                    return Ok(("ssh".to_string(), host.to_string()));
                }
            }
        }
    }
    // The rejected text is quoted back, so it goes through the mask first: a
    // remote that failed to parse can still have carried a credential, and this
    // message reaches both the client and the log.
    Err(crate::error::invalid_request(format!(
        "'{}' is not a valid git remote (expected an https/http/git URL)",
        mask_url_credentials(raw)
    )))
}

/// DNS-free static validation of a user-supplied **git remote** URL (mirror /
/// import). Enforces an allowed scheme ([`ALLOWED_GIT_URL_SCHEMES`]) and rejects
/// an obviously-internal IP-literal host. The git twin of [`check_url_static`];
/// use at create/update time for immediate operator feedback without a DNS
/// lookup that a transient outage could fail.
///
/// Typed like [`check_url_static`]: every rejection is about the remote the
/// caller supplied, so it carries [`crate::error::InvalidRequest`] and stays a
/// `400` without dragging the storage failure next to it down with it.
pub fn check_git_url_static(raw: &str) -> Result<()> {
    let (scheme, host) = split_git_remote(raw)?;
    if !ALLOWED_GIT_URL_SCHEMES.contains(&scheme.as_str()) {
        return Err(crate::error::invalid_request(format!(
            "git remote scheme '{scheme}' not allowed (only https/http/git) — \
             file://, ext:: and other transports are refused"
        )));
    }
    if host.is_empty() {
        return Err(crate::error::invalid_request("git remote URL has no host"));
    }
    // `host_str()` keeps brackets for IPv6 literals (`[::1]`); strip them so the
    // literal parses. A domain name never contains brackets.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(crate::error::invalid_request(format!(
                "git remote points at a forbidden (private/loopback/link-local) address: {ip}"
            )));
        }
    }
    Ok(())
}

/// Full SSRF guard for a user-supplied HTTP(S) **git remote**.
///
/// Unlike the old check-only contract, the result owns every approved DNS
/// answer and is the only value a clone/fetch sink accepts. The git invocation
/// turns those answers into `http.curloptResolve`, preserving the URL hostname
/// for HTTP Host, TLS SNI, and certificate verification while removing the
/// second system lookup that enabled DNS rebinding.
pub(crate) async fn guard_git_url(raw: &str) -> Result<GuardedGitRemote> {
    check_git_url_static(raw)?;
    resolve_git_http_remote(raw, Some(is_forbidden_ip)).await
}

/// Resolve an exact administrator-trusted import origin without weakening the
/// ordinary forbidden-address classifier. The explicit trust boundary chooses
/// this separate path; it still pins the answers to the subprocess.
pub(crate) async fn guard_trusted_git_url(raw: &str) -> Result<GuardedGitRemote> {
    resolve_git_http_remote(raw, None).await
}

async fn resolve_git_http_remote(
    raw: &str,
    is_forbidden: Option<fn(IpAddr) -> bool>,
) -> Result<GuardedGitRemote> {
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed)
        .map_err(|error| crate::error::invalid_request(format!("invalid git remote: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(crate::error::invalid_request(
            "outbound git destination binding supports only http or https",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| crate::error::invalid_request("git remote URL has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| crate::error::invalid_request("git remote URL has no effective port"))?;
    let literal = host.trim_start_matches('[').trim_end_matches(']');

    // IP-literal host: there is no DNS decision to bind. Trusted origins may
    // deliberately name a private literal; ordinary callers were classified by
    // check_git_url_static before entering this helper.
    if let Ok(address) = literal.parse::<IpAddr>() {
        if is_forbidden.is_some_and(|classifier| classifier(address)) {
            return Err(crate::error::invalid_request(format!(
                "git remote points at a forbidden (private/loopback/link-local) address: {address}"
            )));
        }
        return Ok(GuardedGitRemote {
            url: trimmed.to_owned(),
            binding: None,
        });
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("failed to resolve git remote host '{host}'"))?;

    let mut addresses = Vec::new();
    for addr in addrs {
        let address = addr.ip();
        if is_forbidden.is_some_and(|classifier| classifier(address)) {
            anyhow::bail!(
                "git remote host '{host}' resolves to a forbidden address: {}",
                address
            );
        }
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        anyhow::bail!("git remote host '{host}' did not resolve to any address");
    }

    Ok(GuardedGitRemote {
        url: trimmed.to_owned(),
        binding: Some(GitHttpBinding {
            host: host.to_owned(),
            port,
            addresses,
        }),
    })
}

#[cfg(test)]
pub(crate) fn guard_git_url_with_addresses(
    raw: &str,
    addresses: Vec<IpAddr>,
    is_forbidden: fn(IpAddr) -> bool,
) -> Result<GuardedGitRemote> {
    check_git_url_static(raw)?;
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed).context("test git remote URL")?;
    let host = url.host_str().context("test git remote host")?;
    let port = url
        .port_or_known_default()
        .context("test git remote effective port")?;
    if let Some(address) = addresses.iter().copied().find(|ip| is_forbidden(*ip)) {
        anyhow::bail!("git remote host '{host}' resolves to a forbidden address: {address}");
    }
    if addresses.is_empty() {
        anyhow::bail!("git remote host '{host}' did not resolve to any address");
    }
    Ok(GuardedGitRemote {
        url: trimmed.to_owned(),
        binding: Some(GitHttpBinding {
            host: host.to_owned(),
            port,
            addresses,
        }),
    })
}

// ── Credentials written inside the URL ──────────────────────────────────────
//
// A remote typed as `https://user:token@host/repo.git` carries its credential
// in the URL itself. Every column that holds such a URL — `mirrors.url`,
// `import_tasks.source_url`, `webhooks.url` — is plaintext, so the secret walks
// straight past the encryption the row's *own* credential column provides, and
// then out again through every response and error message that quotes the URL.
//
// These helpers are the one place that takes a credential back out of a URL, so
// the three call sites cannot drift apart on what counts as a secret:
// **the password is one, the login name is not**.

/// A user-supplied URL with the credential taken out of its userinfo section.
///
/// Two rewritten forms, because the callers differ in what they can store:
///
/// * [`url`](Self::url) — the whole `user[:password]@` gone. For a caller with
///   a column for the login (`mirrors.username`).
/// * [`url_without_secret`](Self::url_without_secret) — only the password gone,
///   `user@` kept. For a caller with nowhere to put a login
///   (`import_tasks.source_url`): a login name is not a secret, and leaving it
///   in the URL is what lets `git` authenticate at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlCredentials {
    /// The URL with the entire userinfo section removed.
    pub url: String,
    /// The URL with the password removed and the login kept.
    pub url_without_secret: String,
    /// The login half of the userinfo, when there was one to lift.
    pub username: Option<String>,
    /// The secret half of the userinfo.
    pub password: Option<String>,
}

impl UrlCredentials {
    /// Did the URL carry anything this type had to take out of it?
    pub fn is_present(&self) -> bool {
        self.username.is_some() || self.password.is_some()
    }
}

/// Take the credential out of a URL, rejecting the shapes that cannot be
/// stored safely. Use at create/update time, where the operator is present to
/// read the error and fix the input.
///
/// Two rejections, both of them cases where storing what was typed would leave
/// a usable secret in a plaintext column:
///
/// * a password on a non-`http(s)` transport (`ssh://u:p@host/…`) — `git` never
///   uses it, so it would be stored for nothing;
/// * a lone `user@` on `http(s)` — GitHub documents `https://<token>@host/…`,
///   and nothing in the URL distinguishes that token from a login name. Guessing
///   either way is wrong half the time, so the operator is asked to put the
///   credential in the password field instead.
pub fn split_url_credentials(raw: &str) -> Result<UrlCredentials> {
    let lifted = strip_url_credentials(raw);
    if !lifted.is_present() {
        return Ok(lifted);
    }

    let scheme = reqwest::Url::parse(raw.trim())
        .map(|url| url.scheme().to_string())
        .unwrap_or_default();
    if !matches!(scheme.as_str(), "http" | "https") {
        if lifted.password.is_some() {
            return Err(crate::error::invalid_request(format!(
                "a password written into a '{scheme}' URL is never used by git and would be \
                 stored in the clear — remove it from the URL"
            )));
        }
        return Ok(lifted);
    }

    if lifted.password.is_none() {
        return Err(crate::error::invalid_request(
            "the URL carries a credential with no password (`https://something@host/…`), and \
             nothing in it says whether `something` is a login name or a token — put the \
             credential in the password field instead (the username may be anything the remote \
             accepts, e.g. `x-access-token`)",
        ));
    }
    Ok(lifted)
}

/// The lenient twin of [`split_url_credentials`], for values already in the
/// database. Never rejects: a row is not an input an operator can correct, and
/// leaving the secret in place because its shape is ambiguous is the one
/// outcome the startup passes exist to prevent.
pub fn strip_url_credentials(raw: &str) -> UrlCredentials {
    let raw = raw.trim();
    let untouched = || UrlCredentials {
        url: raw.to_string(),
        url_without_secret: raw.to_string(),
        username: None,
        password: None,
    };

    // scp-like shorthand (`git@host:path`) and bare paths do not parse as a
    // URL. The shorthand's `user@` is an ssh login and has no password half, so
    // there is nothing in it to lift.
    let Ok(url) = reqwest::Url::parse(raw) else {
        return untouched();
    };
    let username = decode_userinfo(url.username());
    let password = url.password().map(decode_userinfo);
    if username.is_empty() && password.is_none() {
        return untouched();
    }

    // Only `http(s)` puts the userinfo to work as a credential (HTTP Basic).
    // On `ssh://` and `git://` the login belongs to the transport and must stay
    // in the URL; only a password is worth taking out.
    let http = matches!(url.scheme(), "http" | "https");

    let mut bare = url.clone();
    let stripped = bare
        .set_username("")
        .and_then(|()| bare.set_password(None))
        .is_ok();
    let mut without_secret = url.clone();
    let secret_stripped = without_secret.set_password(None).is_ok();

    if !stripped || !secret_stripped {
        // A URL that cannot hold a userinfo section cannot have had one, so
        // this is unreachable for anything `Url::parse` accepted with a
        // credential — but silently keeping the secret is not the failure mode
        // to pick if it ever is.
        tracing::warn!("a URL credential could not be rewritten out of its URL");
        return untouched();
    }

    UrlCredentials {
        url: if http {
            bare.to_string()
        } else {
            without_secret.to_string()
        },
        url_without_secret: without_secret.to_string(),
        username: if http {
            Some(username).filter(|u| !u.is_empty())
        } else {
            None
        },
        password,
    }
}

/// Percent-decode one half of a userinfo section back to what was typed.
///
/// A password with a `@` or `/` in it only survives a URL as `%40` / `%2F`;
/// storing the encoded form would hand `git` a credential the remote rejects.
/// Undecodable bytes are kept as they came — a credential that is not valid
/// UTF-8 is still the credential, and this is not the place to refuse it.
fn decode_userinfo(raw: &str) -> String {
    urlencoding::decode(raw)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// Replace the `user[:password]@` section of every URL in `text` with `***`.
///
/// The last net in front of text that is about to be persisted or logged —
/// `mirrors.last_sync_error`, `import_tasks.error`, a delivery record, a
/// `tracing` line — for a URL that still carries a credential: one an operator
/// typed before the create-time split existed, or one echoed back at us inside
/// a remote's own error text.
///
/// Masks the login as well as the password. It cannot tell a `git@` from a
/// token (that ambiguity is why [`split_url_credentials`] refuses the shape),
/// and a masked login costs one diagnostic detail while a leaked token costs
/// the account.
pub fn mask_url_credentials(text: &str) -> String {
    const AUTHORITY_END: &[char] = &['/', '?', '#', '"', '\'', '`', '<', '>', ')', ',', ';'];

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(marker) = rest.find("://") {
        let authority_start = marker + "://".len();
        out.push_str(&rest[..authority_start]);
        let tail = &rest[authority_start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || AUTHORITY_END.contains(&c))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        // `rfind`: a literal `@` inside a password has to be percent-encoded to
        // parse at all, so the last one is the userinfo separator.
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***");
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use reqwest::dns::Resolve;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    struct SequencedResolver {
        answers: Mutex<VecDeque<Vec<SocketAddr>>>,
        calls: Arc<AtomicUsize>,
    }

    impl SequencedResolver {
        fn new(answers: Vec<Vec<SocketAddr>>, calls: Arc<AtomicUsize>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                calls,
            }
        }
    }

    impl reqwest::dns::Resolve for SequencedResolver {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = self
                .answers
                .lock()
                .expect("scripted resolver lock poisoned")
                .pop_front();
            Box::pin(async move {
                let addrs = answer.ok_or_else(|| {
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "scripted resolver has no answer left",
                    )) as DnsError
                })?;
                Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
            })
        }
    }

    #[test]
    fn classifies_v4_internal() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "255.255.255.255",
            "100.64.0.1", // CGNAT
            "224.0.0.1",  // multicast
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_forbidden_ip(ip), "{s} should be forbidden");
        }
    }

    #[test]
    fn allows_v4_public() {
        for s in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!is_forbidden_ip(ip), "{s} should be allowed");
        }
    }

    #[test]
    fn classifies_v6_internal() {
        for s in ["::1", "::", "fe80::1", "fc00::1", "fd12:3456::1", "ff02::1"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_forbidden_ip(ip), "{s} should be forbidden");
        }
        // IPv4-mapped loopback must fold back to the v4 check.
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(
            is_forbidden_ip(mapped),
            "mapped loopback should be forbidden"
        );
    }

    #[test]
    fn allows_v6_public() {
        let ip: IpAddr = "2606:4700:4700::1111".parse().unwrap();
        assert!(!is_forbidden_ip(ip), "public v6 should be allowed");
    }

    #[test]
    fn rejects_bad_scheme_and_literals() {
        assert!(check_url_static("file:///etc/passwd").is_err());
        assert!(check_url_static("ftp://example.com/x").is_err());
        assert!(check_url_static("gopher://127.0.0.1/").is_err());
        assert!(check_url_static("http://127.0.0.1/hook").is_err());
        assert!(check_url_static("http://169.254.169.254/latest/meta-data").is_err());
        assert!(check_url_static("http://[::1]/hook").is_err());
        assert!(check_url_static("http://10.0.0.5:8080/hook").is_err());
        // Public literal is fine.
        assert!(check_url_static("https://1.1.1.1/hook").is_ok());
        assert!(check_url_static("https://example.com/hook").is_ok());
    }

    #[tokio::test]
    async fn a_rebinding_resolver_never_returns_its_forbidden_second_answer() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = ForbiddenAddressResolver::new(SequencedResolver::new(
            vec![
                vec!["93.184.216.34:443".parse().unwrap()],
                vec!["169.254.169.254:80".parse().unwrap()],
            ],
            Arc::clone(&calls),
        ));

        let first = resolver
            .resolve("rebind.test".parse().unwrap())
            .await
            .expect("the public answer should reach the connector")
            .collect::<Vec<_>>();
        assert_eq!(first, vec!["93.184.216.34:443".parse().unwrap()]);

        let error = match resolver.resolve("rebind.test".parse().unwrap()).await {
            Ok(_) => panic!("the rebinding answer reached the connector"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("169.254.169.254"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reqwest_connects_only_to_the_answer_checked_by_its_resolver() {
        let public_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = public_listener.local_addr().unwrap().port();
        let rebound_ip = Ipv4Addr::new(127, 0, 0, 2);
        let rebound_listener = tokio::net::TcpListener::bind((rebound_ip, port))
            .await
            .unwrap();

        let public_request = tokio::spawn(async move {
            let (mut stream, _) = public_listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /hook HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        });

        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = SequencedResolver::new(
            vec![
                vec![SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)],
                vec![SocketAddr::new(rebound_ip.into(), 0)],
            ],
            Arc::clone(&calls),
        );
        // Loopback stands in for a reachable public address in this live test;
        // production `is_forbidden_ip` coverage above proves the real policy.
        // The second loopback address stands in for the rebinding target.
        let test_classifier: fn(IpAddr) -> bool =
            |ip| ip == IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
        let client = ssrf_safe_client_builder(ForbiddenAddressResolver::with_classifier(
            resolver,
            test_classifier,
        ))
        .build()
        .unwrap();
        let url = format!("http://rebind.test:{port}/hook");

        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        public_request.await.unwrap();

        client
            .get(&url)
            .send()
            .await
            .expect_err("the second DNS answer must be rejected before connect");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rebound_listener.accept())
                .await
                .is_err(),
            "the connector reached the forbidden rebound address"
        );
    }

    // ── Git-remote guard ────────────────────────────────────────────────────

    #[test]
    fn git_url_rejects_dangerous_schemes_and_local_paths() {
        // Local-file / transport-helper / other transports — refused outright.
        assert!(check_git_url_static("file:///etc/passwd").is_err());
        assert!(check_git_url_static("ext::sh -c 'id'").is_err());
        assert!(check_git_url_static("ftp://example.com/repo.git").is_err());
        // Bare local path (no scheme, no host:) — no remote host.
        assert!(check_git_url_static("/srv/git/repo.git").is_err());
        assert!(check_git_url_static("./relative/repo.git").is_err());
        assert!(check_git_url_static("").is_err());
    }

    #[test]
    fn git_url_rejects_internal_ip_literals() {
        for s in [
            "https://127.0.0.1/r.git",
            "http://169.254.169.254/r.git", // cloud metadata
            "git://10.0.0.5/r.git",
            "ssh://git@192.168.1.1/r.git",
            "git://[::1]/r.git",
            "https://[fd00::1]/r.git",
        ] {
            assert!(check_git_url_static(s).is_err(), "{s} should be rejected");
        }
    }

    #[test]
    fn git_url_allows_public_forms() {
        for s in [
            "https://github.com/owner/repo.git",
            "http://example.com/repo.git",
            "git://example.com/repo.git",
            "https://1.1.1.1/repo.git", // public IP literal
        ] {
            assert!(check_git_url_static(s).is_ok(), "{s} should be allowed");
        }
    }

    #[test]
    fn git_url_rejects_ssh_and_scp_like_remotes_explicitly() {
        for remote in [
            "ssh://git@example.com:22/owner/repo.git",
            "git@github.com:owner/repo.git",
        ] {
            let error = check_git_url_static(remote).expect_err("ssh must be disabled");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("scheme 'ssh' not allowed"),
                "the rejection did not explain the disabled transport: {rendered}"
            );
        }
    }

    // ── Credentials inside the URL ──────────────────────────────────────────

    #[test]
    fn a_user_password_url_is_split_into_a_bare_url_and_its_two_halves() {
        let split = split_url_credentials("https://sync-bot:ghp_SECRET@example.com/o/r.git")
            .expect("a `user:password@` URL is the shape that can be stored safely");

        assert_eq!(split.url, "https://example.com/o/r.git");
        assert_eq!(
            split.url_without_secret,
            "https://sync-bot@example.com/o/r.git"
        );
        assert_eq!(split.username.as_deref(), Some("sync-bot"));
        assert_eq!(split.password.as_deref(), Some("ghp_SECRET"));
        // Neither rewritten form may still carry the secret — that is the whole
        // point of the split.
        assert!(!split.url.contains("ghp_SECRET"));
        assert!(!split.url_without_secret.contains("ghp_SECRET"));
    }

    #[test]
    fn a_percent_encoded_password_is_decoded_back_to_what_was_typed() {
        // `@` and `/` only survive a URL encoded; storing the encoded form would
        // hand git a credential the remote rejects.
        let split = split_url_credentials("https://bot:p%40ss%2Fword@example.com/r.git").unwrap();
        assert_eq!(split.password.as_deref(), Some("p@ss/word"));
    }

    #[test]
    fn a_url_without_a_credential_is_returned_untouched() {
        for raw in [
            "https://example.com/o/r.git",
            "git@github.com:owner/repo.git", // scp-like: an ssh login, no secret
            "ssh://git@example.com/o/r.git", // ditto, in URL form
            "git://example.com/r.git",
        ] {
            let split = split_url_credentials(raw).expect("no credential to reject");
            assert!(!split.is_present(), "{raw} was treated as carrying one");
            assert_eq!(split.url, raw);
            assert_eq!(split.url_without_secret, raw);
        }
    }

    #[test]
    fn a_lone_userinfo_on_http_is_refused_rather_than_guessed() {
        // `https://<token>@host/…` is GitHub's documented paste form, and it is
        // also what a login name looks like. Storing it as a username would put
        // a token in a plaintext column; storing it as a password would break a
        // login. The operator is asked instead.
        let error = split_url_credentials("https://ghp_SECRET@github.com/o/r.git")
            .expect_err("an ambiguous credential must not be stored");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("password field"),
            "the rejection must say what to do instead: {rendered}"
        );
        assert!(
            !rendered.contains("ghp_SECRET"),
            "the rejection quoted the credential back: {rendered}"
        );
    }

    #[test]
    fn a_password_on_a_transport_that_cannot_use_one_is_refused() {
        let error = split_url_credentials("ssh://git:hunter2@example.com/o/r.git")
            .expect_err("git never uses a password from an ssh URL");
        assert!(format!("{error:#}").contains("ssh"));
    }

    #[test]
    fn the_lenient_strip_never_refuses_a_stored_row() {
        // A lone userinfo — refused on the way in, still has to be taken out of
        // a row written before that check existed.
        let lifted = strip_url_credentials("https://ghp_SECRET@github.com/o/r.git");
        assert_eq!(lifted.url, "https://github.com/o/r.git");
        assert_eq!(lifted.username.as_deref(), Some("ghp_SECRET"));
        assert_eq!(lifted.password, None);

        // An ssh login stays in the URL; only the password is worth lifting.
        let lifted = strip_url_credentials("ssh://git:hunter2@example.com/o/r.git");
        assert_eq!(lifted.url, "ssh://git@example.com/o/r.git");
        assert_eq!(lifted.username, None);
        assert_eq!(lifted.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn masking_removes_the_userinfo_and_keeps_the_rest_of_the_message() {
        assert_eq!(
            mask_url_credentials(
                "fatal: could not read from https://bot:ghp_SECRET@example.com/o/r.git"
            ),
            "fatal: could not read from https://***@example.com/o/r.git"
        );
        // Several URLs in one message, and one without a credential.
        assert_eq!(
            mask_url_credentials("a https://u:p@a.example/x and b https://b.example/y"),
            "a https://***@a.example/x and b https://b.example/y"
        );
        // Idempotent: masking an already-masked message changes nothing.
        let once = mask_url_credentials("https://u:p@host/x");
        assert_eq!(mask_url_credentials(&once), once);
        // Nothing that looks like a URL: untouched.
        assert_eq!(mask_url_credentials("no urls here"), "no urls here");
        // A `@` after the authority (an e-mail in a path) is not userinfo.
        assert_eq!(
            mask_url_credentials("https://example.com/u/a@b.com"),
            "https://example.com/u/a@b.com"
        );
    }

    #[test]
    fn a_rejected_remote_is_quoted_back_without_its_credential() {
        // `ext::` is refused for what it is; the refusal must not repeat the
        // token that was sitting in the URL.
        let error = check_git_url_static("ext::https://u:ghp_SECRET@example.com/r.git")
            .expect_err("ext:: is not an allowed transport");
        assert!(!format!("{error:#}").contains("ghp_SECRET"));
    }

    #[tokio::test]
    async fn guard_git_url_blocks_internal_targets() {
        assert!(guard_git_url("https://127.0.0.1/r.git").await.is_err());
        assert!(guard_git_url("git://[::1]/r.git").await.is_err());
        // `localhost` is a domain, caught only by the DNS-resolution stage.
        assert!(guard_git_url("http://localhost/r.git").await.is_err());
        assert!(guard_git_url("git@localhost:owner/repo.git").await.is_err());
        // Public IP literal passes without DNS.
        assert!(guard_git_url("https://1.1.1.1/r.git").await.is_ok());
    }
}
