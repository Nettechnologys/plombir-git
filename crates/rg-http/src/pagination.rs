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

/// Query parameters for pagination.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct PaginationParams {
    /// Page number (1-based). Default: 1
    #[serde(
        default = "default_page",
        deserialize_with = "deserialize_u64_from_query"
    )]
    pub page: u64,
    /// Items per page. Default: 20, Max: 100
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

pub fn default_page() -> u64 {
    1
}

pub fn default_per_page() -> u64 {
    DEFAULT_PER_PAGE
}

pub fn deserialize_u64_from_query<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim) {
        Some("") | None => Ok(default_page()),
        Some(raw) => raw.parse::<u64>().map_err(serde::de::Error::custom),
    }
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
    pub fn offset(&self) -> u64 {
        (self.page - 1) * self.per_page
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
        let total_pages = if total == 0 {
            1
        } else {
            total.div_ceil(params.per_page)
        };

        Self {
            page: params.page,
            per_page: params.per_page,
            total,
            total_pages,
            has_next: params.page < total_pages,
            has_prev: params.page > 1,
        }
    }
}

/// Paginated response wrapper.
///
/// CRITICAL: Serialization (踩坑经验 #2)
///
/// When returning from Axum handler, MUST wrap with serde_json::to_value():
///   (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
/// Without to_value(), the `data` field may be empty in the JSON response.
///
/// CRITICAL: Serialization (踩坑经验 #2)
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
}
