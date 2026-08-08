//! Telling "leave this alone" apart from "clear this" in a PATCH body.
//!
//! A partial update has three inputs per field, not two: the field is absent
//! (leave it), the field carries a value (set it), or the field is explicitly
//! `null` (clear it). `Option<Option<T>>` is the type that models all three —
//! and on its own it cannot carry the third, which is the trap this module
//! exists to close.
//!
//! With plain `#[serde(default)]`, an explicit `null` deserializes to the
//! **outer** `None`, the exact value an absent field produces. `Some(None)` is
//! unreachable, so a handler written as `if let Some(v) = field { set(v) }`
//! only ever sees `Some(Some(v))` and the clearing branch beneath it never
//! runs. It fails silently: `PATCH {"assignee_id": null}` answered `200` and
//! left the assignee in place (card_a156a521ca3b).
//!
//! [`double_option`] closes it. Serde only calls a field's `deserialize_with`
//! when the key is present, so absence still falls through to `default` — the
//! outer `None` — while a present key deserializes `Option<T>` (`None` for
//! `null`, `Some(v)` otherwise) and wraps it in `Some`. All three inputs become
//! distinguishable, and both halves are load-bearing: without `default` an
//! absent field is an error, without this function `null` is indistinguishable
//! from absence.
//!
//! Use it on every request field whose API contract says "null to clear":
//!
//! ```ignore
//! #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
//! pub assignee_id: Option<Option<i64>>,
//! ```

use serde::{Deserialize, Deserializer};

/// Deserialize a present field into `Some(_)` so an explicit `null` survives as
/// `Some(None)` rather than collapsing into the absent-field `None`.
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Patch {
        #[serde(default, deserialize_with = "super::double_option")]
        assignee_id: Option<Option<i64>>,
    }

    /// The three inputs must land on three different values. Asserting them
    /// together is the point: the bug this guards against is exactly two of
    /// them collapsing into one, which no single-case test can see.
    #[test]
    fn absent_null_and_a_value_stay_three_distinct_answers() {
        assert_eq!(
            serde_json::from_str::<Patch>("{}").unwrap(),
            Patch { assignee_id: None },
            "an absent field must mean 'leave it alone'"
        );
        assert_eq!(
            serde_json::from_str::<Patch>(r#"{"assignee_id": null}"#).unwrap(),
            Patch {
                assignee_id: Some(None)
            },
            "an explicit null must mean 'clear it', not 'leave it alone'"
        );
        assert_eq!(
            serde_json::from_str::<Patch>(r#"{"assignee_id": 7}"#).unwrap(),
            Patch {
                assignee_id: Some(Some(7))
            },
            "a value must mean 'set it'"
        );
    }
}
