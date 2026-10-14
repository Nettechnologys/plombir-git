//! Transport rules shared by every client that carries a credential to a server.
//!
//! The CLI, the MCP server and the runner all send a PAT or runner token to a
//! Plombir Git server, and each one must draw the same boundary. Keeping the
//! predicate and the redirect policy here, rather than in each binary, is what
//! stops one copy from being tightened while another silently keeps the old
//! rule. The crate is the lowest one all three depend on, so none of them has to
//! pull in the server-side `rg-core`.

/// Whether `url` addresses this machine, so plaintext HTTP to it cannot leave
/// the host. Accepts `localhost` and loopback IP literals, including the
/// IPv4-mapped IPv6 form. It never resolves names: a DNS name that happens to
/// point at loopback is not trusted as loopback.
pub fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    literal
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => ip.is_loopback(),
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|mapped| mapped.is_loopback())
            }
        })
}

/// Keep credentials on the exact origin of each initiating request.
///
/// Reqwest's default sensitive-header stripping compares host and port but not
/// scheme, so it is not sufficient for an HTTPS-to-HTTP redirect on the same
/// socket. Any redirect that leaves the origin is refused outright; allowed
/// redirects are delegated to the default policy and keep its redirect limit.
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

/// How a credential may travel to a server URL that passed
/// [`credential_server_transport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialTransport {
    Https,
    /// Plaintext, but to this machine: it cannot leave the host.
    LoopbackHttp,
    /// Plaintext to a remote host, and no credential rides on it.
    HttpWithoutCredential,
    /// Plaintext to a remote host because the operator explicitly allowed it.
    /// The caller warns: the credential may be read on the wire.
    InsecureHttpAllowed,
}

/// Why a server URL is refused a credential. Each binary words the refusal in
/// its own flag or variable names; the reasons are the same for all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialServerRefusal {
    /// Not an absolute URL; the parser's own explanation.
    NotAUrl(String),
    NoHost,
    /// `user:password@` in the URL: a second credential, in the clear, in
    /// every log line that prints the URL.
    UserInfo,
    /// Plaintext HTTP to a remote host, with a credential, and no opt-in.
    PlaintextRemote,
    /// A scheme other than `http` or `https`.
    UnsupportedScheme(String),
}

/// The one rule for sending a PAT or runner token to `raw`: HTTPS, plaintext
/// HTTP to loopback, plaintext HTTP carrying no credential, or plaintext HTTP
/// the operator explicitly allowed — and never user information in the URL.
///
/// The CLI, the MCP server and the runner each held a copy of this parse, and
/// only their messages differed (card_1274e045904e).
pub fn credential_server_transport(
    raw: &str,
    carries_credential: bool,
    allow_insecure_http: bool,
) -> Result<CredentialTransport, CredentialServerRefusal> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| CredentialServerRefusal::NotAUrl(error.to_string()))?;
    if url.host_str().is_none() {
        return Err(CredentialServerRefusal::NoHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CredentialServerRefusal::UserInfo);
    }
    match url.scheme() {
        "https" => Ok(CredentialTransport::Https),
        "http" if is_loopback_host(&url) => Ok(CredentialTransport::LoopbackHttp),
        "http" if !carries_credential => Ok(CredentialTransport::HttpWithoutCredential),
        "http" if allow_insecure_http => Ok(CredentialTransport::InsecureHttpAllowed),
        "http" => Err(CredentialServerRefusal::PlaintextRemote),
        scheme => Err(CredentialServerRefusal::UnsupportedScheme(
            scheme.to_string(),
        )),
    }
}

#[cfg(test)]
mod transport_tests {
    use super::{credential_server_transport, CredentialServerRefusal, CredentialTransport};

    #[test]
    fn every_outcome_of_the_rule() {
        use CredentialServerRefusal as No;
        use CredentialTransport as Yes;
        for (raw, credential, allow, expected) in [
            ("https://forge.example/", true, false, Ok(Yes::Https)),
            ("http://127.0.0.1:8080/", true, false, Ok(Yes::LoopbackHttp)),
            ("http://localhost/", true, false, Ok(Yes::LoopbackHttp)),
            (
                "http://forge.example/",
                false,
                false,
                Ok(Yes::HttpWithoutCredential),
            ),
            (
                "http://forge.example/",
                true,
                true,
                Ok(Yes::InsecureHttpAllowed),
            ),
            (
                "http://forge.example/",
                true,
                false,
                Err(No::PlaintextRemote),
            ),
            (
                "https://me:pw@forge.example/",
                true,
                false,
                Err(No::UserInfo),
            ),
            ("https://me@forge.example/", false, true, Err(No::UserInfo)),
            (
                "ftp://forge.example/",
                true,
                true,
                Err(No::UnsupportedScheme("ftp".to_string())),
            ),
            ("unix:/run/forge.sock", true, true, Err(No::NoHost)),
        ] {
            assert_eq!(
                credential_server_transport(raw, credential, allow),
                expected,
                "{raw} credential={credential} allow_insecure_http={allow}"
            );
        }
        assert!(matches!(
            credential_server_transport("forge.example", true, false),
            Err(No::NotAUrl(_))
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::is_loopback_host;

    fn url(raw: &str) -> reqwest::Url {
        reqwest::Url::parse(raw).expect("test URL parses")
    }

    #[test]
    fn loopback_names_and_literals_are_loopback() {
        for raw in [
            "http://localhost:3000/",
            "http://LOCALHOST/",
            "http://127.0.0.1:8080/",
            "http://127.4.5.6/",
            "http://[::1]:8080/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert!(is_loopback_host(&url(raw)), "{raw} must count as loopback");
        }
    }

    #[test]
    fn remote_names_and_addresses_are_not_loopback() {
        for raw in [
            "http://example.com/",
            "http://127.0.0.1.example.com/",
            "http://10.0.0.5/",
            "http://[2001:db8::1]/",
            "http://[::ffff:10.0.0.5]/",
        ] {
            assert!(
                !is_loopback_host(&url(raw)),
                "{raw} must not count as loopback"
            );
        }
    }
}
