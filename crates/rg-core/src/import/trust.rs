//! Administrative trust boundary for private self-hosted import sources.
//!
//! Import URLs normally remain user-controlled and therefore pass the full
//! private-address SSRF guard. An operator may opt specific HTTP(S) origins
//! into this policy; the exception is exact on scheme, host, and effective
//! port, and never acts as a wildcard for other private-network destinations.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::net::HttpOrigin as ImportOrigin;

/// Exact origins an administrator has allowed imports to reach even when they
/// resolve to a private, loopback, or link-local address.
#[derive(Clone, Debug, Default)]
pub struct TrustedImportOrigins(Arc<HashSet<ImportOrigin>>);

impl TrustedImportOrigins {
    /// Parse the operator-facing `[imports].trusted_origins` values.
    pub fn parse(values: &[String]) -> Result<Self> {
        let mut origins = HashSet::with_capacity(values.len());
        for value in values {
            origins.insert(ImportOrigin::from_config(value, "trusted import origin")?);
        }
        Ok(Self(Arc::new(origins)))
    }

    fn contains(&self, raw: &str) -> bool {
        ImportOrigin::from_target(raw).is_some_and(|origin| self.0.contains(&origin))
    }

    /// DNS-free request-time check. Trusted origins bypass only the private-IP
    /// classification; malformed URLs and non-HTTP(S) trust entries never do.
    pub fn check_url_static(&self, raw: &str) -> Result<()> {
        if self.contains(raw) {
            return Ok(());
        }
        crate::net::check_git_url_static(raw)
    }

    /// Full pre-network import guard. An exact configured origin is the only
    /// path around DNS/private-address rejection.
    pub async fn guard_url(&self, raw: &str) -> Result<()> {
        if self.contains(raw) {
            return Ok(());
        }
        crate::net::guard_git_url(raw).await
    }
}

/// Import-owned transport confidentiality policy.
///
/// Exact HTTP origins name where an administrator explicitly permits an import
/// credential to cross a plaintext transport. Native `git://` has no encrypted
/// mode and is refused for every import, independently of those origins.
///
/// This is deliberately independent from [`TrustedImportOrigins`]. An origin
/// that may bypass private-address SSRF rejection has not thereby been granted
/// authority to expose a PAT to the network, and the converse does not make a
/// private destination reachable.
#[derive(Clone, Debug, Default)]
pub struct ImportTransportPolicy(Arc<HashSet<ImportOrigin>>);

impl ImportTransportPolicy {
    /// Parse `[imports].allow_insecure_http_origins` as exact HTTP origins.
    pub fn parse(values: &[String]) -> Result<Self> {
        let mut origins = HashSet::with_capacity(values.len());
        for value in values {
            let origin = ImportOrigin::from_config(value, "plaintext import opt-in origin")?;
            if origin.scheme != "http" {
                anyhow::bail!(
                    "plaintext import opt-in origin '{}' must use http",
                    value.trim()
                );
            }
            origins.insert(origin);
        }
        Ok(Self(Arc::new(origins)))
    }

    /// Whether at least one plaintext credential origin was explicitly named.
    pub fn allows_insecure_http(&self) -> bool {
        !self.0.is_empty()
    }

    /// Reject native Git before an import reaches DNS, an API, or a git sink.
    ///
    /// This rule has no configurable state: `[imports].allow_insecure_http_origins`
    /// names exact HTTP origins and [`TrustedImportOrigins`] grants reachability,
    /// not confidentiality. Neither is authority to enable another plaintext
    /// protocol. Malformed and otherwise-disallowed URLs remain owned by the
    /// ordinary import URL guard.
    pub(crate) fn require_confidential_transport(raw: &str) -> Result<()> {
        let Ok(url) = reqwest::Url::parse(raw.trim()) else {
            return Ok(());
        };
        if url.scheme() == "git" {
            return Err(crate::error::invalid_request(
                "plaintext native Git protocol imports are disabled; use https:// \
                 (`[imports].allow_insecure_http_origins` is an HTTP-only exception and \
                 `[imports].trusted_origins` does not enable git://)",
            ));
        }
        Ok(())
    }

    /// Reject an unsafe source before any API or git I/O.
    ///
    /// `raw` may still contain legacy URL userinfo at request admission. A
    /// password there is a source credential too, even if `supplied_token` is
    /// absent; [`crate::net::split_url_credentials`] removes it before storage.
    pub fn require_confidential_credentials(
        &self,
        raw: &str,
        supplied_token: Option<&str>,
    ) -> Result<()> {
        Self::require_confidential_transport(raw)?;

        let parsed = reqwest::Url::parse(raw).ok();
        let carries_credential = supplied_token.is_some_and(|token| !token.is_empty())
            || parsed
                .as_ref()
                .and_then(reqwest::Url::password)
                .is_some_and(|password| !password.is_empty());
        if !carries_credential {
            return Ok(());
        }

        let Some(origin) = parsed.as_ref().and_then(ImportOrigin::from_url) else {
            // The ordinary import URL guard owns malformed/non-HTTP schemes.
            return Ok(());
        };
        if origin.scheme == "http" && !self.0.contains(&origin) {
            return Err(crate::error::invalid_request(
                "plaintext HTTP imports may not carry credentials; use https:// or add the \
                 exact origin to `[imports].allow_insecure_http_origins` as a separate \
                 instance-operator exception",
            ));
        }
        Ok(())
    }
}

/// Require two HTTP(S) URLs to have the same scheme, host, and effective port.
/// Paths are deliberately irrelevant: a GitLab source, `/api/v4`, and its
/// API-returned clone URL are different resources at one credential origin.
pub fn require_same_origin(expected: &str, candidate: &str) -> Result<()> {
    let expected_origin = ImportOrigin::from_target(expected)
        .ok_or_else(|| anyhow::anyhow!("expected import URL has no HTTP(S) origin"))?;
    let candidate_origin = ImportOrigin::from_target(candidate).ok_or_else(|| {
        anyhow::anyhow!("import URL returned by the source has no HTTP(S) origin")
    })?;
    if candidate_origin != expected_origin {
        anyhow::bail!(
            "import URL returned by the source changed credential origin (scheme, host, or port)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(value: &str) -> TrustedImportOrigins {
        TrustedImportOrigins::parse(&[value.to_owned()]).expect("valid trusted origin")
    }

    fn plaintext_configured(value: &str) -> ImportTransportPolicy {
        ImportTransportPolicy::parse(&[value.to_owned()]).expect("valid plaintext transport origin")
    }

    #[test]
    fn one_exact_origin_covers_clone_and_api_paths_but_not_neighbors() {
        let trusted = configured("http://127.0.0.1:8443");

        trusted
            .check_url_static("http://127.0.0.1:8443/group/project.git")
            .expect("clone URL on the configured origin");
        trusted
            .check_url_static("http://127.0.0.1:8443/api/v4/projects/group%2Fproject")
            .expect("API URL on the configured origin");

        assert!(trusted
            .check_url_static("http://127.0.0.1:9443/group/project.git")
            .is_err());
        assert!(trusted
            .check_url_static("http://127.0.0.2:8443/group/project.git")
            .is_err());
        assert!(trusted
            .check_url_static("https://127.0.0.1:8443/group/project.git")
            .is_err());
    }

    #[tokio::test]
    async fn private_origin_needs_the_admin_configuration() {
        let raw = "http://127.0.0.1:8443/group/project.git";
        assert!(TrustedImportOrigins::default()
            .guard_url(raw)
            .await
            .is_err());
        configured("http://127.0.0.1:8443")
            .guard_url(raw)
            .await
            .expect("the exact operator-approved private origin");
    }

    #[test]
    fn configured_values_are_origins_not_urls_or_patterns() {
        for invalid in [
            "ssh://gitlab.internal",
            "https://*.internal.example",
            "https://user@gitlab.internal",
            "https://gitlab.internal/api/v4",
            "https://gitlab.internal?tenant=one",
        ] {
            assert!(
                TrustedImportOrigins::parse(&[invalid.to_owned()]).is_err(),
                "accepted non-origin config value: {invalid}"
            );
        }
    }

    #[test]
    fn credentialed_http_needs_its_own_exact_origin_opt_in() {
        let source = "http://gitlab.internal:8080/team/widgets.git";
        let token = Some("private-import-token");
        let error = ImportTransportPolicy::default()
            .require_confidential_credentials(source, token)
            .expect_err("the secure default must reject a plaintext credential");
        let typed = error
            .downcast_ref::<crate::error::InvalidRequest>()
            .expect("transport rejection remains a request error");
        assert!(typed.message.contains("allow_insecure_http_origins"));

        let allowed = plaintext_configured("http://gitlab.internal:8080");
        allowed
            .require_confidential_credentials(source, token)
            .expect("the exact operator-approved plaintext origin");
        for neighbor in [
            "http://gitlab.internal/team/widgets.git",
            "http://other.internal:8080/team/widgets.git",
        ] {
            assert!(allowed
                .require_confidential_credentials(neighbor, token)
                .is_err());
        }
    }

    #[test]
    fn anonymous_http_and_credentialed_https_keep_working_without_an_exception() {
        let policy = ImportTransportPolicy::default();
        policy
            .require_confidential_credentials("http://git.example/public.git", None)
            .expect("an anonymous public import carries no credential to expose");
        policy
            .require_confidential_credentials(
                "https://git.example/private.git",
                Some("private-import-token"),
            )
            .expect("HTTPS is the credentialed default");
    }

    #[test]
    fn native_git_is_refused_even_when_both_http_operator_exceptions_exist() {
        let source = "git://git.internal/team/widgets.git";
        let trusted = configured("http://git.internal");
        let transport = plaintext_configured("http://git.internal");

        // Reachability trust is exact on scheme and therefore cannot consume a
        // native-Git URL. The transport policy must still name the independent
        // confidentiality reason rather than relying on that implementation
        // detail of the SSRF exception.
        assert!(trusted.check_url_static(source).is_ok());
        for token in [None, Some("private-import-token")] {
            let error = transport
                .require_confidential_credentials(source, token)
                .expect_err("native Git has no confidential import mode");
            let typed = error
                .downcast_ref::<crate::error::InvalidRequest>()
                .expect("transport rejection remains a request error");
            assert!(typed.message.contains("git://"));
            assert!(typed.message.contains("HTTP-only"));
            assert!(typed.message.contains("trusted_origins"));
        }
    }

    #[test]
    fn url_embedded_password_is_a_credential_and_opt_ins_name_only_http_origins() {
        assert!(ImportTransportPolicy::default()
            .require_confidential_credentials(
                "http://oauth2:private-import-token@git.example/team/widgets.git",
                None,
            )
            .is_err());
        assert!(ImportTransportPolicy::parse(&["https://git.example".to_owned()]).is_err());
    }

    #[test]
    fn credential_origin_comparison_includes_scheme_host_and_port() {
        require_same_origin(
            "https://gitlab.internal/group/repo",
            "https://gitlab.internal/api/v4/projects/1",
        )
        .expect("paths may differ on one origin");

        for candidate in [
            "http://gitlab.internal/group/repo.git",
            "https://other.internal/group/repo.git",
            "https://gitlab.internal:8443/group/repo.git",
        ] {
            assert!(require_same_origin("https://gitlab.internal/group/repo", candidate).is_err());
        }
    }
}
