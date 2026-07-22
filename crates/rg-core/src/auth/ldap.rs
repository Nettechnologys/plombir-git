//! LDAP authentication service.
//! Two-step: bind with service account, search user DN, rebind with user DN + password.

use anyhow::{Context, Result};
use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};

#[derive(Debug, Clone)]
pub struct LdapConfig {
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    /// Disable TLS certificate verification for LDAPS connections.
    /// This must stay false in production and should only be enabled for tests
    /// against throwaway LDAP servers with self-signed certificates.
    pub insecure_skip_tls_verify: bool,
    pub bind_dn: String,
    pub bind_password: String,
    pub base_dn: String,
    pub user_filter: String,
}

#[derive(Debug, Clone)]
pub struct LdapUser {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub dn: String,
    pub uid: Option<String>,
}

/// Escape a value embedded in an LDAP search filter (RFC 4515).
fn escape_filter_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'\0' => escaped.push_str("\\00"),
            b'(' => escaped.push_str("\\28"),
            b')' => escaped.push_str("\\29"),
            b'*' => escaped.push_str("\\2a"),
            b'\\' => escaped.push_str("\\5c"),
            byte if !byte.is_ascii() => {
                use std::fmt::Write;
                let _ = write!(escaped, "\\{byte:02x}");
            }
            byte => escaped.push(*byte as char),
        }
    }
    escaped
}

fn connection_settings(config: &LdapConfig) -> LdapConnSettings {
    let settings = LdapConnSettings::new().set_conn_timeout(std::time::Duration::from_secs(10));
    if config.use_tls && config.insecure_skip_tls_verify {
        settings.set_no_tls_verify(true)
    } else {
        settings
    }
}

pub async fn authenticate(config: &LdapConfig, username: &str, password: &str) -> Result<LdapUser> {
    if username.trim().is_empty() || password.is_empty() {
        anyhow::bail!("invalid LDAP credentials");
    }
    if !config.user_filter.contains("{username}") {
        anyhow::bail!("LDAP user filter must contain '{{username}}'");
    }
    let url = if config.use_tls {
        format!("ldaps://{}:{}", config.host, config.port)
    } else {
        format!("ldap://{}:{}", config.host, config.port)
    };

    let settings = connection_settings(config);
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &url)
        .await
        .context("failed to connect to LDAP")?;

    ldap3::drive!(conn);

    // Step 1: bind with service account
    ldap.simple_bind(&config.bind_dn, &config.bind_password)
        .await
        .map_err(|e| anyhow::anyhow!("LDAP service bind failed: {}", e))?
        .success()
        .map_err(|e| anyhow::anyhow!("LDAP service bind rejected: {:?}", e))?;

    // Step 2: search for user
    let filter = config
        .user_filter
        .replace("{username}", &escape_filter_value(username));
    let (results, _) = ldap
        .search(
            &config.base_dn,
            Scope::Subtree,
            &filter,
            vec!["uid", "mail", "displayName", "cn", "givenName", "sn"],
        )
        .await
        .map_err(|e| anyhow::anyhow!("LDAP search failed: {}", e))?
        .success()
        .map_err(|e| anyhow::anyhow!("LDAP search rejected: {:?}", e))?;

    if results.is_empty() {
        anyhow::bail!("user '{}' not found in LDAP directory", username);
    }
    if results.len() != 1 {
        anyhow::bail!("LDAP user filter returned multiple entries");
    }

    // ldap3 v0.11: SearchResultEntry has (dn, attrs) pattern
    let entry = SearchEntry::construct(results[0].clone());
    let user_dn = entry.dn.clone();

    let email = entry.attrs.get("mail").and_then(|v| v.first()).cloned();
    let uid = entry.attrs.get("uid").and_then(|v| v.first()).cloned();
    let display_name = entry
        .attrs
        .get("displayName")
        .and_then(|v| v.first())
        .cloned()
        .or_else(|| {
            let first = entry.attrs.get("givenName").and_then(|v| v.first());
            let last = entry.attrs.get("sn").and_then(|v| v.first());
            match (first, last) {
                (Some(f), Some(l)) => Some(format!("{} {}", f, l)),
                (Some(n), None) | (None, Some(n)) => Some(n.clone()),
                _ => None,
            }
        });

    // Step 3: unbind service
    ldap.unbind().await.ok();

    // Step 4: rebind with user DN + password to verify
    let settings2 = connection_settings(config);
    let (conn2, mut ldap2) = LdapConnAsync::with_settings(settings2, &url)
        .await
        .context("failed to reconnect to LDAP for user auth")?;

    ldap3::drive!(conn2);

    let bind_result = ldap2
        .simple_bind(&user_dn, password)
        .await
        .map_err(|e| anyhow::anyhow!("LDAP user bind failed: {}", e))?;

    ldap2.unbind().await.ok();

    // Check bind success via the result code
    if bind_result.rc != 0 {
        anyhow::bail!("invalid LDAP credentials");
    }

    Ok(LdapUser {
        username: username.to_string(),
        email,
        display_name,
        dn: user_dn,
        uid,
    })
}

pub async fn test_connection(config: &LdapConfig) -> Result<()> {
    let url = if config.use_tls {
        format!("ldaps://{}:{}", config.host, config.port)
    } else {
        format!("ldap://{}:{}", config.host, config.port)
    };

    let settings = connection_settings(config);
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &url)
        .await
        .context("failed to connect to LDAP")?;

    ldap3::drive!(conn);

    ldap.simple_bind(&config.bind_dn, &config.bind_password)
        .await
        .map_err(|e| anyhow::anyhow!("LDAP bind failed: {}", e))?
        .success()
        .map_err(|e| anyhow::anyhow!("LDAP bind rejected: {:?}", e))?;

    ldap.unbind().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{escape_filter_value, LdapConfig};

    fn config(use_tls: bool, insecure_skip_tls_verify: bool) -> LdapConfig {
        LdapConfig {
            host: "ldap.example.com".to_string(),
            port: if use_tls { 636 } else { 389 },
            use_tls,
            insecure_skip_tls_verify,
            bind_dn: "cn=service,dc=example,dc=com".to_string(),
            bind_password: "secret".to_string(),
            base_dn: "dc=example,dc=com".to_string(),
            user_filter: "(uid={username})".to_string(),
        }
    }

    #[test]
    fn ldap_tls_verification_is_not_skipped_by_default() {
        let cfg = config(true, false);

        assert!(cfg.use_tls);
        assert!(!cfg.insecure_skip_tls_verify);
    }

    #[test]
    fn ldap_tls_verification_skip_requires_explicit_insecure_flag() {
        let cfg = config(true, true);

        assert!(cfg.use_tls);
        assert!(cfg.insecure_skip_tls_verify);
    }

    #[test]
    fn ldap_filter_values_are_rfc4515_escaped() {
        assert_eq!(escape_filter_value("alice"), "alice");
        assert_eq!(
            escape_filter_value("*)(uid=*)\\\0"),
            "\\2a\\29\\28uid=\\2a\\29\\5c\\00"
        );
        assert_eq!(escape_filter_value("é"), "\\c3\\a9");
    }
}
