//! One HTTP answer, whole — the unit the id-scope sweeps compare.
//!
//! A status is one bit of a reply, and every sweep in this directory that pins
//! a masked refusal learned the same lesson the hard way: the oracle moves into
//! the body. `require_namespace_create` answered a single `403` to three
//! refusals with two different messages under it (`card_2179245d41db`), and
//! `404 artifact expired` against `404 artifact not found` put the same oracle
//! one level lower still. So "what did the caller learn" is the status *and* the
//! body, normalized of what is fresh on every response, and it is one type here
//! rather than a `shape()` helper re-spelled per test file — the three copies
//! that existed had already drifted apart in what they normalize.

use reqwest::{Response, StatusCode};

/// One answer, whole.
///
/// The body is kept *untruncated* on purpose. The first version of this returned
/// `body.chars().take(160)`, which is harmless while the body is only ever
/// quoted in a failure message and fatal the moment it is compared: a clipped
/// JSON body does not parse, the `request_id` normalization below never runs,
/// and two answers that say the same thing differ by a uuid forever. Clipping
/// happens in [`Answer::excerpt`], at the point of reporting, and nowhere else.
pub struct Answer {
    pub status: StatusCode,
    pub body: String,
}

impl Answer {
    /// Read a response to its end. The body is always drained, so a comparison
    /// cannot silently rest on an unread stream.
    pub async fn of(response: Response) -> Self {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Self { status, body }
    }

    /// Everything a caller learns, minus what is fresh on every response.
    ///
    /// `AppError::into_response` sets `request_id: None` and the tracing
    /// middleware stamps a uuid into the envelope further down the stack, so a
    /// raw body comparison is red between any two requests. The status is part
    /// of the shape: `401` and `404` may both be masked answers, but a caller
    /// who gets one for a foreign id and the other for an absent one has still
    /// learned which is which.
    pub fn shape(&self) -> (u16, String) {
        let normalized = match serde_json::from_str::<serde_json::Value>(&self.body) {
            Ok(mut parsed) => {
                if let Some(object) = parsed.as_object_mut() {
                    object.remove("request_id");
                    if let Some(error) = object.get_mut("error").and_then(|e| e.as_object_mut()) {
                        error.remove("request_id");
                    }
                }
                parsed.to_string()
            }
            // Not JSON — an empty `HEAD` body, or a handler that answers in
            // some other shape. Compared verbatim rather than waved through.
            Err(_) => self.body.clone(),
        };
        (self.status.as_u16(), normalized)
    }

    /// For failure messages only — never for comparison.
    pub fn excerpt(&self) -> String {
        self.body.chars().take(160).collect()
    }

    /// Whether the two shapes being equal actually asserted anything about a
    /// body. A `HEAD` route answers nothing by protocol, so its pair is a
    /// statement about the status alone and must not be counted towards a
    /// sweep's anti-vacuity floor.
    pub fn speaks(&self) -> bool {
        !self.body.trim().is_empty()
    }
}
