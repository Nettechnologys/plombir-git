//! How long the text a person writes may be.
//!
//! The ceiling is checked before the write, so a value that is too long is a
//! `400` naming the field on every backend. Without it the answer depended on
//! the database: PostgreSQL and SQLite stored any length, while MySQL's strict
//! mode refused a value longer than its column and the caller got a `500`
//! (card_9ea48ee6a422).
//!
//! The columns behind these ceilings hold more than the ceiling allows: the
//! bodies are `MEDIUMTEXT` on MySQL (`m20261016_000001_free_text_columns_mysql`),
//! the one-line fields stay `VARCHAR(255)`, which counts characters.

use crate::error::invalid_request;

/// An issue, pull request, comment, review, release or wiki page body.
///
/// The same ceiling GitHub puts on an issue or comment body, so anything
/// written there fits here. 65 536 four-byte characters are 256 KiB, well
/// inside MySQL's `MEDIUMTEXT`.
pub const MAX_BODY_CHARS: usize = 65_536;

/// A title or a short one-line field: the `VARCHAR(255)` it is stored in.
pub const MAX_LINE_CHARS: usize = 255;

/// A path inside a repository, as a review comment anchors to it: Linux's
/// `PATH_MAX`. Stored as `TEXT`, which 4 096 four-byte characters fit.
pub const MAX_PATH_CHARS: usize = 4096;

/// Refuse `value` when it holds more than `max` characters, naming `field`.
pub fn check(field: &str, value: &str, max: usize) -> anyhow::Result<()> {
    // `chars().count()` walks the whole string; a value that is short in bytes
    // cannot be long in characters, so most values never pay for it.
    if value.len() > max && value.chars().count() > max {
        return Err(invalid_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(())
}

/// [`check`] for a field the caller may leave out.
pub fn check_optional(field: &str, value: Option<&str>, max: usize) -> anyhow::Result<()> {
    value.map_or(Ok(()), |value| check(field, value, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_counts_characters_not_bytes() {
        let at_ceiling = "ž".repeat(MAX_LINE_CHARS);
        assert!(at_ceiling.len() > MAX_LINE_CHARS);
        check("title", &at_ceiling, MAX_LINE_CHARS).expect("255 characters fit");

        let over = format!("{at_ceiling}x");
        let refused = check("title", &over, MAX_LINE_CHARS).expect_err("256 characters do not");
        assert!(refused
            .downcast_ref::<crate::error::InvalidRequest>()
            .is_some());
        assert_eq!(refused.to_string(), "title must be at most 255 characters");
    }

    #[test]
    fn a_body_at_the_ceiling_passes_and_one_more_is_refused() {
        let body = "x".repeat(MAX_BODY_CHARS);
        check("body", &body, MAX_BODY_CHARS).expect("the ceiling itself is allowed");
        let refused = check("body", &format!("{body}x"), MAX_BODY_CHARS).unwrap_err();
        assert!(refused.to_string().starts_with("body "));
        check_optional("body", None, MAX_BODY_CHARS).expect("an absent field is fine");
    }
}
