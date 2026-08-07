//! Unified API pagination support.
//!
//! All list endpoints accept `page` and `per_page` query parameters.
//! Response wraps data in `PaginatedResponse { data, pagination }`.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// Default number of items per page.
const DEFAULT_PER_PAGE: u64 = 20;
/// Maximum number of items per page.
const MAX_PER_PAGE: u64 = 100;

/// The largest row offset a query may be given.
///
/// `LIMIT`/`OFFSET` is a signed 64-bit value in SQLite, PostgreSQL and MySQL
/// alike, so anything past `i64::MAX` is not a far page — it is a number the
/// driver cannot carry. Beyond the real row count every offset means the same
/// thing (an empty page), which is why saturating here loses nothing: the only
/// requests it changes are the ones that had no valid answer to begin with.
pub const MAX_OFFSET: u64 = i64::MAX as u64;

/// Clamp a 0-based page index so `page_index * per_page` stays representable.
///
/// For the handlers that build their own paginator rather than going through
/// [`PaginationParams`]. `sea_orm`'s `Paginator::fetch_page` multiplies the page
/// index by the page size with no check of its own, so a hand-rolled paginator
/// handed an unbounded index inherits exactly the overflow
/// [`PaginationParams::offset`] guards against — a panic with no response under
/// `overflow-checks`, and a wrapped offset serving somebody else's page without
/// them.
pub fn clamp_page_index(page_index: u64, per_page: u64) -> u64 {
    page_index.min(MAX_OFFSET / per_page.max(1))
}

/// Query parameters for pagination.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PaginationParams {
    /// Page number (1-based). Default: 1
    #[serde(
        default = "default_page",
        deserialize_with = "deserialize_page_from_query"
    )]
    pub page: u64,
    /// Items per page. Default: 20, Max: 100
    #[serde(
        default = "default_per_page",
        deserialize_with = "deserialize_per_page_from_query"
    )]
    pub per_page: u64,
}

pub fn default_page() -> u64 {
    1
}

pub fn default_per_page() -> u64 {
    DEFAULT_PER_PAGE
}

/// Parse a `u64` query parameter that reaches us as a string.
///
/// Every numeric field of this struct needs this, not just one. Most call sites
/// pull `PaginationParams` in through `#[serde(flatten)]`, and under `flatten`
/// serde reads the inner struct via `deserialize_any` — from which
/// `serde_urlencoded` only ever hands out strings. A bare `u64` field is
/// therefore unreachable behind a flatten: every value, valid or not, answers
/// `400 invalid type: string "2", expected u64`. Parsing the string ourselves
/// keeps the field reachable and still rejects junk with the `400` it deserves.
fn u64_from_query<'de, D>(deserializer: D, when_empty: u64) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim) {
        Some("") | None => Ok(when_empty),
        Some(raw) => raw.parse::<u64>().map_err(serde::de::Error::custom),
    }
}

pub fn deserialize_page_from_query<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    u64_from_query(deserializer, default_page())
}

pub fn deserialize_per_page_from_query<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    u64_from_query(deserializer, default_per_page())
}

impl PaginationParams {
    /// Create a new pagination params with defaults.
    pub fn new(page: u64, per_page: u64) -> Self {
        Self {
            page: page.max(1),
            per_page: per_page.clamp(1, MAX_PER_PAGE),
        }
    }

    /// Get the offset (0-based) for database queries.
    ///
    /// Saturating rather than wrapping, and capped at [`MAX_OFFSET`]. `page` is
    /// a `u64` straight off the query string with no ceiling of its own — the
    /// plain `(page - 1) * per_page` panicked mid-handler under
    /// `overflow-checks` (the client got a dropped connection, not a status) and
    /// wrapped in release, where `?page=4611686018427387905&per_page=100`
    /// computed an offset of 0 and answered `200` with the *first* page's rows
    /// under a `pagination.page` that said otherwise (card_8973efdcbea5).
    ///
    /// Saturating is deliberate over rejecting: a page past the end is an empty
    /// page, which is the same answer `?page=9999` already gets, and no request
    /// with a representable offset behaves differently.
    ///
    /// Note this must hold for a `PaginationParams` built by hand or by serde,
    /// not only for one that went through [`clamp`](Self::clamp) — the fields
    /// are public and deserialization fills them straight from the query.
    pub fn offset(&self) -> u64 {
        self.page
            .saturating_sub(1)
            .saturating_mul(self.per_page)
            .min(MAX_OFFSET)
    }

    /// Get the limit for database queries.
    pub fn limit(&self) -> u64 {
        self.per_page.min(MAX_PER_PAGE)
    }

    /// Clamp per_page to valid range.
    pub fn clamp(&self) -> Self {
        Self::new(self.page, self.per_page)
    }
}

/// Pagination metadata in response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaginationMeta {
    /// Current page number (1-based).
    pub page: u64,
    /// Items per page.
    pub per_page: u64,
    /// Total number of items.
    pub total: u64,
    /// Total number of pages.
    pub total_pages: u64,
    /// Whether there is a next page.
    pub has_next: bool,
    /// Whether there is a previous page.
    pub has_prev: bool,
}

impl PaginationMeta {
    /// Create pagination metadata from params and total count.
    pub fn from_params(params: &PaginationParams, total: u64) -> Self {
        // `per_page` is only guaranteed non-zero once `clamp` has run, and these
        // fields are public — a `?per_page=0` that reaches an unclamped params
        // struct would divide by zero here, which panics in every profile and
        // costs the client its response rather than its page.
        let per_page = params.per_page.max(1);
        let total_pages = if total == 0 {
            1
        } else {
            total.div_ceil(per_page)
        };

        Self {
            page: params.page,
            // The effective page size, not the requested one: reporting a
            // `per_page` the `total_pages` beside it was not computed from is
            // the kind of self-contradicting body this phase exists to stop.
            per_page,
            total,
            total_pages,
            has_next: params.page < total_pages,
            has_prev: params.page > 1,
        }
    }
}

/// Paginated response wrapper.
///
/// CRITICAL: Serialization (pitfall #2)
///
/// When returning from Axum handler, MUST wrap with serde_json::to_value():
///   (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
/// Without to_value(), the `data` field may be empty in the JSON response.
///
/// CRITICAL: Serialization (pitfall #2)
///
/// When returning `PaginatedResponse<T>` from an Axum handler,
/// you MUST wrap it with `serde_json::to_value()` before returning:
///
///   OK pattern:
///     let resp = PaginatedResponse::new(data, &params, total);
///     (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
///
///   WRONG pattern (will compile but produce wrong JSON or empty data field):
///     (StatusCode::OK, Json(resp)).into_response()
///     // or
///     Json(resp).into_response()
///
/// Reason: `PaginatedResponse<T>` implements `Serialize`, but Axum's
/// `Json()` extractor may not correctly serialize generic wrappers
/// without explicit `to_value()` conversion. Always use `to_value()`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub pagination: PaginationMeta,
}

impl<T: Serialize> PaginatedResponse<T> {
    /// Create a new paginated response.
    pub fn new(data: Vec<T>, params: &PaginationParams, total: u64) -> Self {
        Self {
            data,
            pagination: PaginationMeta::from_params(params, total),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PaginationParams tests ──────────────────────────────────────────

    #[test]
    fn test_default_page() {
        let params = PaginationParams::new(1, 20);
        assert_eq!(params.page, 1);
        assert_eq!(params.per_page, 20);
    }

    #[test]
    fn test_page_clamped_to_min_1() {
        let params = PaginationParams::new(0, 20);
        assert_eq!(params.page, 1);
    }

    #[test]
    fn test_per_page_clamped_to_range() {
        let params = PaginationParams::new(1, 200);
        assert_eq!(params.per_page, 100); // MAX_PER_PAGE

        let params = PaginationParams::new(1, 0);
        assert_eq!(params.per_page, 1); // min 1
    }

    #[test]
    fn test_offset_calculation() {
        let params = PaginationParams::new(1, 20);
        assert_eq!(params.offset(), 0);

        let params = PaginationParams::new(3, 10);
        assert_eq!(params.offset(), 20);
    }

    // ── Absurd page numbers (card_8973efdcbea5) ─────────────────────────
    //
    // `page` arrives as a bare `u64` with no ceiling, and `offset()` used to
    // multiply it out raw. Under `overflow-checks` that panicked inside the
    // handler, so the client got a dropped connection instead of a status; with
    // checks off it wrapped, and a page number near `2^62` computed an offset of
    // 0 and served the *first* page's rows under a `pagination.page` that
    // claimed otherwise. The pairs below are the two ends of that.

    #[test]
    fn a_page_number_that_would_overflow_the_offset_saturates_instead() {
        // The release-build symptom: this product wraps to exactly 0, so before
        // the fix these rows were page one's.
        let params = PaginationParams::new((1u64 << 62) + 1, 100);
        assert_eq!(
            params.offset(),
            MAX_OFFSET,
            "a page past the end must ask for a far offset, never for page one"
        );

        // The debug-build symptom: no product this large is representable at
        // all, so the multiplication itself used to abort the handler.
        let params = PaginationParams::new(u64::MAX, MAX_PER_PAGE);
        assert_eq!(params.offset(), MAX_OFFSET);
    }

    #[test]
    fn a_saturated_offset_still_fits_what_a_database_can_be_asked_for() {
        // Every backend takes `OFFSET` as a signed 64-bit value; an offset the
        // driver cannot carry is not a page, it is an error on the way out.
        assert!(i64::try_from(MAX_OFFSET).is_ok());
        assert_eq!(MAX_OFFSET, i64::MAX as u64);
    }

    #[test]
    fn an_offset_is_safe_on_params_that_never_went_through_clamp() {
        // Serde fills these fields directly, and they are public besides, so
        // `offset()` cannot rely on `clamp()` having run first.
        let raw = PaginationParams {
            page: u64::MAX,
            per_page: u64::MAX,
        };
        assert_eq!(raw.offset(), MAX_OFFSET);

        let zero_page = PaginationParams {
            page: 0,
            per_page: 20,
        };
        assert_eq!(
            zero_page.offset(),
            0,
            "page 0 must not underflow into a huge offset"
        );
    }

    #[test]
    fn ordinary_page_numbers_are_untouched_by_the_ceiling() {
        assert_eq!(PaginationParams::new(1, 20).offset(), 0);
        assert_eq!(PaginationParams::new(3, 20).offset(), 40);
        assert_eq!(PaginationParams::new(1000, 100).offset(), 99_900);
    }

    #[test]
    fn a_hand_rolled_page_index_gets_the_same_ceiling() {
        // What the audit-log handlers hand to sea_orm's paginator, which
        // multiplies index by page size with no check of its own.
        assert_eq!(clamp_page_index(4, 20), 4);
        assert_eq!(clamp_page_index(u64::MAX, 100) * 100, MAX_OFFSET - 7);
        assert!(clamp_page_index(u64::MAX, 100).checked_mul(100).is_some());
        // A zero page size must not divide by zero on the way to the ceiling.
        assert_eq!(clamp_page_index(u64::MAX, 0), MAX_OFFSET);
    }

    #[test]
    fn a_zero_page_size_does_not_divide_by_zero_in_the_metadata() {
        // `per_page` is only non-zero after `clamp()`, and `from_params` is
        // public. Dividing by it unguarded panics in release too.
        let raw = PaginationParams {
            page: 1,
            per_page: 0,
        };
        let meta = PaginationMeta::from_params(&raw, 55);
        assert_eq!(meta.total_pages, 55);
        assert_eq!(
            meta.per_page, 1,
            "the reported page size must be the one total_pages was computed from"
        );
    }

    #[test]
    fn test_limit_calculation() {
        let params = PaginationParams::new(1, 50);
        assert_eq!(params.limit(), 50);

        let params = PaginationParams::new(1, 200);
        assert_eq!(params.limit(), 100); // clamped to MAX_PER_PAGE
    }

    #[test]
    fn test_clamp_method() {
        let params = PaginationParams {
            page: 0,
            per_page: 500,
        };
        let clamped = params.clamp();
        assert_eq!(clamped.page, 1);
        assert_eq!(clamped.per_page, 100);
    }

    // ── PaginationMeta tests ────────────────────────────────────────────

    #[test]
    fn test_meta_first_page() {
        let params = PaginationParams::new(1, 20);
        let meta = PaginationMeta::from_params(&params, 55);
        assert_eq!(meta.page, 1);
        assert_eq!(meta.per_page, 20);
        assert_eq!(meta.total, 55);
        assert_eq!(meta.total_pages, 3); // ceil(55/20) = 3
        assert!(meta.has_next);
        assert!(!meta.has_prev);
    }

    #[test]
    fn test_meta_last_page() {
        let params = PaginationParams::new(3, 20);
        let meta = PaginationMeta::from_params(&params, 55);
        assert_eq!(meta.page, 3);
        assert!(!meta.has_next);
        assert!(meta.has_prev);
    }

    #[test]
    fn test_meta_middle_page() {
        let params = PaginationParams::new(2, 20);
        let meta = PaginationMeta::from_params(&params, 55);
        assert!(meta.has_next);
        assert!(meta.has_prev);
    }

    #[test]
    fn test_meta_zero_total() {
        let params = PaginationParams::new(1, 20);
        let meta = PaginationMeta::from_params(&params, 0);
        assert_eq!(meta.total_pages, 1);
        assert!(!meta.has_next);
        assert!(!meta.has_prev);
    }

    #[test]
    fn test_meta_exact_division() {
        let params = PaginationParams::new(1, 10);
        let meta = PaginationMeta::from_params(&params, 20);
        assert_eq!(meta.total_pages, 2);
    }

    // ── PaginatedResponse tests ─────────────────────────────────────────

    #[test]
    fn test_paginated_response_creation() {
        let params = PaginationParams::new(1, 10);
        let resp = PaginatedResponse::new(vec!["a", "b"], &params, 50);
        assert_eq!(resp.data, vec!["a", "b"]);
        assert_eq!(resp.pagination.page, 1);
        assert_eq!(resp.pagination.total, 50);
        assert_eq!(resp.pagination.total_pages, 5);
    }

    #[test]
    fn test_paginated_response_empty() {
        let params = PaginationParams::new(1, 10);
        let resp: PaginatedResponse<String> = PaginatedResponse::new(vec![], &params, 0);
        assert!(resp.data.is_empty());
        assert_eq!(resp.pagination.total, 0);
        assert_eq!(resp.pagination.total_pages, 1);
    }

    #[test]
    fn test_default_per_page_fn() {
        assert_eq!(default_page(), 1);
        assert_eq!(default_per_page(), 20);
    }

    // ── Query-string parsing under `#[serde(flatten)]` ──────────────────
    //
    // Every list handler but a handful pulls these params in flattened into a
    // larger query struct, and that is the shape where a bare `u64` field stops
    // being reachable. Asserting on `PaginationParams` alone would keep passing
    // with the bug in place, so these go through a flattened stand-in — the
    // same shape `issues::ListQuery` and its four siblings have.

    #[derive(Debug, Deserialize)]
    struct FlattenedQuery {
        state: Option<String>,
        #[serde(flatten)]
        pagination: PaginationParams,
    }

    fn parse(query: &str) -> Result<FlattenedQuery, serde_urlencoded::de::Error> {
        serde_urlencoded::from_str::<FlattenedQuery>(query)
    }

    #[test]
    fn a_flattened_per_page_is_read_from_the_query_string() {
        let parsed = parse("state=open&per_page=2").expect("per_page=2 is a valid page size");
        assert_eq!(parsed.pagination.per_page, 2);
        assert_eq!(parsed.pagination.page, 1);
        assert_eq!(parsed.state.as_deref(), Some("open"));
    }

    #[test]
    fn a_flattened_page_is_read_from_the_query_string() {
        let parsed = parse("page=3&per_page=5").expect("page and per_page parse together");
        assert_eq!(parsed.pagination.page, 3);
        assert_eq!(parsed.pagination.per_page, 5);
    }

    #[test]
    fn an_absent_pagination_falls_back_to_the_documented_defaults() {
        let parsed = parse("state=open").expect("pagination is optional");
        assert_eq!(parsed.pagination.page, default_page());
        assert_eq!(parsed.pagination.per_page, default_per_page());
    }

    #[test]
    fn an_empty_pagination_value_falls_back_to_the_documented_defaults() {
        let parsed = parse("page=&per_page=").expect("an empty value means 'unset'");
        assert_eq!(parsed.pagination.page, default_page());
        assert_eq!(parsed.pagination.per_page, default_per_page());
    }

    #[test]
    fn a_non_numeric_page_size_is_still_rejected() {
        // Parsing the string ourselves must not turn junk into a default: a
        // caller who asked for `per_page=abc` gets told, not silently served 20.
        assert!(parse("per_page=abc").is_err());
        assert!(parse("page=abc").is_err());
    }

    #[test]
    fn an_oversized_page_size_is_clamped_rather_than_rejected() {
        let parsed = parse("per_page=1000").expect("an oversized page size is not a bad request");
        let clamped = parsed.pagination.clamp();
        assert_eq!(clamped.per_page, MAX_PER_PAGE);
        // Zero is the other end of the same range: it must not reach
        // `PaginationMeta::from_params`, whose `div_ceil(per_page)` divides by it.
        let zero = parse("per_page=0").expect("zero is a number, not junk");
        assert_eq!(zero.pagination.clamp().per_page, 1);
    }
}
