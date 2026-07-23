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
//! - [`guard_outbound_url`] — reject non-`http(s)` schemes and any target that
//!   resolves to a private / loopback / link-local / ULA / CGNAT address, so a
//!   user can't point a webhook straight at `127.0.0.1`, `10.0.0.x`,
//!   `169.254.169.254`, `[::1]`, etc.
//!
//! The redirect ban + pre-send resolution check together cover the two classic
//! webhook SSRF vectors (direct-internal-URL and redirect-to-internal). A
//! residual DNS-rebind TOCTOU (host resolves public at check time, internal at
//! connect time) is not fully closed by a shared client; the redirect ban plus
//! the resolution guard reduce it to the narrow rebind-within-the-connect-window
//! case, which is documented rather than silently ignored.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};

/// Default request timeout for outbound HTTP (whole request, including body).
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connect timeout for outbound HTTP (TCP + TLS handshake only).
const OUTBOUND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A `reqwest::ClientBuilder` pre-seeded with the outbound `timeout` +
/// `connect_timeout`, so a slow/hanging peer can't pin a task forever.
///
/// This is the shared floor for **any** long-lived outbound client. It sets
/// only the timeouts — no redirect policy, no user-agent, no default headers —
/// so each caller layers on what it needs:
///
/// - [`outbound_client`] adds `redirect(Policy::none())` + a webhook UA for
///   user-supplied webhook/mirror targets (paired with [`guard_outbound_url`]).
/// - The import clients (`GitHubClient` / `GitLabClient`) add their per-instance
///   auth headers (`Bearer` / `PRIVATE-TOKEN`) + UA. They deliberately keep the
///   reqwest **default** redirect policy — API hosts legitimately 3xx (e.g. a
///   renamed repo) — and do **not** run `guard_outbound_url`, because a
///   self-hosted GitHub Enterprise / GitLab `base_url` on a private IP is a
///   legitimate admin-configured target, exactly like an internal SSO IdP.
pub fn outbound_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(OUTBOUND_TIMEOUT)
        .connect_timeout(OUTBOUND_CONNECT_TIMEOUT)
}

/// Shared, SSRF-hardened outbound HTTP client for user-supplied targets.
///
/// Built once and reused: `timeout` + `connect_timeout` bound every request,
/// and redirects are **not** followed (a `3xx` is returned as-is).
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
pub fn check_url_static(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw).context("invalid outbound URL")?;
    match url.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!("outbound URL scheme '{other}' not allowed (only http/https)"),
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("outbound URL has no host"))?;
    // `host_str()` keeps brackets for IPv6 literals (`[::1]`); strip them so
    // the literal parses. A domain name never contains brackets.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            anyhow::bail!("outbound URL points at a forbidden (private/loopback/link-local) address: {ip}");
        }
    }
    Ok(())
}

/// Full SSRF guard for the delivery path: [`check_url_static`] **plus** DNS
/// resolution — reject if *any* resolved address is internal. Call this
/// immediately before sending to a user-supplied URL.
pub async fn guard_outbound_url(raw: &str) -> Result<()> {
    check_url_static(raw)?;

    let url = reqwest::Url::parse(raw).context("invalid outbound URL")?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("outbound URL has no host"))?;
    let literal = host.trim_start_matches('[').trim_end_matches(']');

    // IP-literal host: already fully validated by check_url_static, no DNS.
    if literal.parse::<IpAddr>().is_ok() {
        return Ok(());
    }

    // Domain host: resolve and reject if ANY address is internal.
    let port = url.port_or_known_default().unwrap_or(0);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("failed to resolve outbound host '{host}'"))?;

    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_forbidden_ip(addr.ip()) {
            anyhow::bail!(
                "outbound host '{host}' resolves to a forbidden address: {}",
                addr.ip()
            );
        }
    }
    if !saw_any {
        anyhow::bail!("outbound host '{host}' did not resolve to any address");
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
pub const ALLOWED_GIT_URL_SCHEMES: &[&str] = &["https", "http", "git", "ssh"];

/// Split a git remote into `(scheme, host)`.
///
/// Handles both the URL forms git understands (`https://`, `http://`, `git://`,
/// `ssh://`) and the scp-like shorthand `[user@]host:path`, which git treats as
/// ssh. A bare local path (no scheme, no `host:` prefix) yields no host and is
/// rejected by the caller.
fn split_git_remote(raw: &str) -> Result<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("git remote URL is empty");
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
    anyhow::bail!(
        "'{raw}' is not a valid git remote (expected an https/http/git/ssh URL or scp-like host:path)"
    );
}

/// DNS-free static validation of a user-supplied **git remote** URL (mirror /
/// import). Enforces an allowed scheme ([`ALLOWED_GIT_URL_SCHEMES`]) and rejects
/// an obviously-internal IP-literal host. The git twin of [`check_url_static`];
/// use at create/update time for immediate operator feedback without a DNS
/// lookup that a transient outage could fail.
pub fn check_git_url_static(raw: &str) -> Result<()> {
    let (scheme, host) = split_git_remote(raw)?;
    if !ALLOWED_GIT_URL_SCHEMES.contains(&scheme.as_str()) {
        anyhow::bail!(
            "git remote scheme '{scheme}' not allowed (only https/http/git/ssh) — \
             file://, ext:: and other transports are refused"
        );
    }
    if host.is_empty() {
        anyhow::bail!("git remote URL has no host");
    }
    // `host_str()` keeps brackets for IPv6 literals (`[::1]`); strip them so the
    // literal parses. A domain name never contains brackets.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            anyhow::bail!(
                "git remote points at a forbidden (private/loopback/link-local) address: {ip}"
            );
        }
    }
    Ok(())
}

/// Full SSRF guard for a user-supplied **git remote**: [`check_git_url_static`]
/// **plus** DNS resolution — reject if *any* resolved address is internal. The
/// git twin of [`guard_outbound_url`]; call immediately before spawning the
/// clone/fetch subprocess.
pub async fn guard_git_url(raw: &str) -> Result<()> {
    check_git_url_static(raw)?;

    let (_scheme, host) = split_git_remote(raw)?;
    let literal = host.trim_start_matches('[').trim_end_matches(']');

    // IP-literal host: already fully validated by check_git_url_static, no DNS.
    if literal.parse::<IpAddr>().is_ok() {
        return Ok(());
    }

    // Domain host: resolve and reject if ANY address is internal. The port is
    // irrelevant to address classification, so resolve on port 0.
    let addrs = tokio::net::lookup_host((host.as_str(), 0))
        .await
        .with_context(|| format!("failed to resolve git remote host '{host}'"))?;

    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_forbidden_ip(addr.ip()) {
            anyhow::bail!(
                "git remote host '{host}' resolves to a forbidden address: {}",
                addr.ip()
            );
        }
    }
    if !saw_any {
        anyhow::bail!("git remote host '{host}' did not resolve to any address");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(is_forbidden_ip(mapped), "mapped loopback should be forbidden");
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
    async fn guard_blocks_ip_literal_targets() {
        assert!(guard_outbound_url("http://127.0.0.1/hook").await.is_err());
        assert!(guard_outbound_url("http://169.254.169.254/latest/meta-data")
            .await
            .is_err());
        assert!(guard_outbound_url("http://[::1]/hook").await.is_err());
        // Public IP literal: passes the guard (no DNS needed).
        assert!(guard_outbound_url("https://1.1.1.1/hook").await.is_ok());
    }

    #[tokio::test]
    async fn guard_blocks_localhost_by_resolution() {
        // `localhost` is a domain, caught only by the DNS-resolution stage.
        assert!(guard_outbound_url("http://localhost/hook").await.is_err());
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
            "ssh://git@example.com:22/owner/repo.git",
            "git@github.com:owner/repo.git", // scp-like → ssh
            "https://1.1.1.1/repo.git",      // public IP literal
        ] {
            assert!(check_git_url_static(s).is_ok(), "{s} should be allowed");
        }
    }

    #[test]
    fn git_url_scp_shorthand_parses_to_ssh_host() {
        assert_eq!(
            split_git_remote("git@github.com:owner/repo.git").unwrap(),
            ("ssh".to_string(), "github.com".to_string())
        );
        // scp-like pointing at an internal literal is caught statically.
        assert!(check_git_url_static("git@127.0.0.1:owner/repo.git").is_err());
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
