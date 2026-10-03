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

use crate::{build_info, AppState};

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
    /// Whether this instance signs and verifies release-asset provenance.
    ///
    /// A capability, not a setting — it comes from `[releases]
    /// attestation_enabled`, not from [`rg_core::instance::InstanceSettings`],
    /// and it is here because both attestation endpoints answer `404` when the
    /// feature is off
    /// *and* when an asset simply has no attestation. Those are opposite facts
    /// for a reader — "this forge does not do provenance" versus "this file was
    /// never signed" — and without this flag the only way to tell them apart is
    /// the wording of an error body (card_5e52392a0274).
    ///
    /// Anonymous on purpose: it says what the software does, not what is in it.
    pub attestation_enabled: bool,
    /// Where to read the source code of the build answering this request:
    /// `<[server].source_url>/tree/<commit>`, or the repository itself when
    /// the build did not record its commit.
    ///
    /// The AGPL §13 offer. It is the operator's URL rather than a constant so
    /// a fork that modifies the code points its users at the fork by changing
    /// one setting, and it is anonymous because the people the offer is owed
    /// to are everyone the instance serves, logged in or not.
    pub source_url: String,
    /// The commit this binary was built from, or `null` when the build was not
    /// told (`PLOMBIR_GIT_SOURCE_COMMIT` unset at compile time).
    ///
    /// `null` rather than a guess: [`source_url`](Self::source_url) then names
    /// the repository without claiming a commit, and the UI can say the commit
    /// is unknown instead of linking a wrong one.
    pub source_commit: Option<String>,
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
    let source_commit = build_info::source_commit().known();
    (
        StatusCode::OK,
        Json(InstanceInfo {
            maintenance_mode: settings.maintenance_mode,
            banner_message: settings.banner_message,
            banner_type: settings.banner_type,
            attestation_enabled: state.attestation_enabled,
            source_url: build_info::source_link(&state.source_url, source_commit),
            source_commit: source_commit.map(str::to_string),
        }),
    )
        .into_response()
}
