//! Process-wide transport policy for outbound webhook deliveries.
//!
//! Webhook dispatch is detached from the request that caused it and can start
//! from issue, pull-request, release, or git-push code. Threading one operator
//! setting through every one of those service signatures would make unrelated
//! domains own webhook configuration. ForgeKeep serves one instance per
//! process, so the resolved policy is published once at server start and read
//! only at the final delivery boundary.
//!
//! Request-time create/update validation still receives the policy explicitly
//! through `AppState`; the process-wide copy exists to re-check legacy rows
//! immediately before network I/O. If nothing was published (unit tests and
//! embedders), the secure default remains in force.

use std::sync::OnceLock;

use anyhow::{Context, Result};

/// Operator-owned policy for outbound webhook transports.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WebhookTransportPolicy {
    allow_insecure_http: bool,
}

impl WebhookTransportPolicy {
    /// Build the policy resolved from `[webhooks].allow_insecure_http`.
    pub const fn new(allow_insecure_http: bool) -> Self {
        Self {
            allow_insecure_http,
        }
    }

    /// Whether plaintext HTTP targets are an explicit operator exception.
    pub const fn allows_insecure_http(self) -> bool {
        self.allow_insecure_http
    }

    /// DNS-free create/update check for one webhook target.
    pub fn validate_url_static(self, raw: &str) -> Result<()> {
        crate::net::check_url_static(raw).context("invalid webhook URL")?;
        let url = reqwest::Url::parse(raw).map_err(|error| {
            crate::error::invalid_request(format!("invalid webhook URL: {error}"))
        })?;
        if url.scheme() == "http" && !self.allow_insecure_http {
            return Err(crate::error::invalid_request(
                "plaintext HTTP webhook targets are disabled; use https:// or set \
                 `[webhooks].allow_insecure_http = true` as an explicit operator exception",
            ));
        }
        Ok(())
    }

    /// Delivery-time check: transport policy plus the shared DNS SSRF guard.
    pub async fn guard_url(self, raw: &str) -> Result<()> {
        self.validate_url_static(raw)?;
        crate::net::guard_outbound_url(raw).await
    }
}

static TRANSPORT_POLICY: OnceLock<WebhookTransportPolicy> = OnceLock::new();

/// Publish the policy for detached deliveries in this process.
///
/// Re-publishing the same value is harmless. A different second value cannot
/// replace the first: one process serving two instances with conflicting
/// security policy is unsupported, and silently changing the delivery rule
/// underneath already-running tasks would be worse than keeping the first.
pub fn publish(policy: WebhookTransportPolicy) {
    if let Err(_ignored) = TRANSPORT_POLICY.set(policy) {
        if TRANSPORT_POLICY.get().copied() != Some(policy) {
            tracing::warn!(
                "a second, different webhook transport policy was published to this process; \
                 keeping the first one"
            );
        }
    }
}

/// Policy used by detached delivery. Unpublished means secure defaults.
pub fn current() -> WebhookTransportPolicy {
    TRANSPORT_POLICY.get().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_http_is_refused_until_the_operator_opts_in() {
        let error = WebhookTransportPolicy::default()
            .validate_url_static("http://example.com/hook")
            .expect_err("secure default must reject plaintext webhook transport");
        let typed = error
            .downcast_ref::<crate::error::InvalidRequest>()
            .expect("transport rejection remains an HTTP 400");
        assert!(typed.message.contains("allow_insecure_http"));

        WebhookTransportPolicy::new(true)
            .validate_url_static("http://example.com/hook")
            .expect("the explicit operator exception admits public HTTP targets");
        WebhookTransportPolicy::default()
            .validate_url_static("https://example.com/hook")
            .expect("HTTPS is the secure default transport");
    }

    #[test]
    fn the_http_exception_does_not_disable_the_ssrf_boundary() {
        let error = WebhookTransportPolicy::new(true)
            .validate_url_static("http://127.0.0.1/hook")
            .expect_err("transport opt-in must not admit loopback targets");
        assert!(error
            .downcast_ref::<crate::error::InvalidRequest>()
            .is_some());
    }
}
