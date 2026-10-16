//! The OpenAPI face of [`rg_core::list_state`]: a listing's `state` parameter
//! is documented with the values its parser accepts, read from the same
//! constants, so the spec and the `400` cannot drift apart.

use utoipa::openapi::schema::{ObjectBuilder, Type};

fn enumerated(accepted: &[&str]) -> utoipa::openapi::Object {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(accepted.iter().copied()))
        .build()
}

pub(crate) fn issue_states() -> utoipa::openapi::Object {
    enumerated(&rg_core::list_state::ISSUE_STATES)
}

pub(crate) fn milestone_states() -> utoipa::openapi::Object {
    enumerated(&rg_core::list_state::MILESTONE_STATES)
}

pub(crate) fn pull_request_states() -> utoipa::openapi::Object {
    enumerated(&rg_core::list_state::PULL_REQUEST_STATES)
}
