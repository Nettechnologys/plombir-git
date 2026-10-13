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
