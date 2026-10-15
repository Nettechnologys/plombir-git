//! Mirror service — sync and manage repository mirrors from remote sources.
/// LFS objects a mirror pass fetches after the refs.
pub(crate) mod lfs;
/// Push mirrors: the repository sends its branches and tags to another forge.
pub mod push;
/// The loop that sends due push mirrors out.
pub mod push_scheduler;
/// The background scheduler that runs the periodic half of the feature.
pub mod scheduler;
/// Mirror service for repository mirroring.
pub mod service;
/// Instance-level confidentiality policy for outbound mirror transport.
pub mod transport;
/// The refusal that keeps a pull mirror's branches and tags its upstream's.
pub mod write_guard;

/// Whether a credential given for `stored` may be sent to `requested`.
///
/// A mirror's credential was handed over for one remote: scheme, host and
/// effective port. A URL that keeps that origin and changes only the path is
/// the same server and may keep it; anything else has to bring its own. A URL
/// that does not parse into an origin (or a scheme with no known default port)
/// counts as a different one — the answer this check must never give by
/// accident is "same".
pub(crate) fn same_origin(stored: &str, requested: &str) -> bool {
    match (origin_of(stored), origin_of(requested)) {
        (Some(stored), Some(requested)) => stored == requested,
        _ => false,
    }
}

/// Scheme, host and effective port: what a credential is given for.
fn origin_of(url: &str) -> Option<(String, String, u16)> {
    let url = reqwest::Url::parse(url).ok()?;
    Some((
        url.scheme().to_string(),
        url.host_str()?.to_ascii_lowercase(),
        url.port_or_known_default()?,
    ))
}

#[cfg(test)]
mod origin_tests {
    use super::same_origin;

    #[test]
    fn a_new_path_on_the_same_server_keeps_the_origin() {
        assert!(same_origin(
            "https://github.com/org/private.git",
            "https://GitHub.com:443/org/renamed.git"
        ));
    }

    #[test]
    fn another_host_port_or_scheme_is_another_origin() {
        let stored = "https://github.com/org/private.git";
        assert!(!same_origin(stored, "https://collector.example/x.git"));
        assert!(!same_origin(
            stored,
            "https://github.com:8443/org/private.git"
        ));
        assert!(!same_origin(stored, "http://github.com/org/private.git"));
    }

    #[test]
    fn a_url_without_an_origin_is_never_the_same_one() {
        assert!(!same_origin(
            "git://a.example/x.git",
            "git://b.example/x.git"
        ));
        assert!(!same_origin("not a url", "not a url"));
    }
}
