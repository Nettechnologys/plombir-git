//! What an unauthenticated browser is allowed to know about this instance.
//!
//! The operator-facing settings live behind `GET /api/v1/admin/settings`, and
//! for a while that was the *only* door to them. Two of the values on the other
//! side describe the instance to everyone using it — the banner an operator
//! writes to announce maintenance, and whether writes are currently closed — so
//! putting them behind `require_instance_admin` meant the banner reached exactly
//! one reader: the admin who had just typed it, in that one page session, until
//! they reloaded (card_801b8bcdb880).
//!
//! This is deliberately not "the settings endpoint, unauthenticated". It serves
//! the announcement and nothing else: a new operator knob added to
//! [`InstanceSettings`](rg_core::instance::InstanceSettings) has to be listed
//! here on purpose before an anonymous caller can see it.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;

/// The instance's own announcement, as anyone may read it.
#[derive(Serialize, ToSchema)]
pub struct InstanceInfo {
    /// Whether writes are currently refused with `503`.
    pub maintenance_mode: bool,
    /// The banner text, or `null` when no banner is set.
    ///
    /// `null` and "no banner" are one state on purpose: the admin PATCH already
    /// folds an empty string into `None`, and `is_banner_active` reads the
    /// `Option`. Serving `""` here would give the frontend a second spelling of
    /// "off" to get wrong.
    pub banner_message: Option<String>,
    /// `info` / `warning` / `error` — how the banner should be shown.
    pub banner_type: String,
}

/// GET /api/v1/instance — the public announcement of this instance.
#[utoipa::path(
    get,
    path = "/instance",
    tag = "Instance",
    responses(
        (status = 200, description = "Instance banner and maintenance state", body = InstanceInfo),
    ),
)]
pub async fn get_instance(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.instance_settings.get(&state.db).await;
    (
        StatusCode::OK,
        Json(InstanceInfo {
            maintenance_mode: settings.maintenance_mode,
            banner_message: settings.banner_message,
            banner_type: settings.banner_type,
        }),
    )
        .into_response()
}
