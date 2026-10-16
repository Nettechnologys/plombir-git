//! The `?state=` filter of a listing, parsed once for every surface that has one.
//!
//! The listings compared `state` for equality with whatever the query string
//! carried, so `?state=all` — what GitHub and Gitea clients send for "all of
//! them" — a typo like `?state=opne`, or `?state=merged` on issues answered
//! `200 []` with `total: 0`. A client cannot tell that from an empty repository
//! (card_8d29eeb02ee1). Every listing parses through [`parse`] now, so an
//! unknown value is a `400` that names the accepted ones, and `all` means "no
//! filter" everywhere.

use anyhow::Result;

/// The value that asks for every row, whatever its state.
pub const ALL: &str = "all";

/// The `?state=` values an issue listing accepts.
pub const ISSUE_STATES: [&str; 3] = ["open", "closed", ALL];

/// The `?state=` values a milestone listing accepts.
pub const MILESTONE_STATES: [&str; 3] = ["open", "closed", ALL];

/// The `?state=` values a pull request listing accepts.
///
/// `merging` is stored too, but only while a merge holds the row; it is not a
/// state anyone lists by.
pub const PULL_REQUEST_STATES: [&str; 4] = ["open", "closed", "merged", ALL];

/// Parse a listing's `?state=` against the values that listing accepts.
///
/// `Ok(None)` is "no filter" ([`ALL`]); `Ok(Some(state))` is the stored value
/// to compare with. Anything else is an [`crate::error::InvalidRequest`] — a
/// `400` — listing what the caller may send instead.
pub fn parse(requested: &str, accepted: &[&'static str]) -> Result<Option<&'static str>> {
    match accepted.iter().find(|state| **state == requested) {
        Some(&ALL) => Ok(None),
        Some(state) => Ok(Some(state)),
        None => Err(crate::error::invalid_request(format!(
            "invalid state: expected one of {}",
            accepted.join(", ")
        ))),
    }
}

/// [`parse`] for a listing whose `state` may be omitted: absent is `default`.
pub fn parse_or(
    requested: Option<&str>,
    default: &'static str,
    accepted: &[&'static str],
) -> Result<Option<&'static str>> {
    parse(requested.unwrap_or(default), accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_no_filter_and_a_known_state_is_itself() {
        assert_eq!(parse("all", &ISSUE_STATES).unwrap(), None);
        assert_eq!(parse("closed", &ISSUE_STATES).unwrap(), Some("closed"));
        assert_eq!(
            parse("merged", &PULL_REQUEST_STATES).unwrap(),
            Some("merged")
        );
        assert_eq!(
            parse_or(None, "open", &PULL_REQUEST_STATES).unwrap(),
            Some("open")
        );
        assert_eq!(parse_or(None, ALL, &MILESTONE_STATES).unwrap(), None);
    }

    #[test]
    fn an_unknown_state_is_a_request_error_naming_the_accepted_ones() {
        for (requested, accepted) in [
            ("opne", &ISSUE_STATES[..]),
            ("merged", &ISSUE_STATES[..]),
            ("merging", &PULL_REQUEST_STATES[..]),
            ("", &MILESTONE_STATES[..]),
            ("OPEN", &ISSUE_STATES[..]),
        ] {
            let error = parse(requested, accepted).expect_err(requested);
            assert!(
                error
                    .downcast_ref::<crate::error::InvalidRequest>()
                    .is_some(),
                "{requested:?} must be the caller's error, not a server one"
            );
            assert!(format!("{error}").contains(&accepted.join(", ")));
        }
    }
}
