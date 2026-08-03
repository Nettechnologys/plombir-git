//! LDAP authentication service.
//! Two-step: bind with service account, search user DN, rebind with user DN + password.

use anyhow::Result;
use ldap3::{LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry};

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
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in value.as_bytes() {
        match byte {
            b'\0' => escaped.push_str("\\00"),
            b'(' => escaped.push_str("\\28"),
            b')' => escaped.push_str("\\29"),
            b'*' => escaped.push_str("\\2a"),
            b'\\' => escaped.push_str("\\5c"),
            byte if !byte.is_ascii() => {
                escaped.push('\\');
                escaped.push(HEX[(byte >> 4) as usize] as char);
                escaped.push(HEX[(byte & 0x0f) as usize] as char);
            }
            byte => escaped.push(byte as char),
        }
    }
    escaped
}

/// LDAP result code for "these credentials are wrong" (RFC 4511 §4.1.9).
///
/// The only non-zero bind result that is a *verdict* about the password. AD
/// folds its "account locked", "password expired" and "must change password"
/// cases into it too, with the detail in the diagnostic message — all of them
/// are still the directory answering about this person's credential.
const LDAP_INVALID_CREDENTIALS: u32 = 49;

/// Tag a failed round-trip to the directory as *its* failure.
///
/// Everything this module does happens against a host the forge does not own,
/// and none of it can be fixed by the person signing in: a refused connection,
/// a TLS handshake that fails, a service account the directory no longer
/// accepts, a search the directory refuses to run. Flattened into a bare
/// `anyhow::Error` they were indistinguishable from "your password is wrong" —
/// and one caller up, [`crate::user::service`] turned every one of them into
/// `invalid credentials`, which the login door then answered with a `401` and
/// a strike on the brute-force counter.
///
/// Layered as *context* over the `LdapError`, so `{:#}` in the operator log
/// still carries the underlying cause — see [`crate::error::UpstreamUnavailable`].
trait DirectoryCall<T> {
    fn directory_call(self, what: &str) -> Result<T>;
}

impl<T> DirectoryCall<T> for std::result::Result<T, LdapError> {
    fn directory_call(self, what: &str) -> Result<T> {
        self.map_err(|error| {
            anyhow::Error::new(error).context(crate::error::UpstreamUnavailable::new(what))
        })
    }
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
        .directory_call("could not connect to the LDAP directory")?;

    ldap3::drive!(conn);

    // Step 1: bind with service account. Both halves are the forge's own
    // credential, not the caller's: a directory that refuses it is broken or
    // misconfigured, and saying "invalid credentials" to the person signing in
    // would blame them for it.
    ldap.simple_bind(&config.bind_dn, &config.bind_password)
        .await
        .directory_call("the LDAP service bind did not complete")?
        .success()
        .directory_call("the LDAP directory refused the service bind")?;

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
        .directory_call("the LDAP directory search did not complete")?
        .success()
        .directory_call("the LDAP directory refused the search")?;

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
    if let Err(error) = ldap.unbind().await {
        tracing::warn!(%error, "LDAP service unbind failed");
    }

    // Step 4: rebind with user DN + password to verify
    let settings2 = connection_settings(config);
    let (conn2, mut ldap2) = LdapConnAsync::with_settings(settings2, &url)
        .await
        .directory_call("could not reconnect to the LDAP directory to verify the password")?;

    ldap3::drive!(conn2);

    let bind_result = ldap2
        .simple_bind(&user_dn, password)
        .await
        .directory_call("the LDAP password bind did not complete")?;

    if let Err(error) = ldap2.unbind().await {
        tracing::warn!(%error, "LDAP user unbind failed");
    }

    // The result code is where the directory answers the only question this
    // module was asked. `49` is that answer — the password is wrong — and it is
    // the sole non-zero code that may reach the caller as a verdict. Anything
    // else (`busy`, `unavailable`, `unwillingToPerform`, a server-side error)
    // means the bind never got to judge the password, so it travels as an
    // outage: reporting it as a rejection would cost this person a strike on
    // the brute-force counter for the directory's bad day.
    if bind_result.rc == LDAP_INVALID_CREDENTIALS {
        anyhow::bail!("invalid LDAP credentials");
    }
    if bind_result.rc != 0 {
        return Err(anyhow::anyhow!(
            "LDAP bind returned result code {} ({})",
            bind_result.rc,
            bind_result.text
        )
        .context(crate::error::UpstreamUnavailable::new(
            "the LDAP directory could not complete the password bind",
        )));
    }

    Ok(LdapUser {
        username: username.to_string(),
        email,
        display_name,
        dn: user_dn,
        uid,
    })
}

/// Dial the directory and bind with the forge's own service account.
///
/// Every failure here belongs to the same host [`authenticate`] talks to, and
/// none of it is a defect of the request that asked for the check — so each leg
/// is tagged with [`DirectoryCall`] too. The admin's "test connection" button is
/// the one door where that distinction is the whole point: it exists to report
/// what the directory did, and a `400` there tells the admin to fix a request
/// that was already correct (card_a86f0776021c).
pub async fn test_connection(config: &LdapConfig) -> Result<()> {
    let url = if config.use_tls {
        format!("ldaps://{}:{}", config.host, config.port)
    } else {
        format!("ldap://{}:{}", config.host, config.port)
    };

    let settings = connection_settings(config);
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &url)
        .await
        .directory_call("could not connect to the LDAP directory")?;

    ldap3::drive!(conn);

    ldap.simple_bind(&config.bind_dn, &config.bind_password)
        .await
        .directory_call("the LDAP service bind did not complete")?
        .success()
        .directory_call("the LDAP directory refused the service bind")?;

    if let Err(error) = ldap.unbind().await {
        tracing::warn!(%error, "LDAP service unbind failed after connection test");
    }
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
