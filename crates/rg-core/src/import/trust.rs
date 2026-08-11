//! Administrative trust boundary for private self-hosted import sources.
//!
//! Import URLs normally remain user-controlled and therefore pass the full
//! private-address SSRF guard. An operator may opt specific HTTP(S) origins
//! into this policy; the exception is exact on scheme, host, and effective
//! port, and never acts as a wildcard for other private-network destinations.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ImportOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl ImportOrigin {
    fn from_url(url: &reqwest::Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        Some(Self {
            scheme: url.scheme().to_owned(),
            host: url.host_str()?.to_owned(),
            port: url.port_or_known_default()?,
        })
    }

    fn from_target(raw: &str) -> Option<Self> {
        reqwest::Url::parse(raw)
            .ok()
            .as_ref()
            .and_then(Self::from_url)
    }

    fn from_config(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        let url = reqwest::Url::parse(trimmed)
            .with_context(|| format!("invalid trusted import origin '{trimmed}'"))?;
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("trusted import origin '{trimmed}' must use http or https");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("trusted import origin '{trimmed}' has no host"))?;
        if host.contains('*') {
            anyhow::bail!(
                "trusted import origin '{trimmed}' must name one exact host, not a wildcard"
            );
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("trusted import origin '{trimmed}' must not contain user information");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            anyhow::bail!(
                "trusted import origin '{trimmed}' must contain only scheme, host, and optional port"
            );
        }
        Self::from_url(&url).ok_or_else(|| {
            anyhow::anyhow!("trusted import origin '{trimmed}' has no effective port")
        })
    }
}

/// Exact origins an administrator has allowed imports to reach even when they
/// resolve to a private, loopback, or link-local address.
#[derive(Clone, Debug, Default)]
pub struct TrustedImportOrigins(Arc<HashSet<ImportOrigin>>);

impl TrustedImportOrigins {
    /// Parse the operator-facing `[imports].trusted_origins` values.
    pub fn parse(values: &[String]) -> Result<Self> {
        let mut origins = HashSet::with_capacity(values.len());
        for value in values {
            origins.insert(ImportOrigin::from_config(value)?);
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
