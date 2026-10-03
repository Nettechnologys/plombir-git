//! Operator-owned transport policy for repository mirrors.
//!
//! Repository owners choose mirror URLs, but the instance operator owns the
//! network, the fetched repository content, and the credentials Plombir Git
//! presents to those remotes. Plain HTTP is therefore a separate instance-level
//! decision, not something a repository owner can enable by spelling `http://`
//! in a settings form. The native `git://` protocol has no encrypted mode and
//! is refused outright rather than inheriting that HTTP-only exception.

use anyhow::{Context, Result};

/// Whether this instance deliberately permits plaintext HTTP mirror remotes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MirrorTransportPolicy {
    allow_insecure_http: bool,
}

impl MirrorTransportPolicy {
    /// Build the policy resolved from `[mirror].allow_insecure_http`.
    pub const fn new(allow_insecure_http: bool) -> Self {
        Self {
            allow_insecure_http,
        }
    }

    /// Whether plaintext HTTP is an explicit operator exception.
    pub const fn allows_insecure_http(self) -> bool {
        self.allow_insecure_http
    }

    /// Reject an unapproved plaintext transport without performing DNS resolution.
    ///
    /// Kept separate from the broader URL guard so the final git invocation can
    /// repeat this confidentiality check without pretending it owns SSRF. The
    /// caller immediately above that sink still performs the full DNS guard.
    pub fn require_confidential_transport(self, raw: &str) -> Result<()> {
        let url = reqwest::Url::parse(raw).map_err(|error| {
            crate::error::invalid_request(format!("invalid mirror URL: {error}"))
        })?;
        match url.scheme() {
            "git" => {
                return Err(crate::error::invalid_request(
                    "plaintext native Git protocol mirror remotes are disabled; use https:// \
                     (`[mirror].allow_insecure_http` is an HTTP-only exception and does not \
                     enable git://)",
                ));
            }
            "http" if !self.allow_insecure_http => {
                return Err(crate::error::invalid_request(
                    "plaintext HTTP mirror remotes are disabled; use https:// or set \
                     `[mirror].allow_insecure_http = true` as an explicit instance-operator \
                     exception",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// Create/update check: git scheme/host validation plus transport policy.
    pub fn validate_url_static(self, raw: &str) -> Result<()> {
        crate::net::check_git_url_static(raw).context("invalid mirror URL")?;
        self.require_confidential_transport(raw)
    }

    /// Resolve and bind a sync-time destination immediately before git.
    pub(crate) async fn destination(self, raw: &str) -> Result<crate::net::GuardedGitRemote> {
        self.require_confidential_transport(raw)?;
        crate::net::guard_git_url(raw).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_http_is_refused_until_the_operator_opts_in() {
        let error = MirrorTransportPolicy::default()
            .validate_url_static("http://example.com/upstream.git")
            .expect_err("secure default must reject plaintext mirror transport");
        let typed = error
            .downcast_ref::<crate::error::InvalidRequest>()
            .expect("transport rejection remains an HTTP 400");
        assert!(typed.message.contains("allow_insecure_http"));

        MirrorTransportPolicy::new(true)
            .validate_url_static("http://example.com/upstream.git")
            .expect("the explicit operator exception admits public HTTP remotes");
        MirrorTransportPolicy::default()
            .validate_url_static("https://example.com/upstream.git")
            .expect("HTTPS remains the secure default transport");
    }

    #[test]
    fn the_http_exception_does_not_disable_the_ssrf_boundary() {
        let error = MirrorTransportPolicy::new(true)
            .validate_url_static("http://127.0.0.1/upstream.git")
            .expect_err("transport opt-in must not admit loopback targets");
        assert!(error
            .downcast_ref::<crate::error::InvalidRequest>()
            .is_some());
    }

    #[tokio::test]
    async fn native_git_protocol_is_always_refused_before_dns() {
        for policy in [
            MirrorTransportPolicy::default(),
            MirrorTransportPolicy::new(true),
        ] {
            let error = match policy
                .destination("git://does-not-resolve.invalid/upstream.git")
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("native Git has no confidential mode or operator exception"),
            };
            let typed = error
                .downcast_ref::<crate::error::InvalidRequest>()
                .expect("transport rejection remains an HTTP 400");
            assert!(typed.message.contains("git://"));
            assert!(typed.message.contains("HTTP-only"));
        }
    }
}
