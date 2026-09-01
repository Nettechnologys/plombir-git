//! Operator-owned transport policy for repository mirrors.
//!
//! Repository owners choose mirror URLs, but the instance operator owns the
//! network and the credentials ForgeKeep presents to those remotes. Plain HTTP
//! is therefore a separate instance-level decision, not something a repository
//! owner can enable by spelling `http://` in a settings form.

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

    /// Reject plaintext HTTP without performing DNS resolution.
    ///
    /// Kept separate from the broader URL guard so the final git invocation can
    /// repeat this confidentiality check without pretending it owns SSRF. The
    /// caller immediately above that sink still performs the full DNS guard.
    pub fn require_confidential_http(self, raw: &str) -> Result<()> {
        let url = reqwest::Url::parse(raw).map_err(|error| {
            crate::error::invalid_request(format!("invalid mirror URL: {error}"))
        })?;
        if url.scheme() == "http" && !self.allow_insecure_http {
            return Err(crate::error::invalid_request(
                "plaintext HTTP mirror remotes are disabled; use https:// or set \
                 `[mirror].allow_insecure_http = true` as an explicit instance-operator \
                 exception",
            ));
        }
        Ok(())
    }

    /// Create/update check: git scheme/host validation plus transport policy.
    pub fn validate_url_static(self, raw: &str) -> Result<()> {
        crate::net::check_git_url_static(raw).context("invalid mirror URL")?;
        self.require_confidential_http(raw)
    }

    /// Sync-time check for legacy rows and DNS changes immediately before git.
    pub async fn guard_url(self, raw: &str) -> Result<()> {
        self.require_confidential_http(raw)?;
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
}
