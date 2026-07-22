//! Project Board REST API.
//!
//! Boards:
//!   POST   /repos/:owner/:name/boards               — create board
//!   GET    /repos/:owner/:name/boards               — list boards
//!   GET    /repos/:owner/:name/boards/:id           — get board with columns/cards
//!   PATCH  /repos/:owner/:name/boards/:id           — update board
//!   DELETE /repos/:owner/:name/boards/:id           — delete board
//!
//! Columns:
//!   POST   /repos/:owner/:name/boards/:id/columns   — create column
//!   PATCH  /repos/:owner/:name/boards/:id/columns/:col_id — update column
//!   DELETE /repos/:owner/:name/boards/:id/columns/:col_id — delete column
//!
//! Cards:
//!   POST   /repos/:owner/:name/boards/:id/columns/:col_id/cards — create card
//!   PATCH  /repos/:owner/:name/boards/:id/cards/:card_id       — update card
//!   POST   /repos/:owner/:name/boards/:id/cards/:card_id/move  — move card
//!   POST   /repos/:owner/:name/boards/:id/cards/reorder        — reorder cards
//!   DELETE /repos/:owner/:name/boards/:id/cards/:card_id       — delete card

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::AppState;

// ── Request types ────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct CreateBoardRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateBoardRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateColumnRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateColumnRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateCardRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateCardRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<Option<i64>>,
}

#[derive(Deserialize, ToSchema)]
pub struct MoveCardRequest {
    pub column_id: i64,
    pub position: i32,
}

#[derive(Deserialize, ToSchema)]
pub struct ReorderCardsRequest {
    /// List of (card_id, new_position) pairs.
    pub positions: Vec<(i64, i32)>,
}

// ── Board handlers ───────────────────────────────────────────────────────

/// POST /api/v1/repos/{owner}/{name}/boards
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/boards",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body = CreateBoardRequest,
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<CreateBoardRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::board::service::create_board(
        &state.db,
        body.name,
        body.description,
        Some(repo.id),
        None,
        user_id,
    )
    .await
    {
        Ok(board) => (StatusCode::CREATED, Json(serde_json::json!(board))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// GET /api/v1/repos/{owner}/{name}/boards
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/boards",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
    ),
)]
pub async fn list_boards(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::board::service::list_boards_by_repo(&state.db, repo.id).await {
        Ok(boards) => (StatusCode::OK, Json(serde_json::json!(boards))).into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// GET /api/v1/repos/{owner}/{name}/boards/{id}
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/boards/{id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_board(
    State(state): State<AppState>,
    Path((_owner, _name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    match rg_core::board::service::get_board(&state.db, id).await {
        Ok(Some(board)) => (StatusCode::OK, Json(serde_json::json!(board))).into_response(),
        Ok(None) => AppError::not_found("board not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// PATCH /api/v1/repos/{owner}/{name}/boards/{id}
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/boards/{id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
    ),
    request_body = UpdateBoardRequest,
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
    Json(body): Json<UpdateBoardRequest>,
) -> impl IntoResponse {
    let _claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };

    let _ = (owner, name); // validated by repo existence in the board

    match rg_core::board::service::update_board(&state.db, id, body.name, body.description).await {
        Ok(board) => (StatusCode::OK, Json(serde_json::json!(board))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// DELETE /api/v1/repos/{owner}/{name}/boards/{id}
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/boards/{id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_owner, _name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let _ = claims;
    match rg_core::board::service::delete_board(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

// ── Column handlers ──────────────────────────────────────────────────────

/// POST /api/v1/repos/{owner}/{name}/boards/{id}/columns
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/boards/{id}/columns",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
    ),
    request_body = CreateColumnRequest,
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_column(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_owner, _name, board_id)): Path<(String, String, i64)>,
    Json(body): Json<CreateColumnRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let _ = claims;
    match rg_core::board::service::create_column(&state.db, board_id, body.name, body.color).await {
        Ok(column) => (StatusCode::CREATED, Json(serde_json::json!(column))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// PATCH /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("col_id" = i64, Path, description = "column id"),
    ),
    request_body = UpdateColumnRequest,
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
    ),
)]
pub async fn update_column(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id, col_id)): Path<(String, String, i64, i64)>,
    Json(body): Json<UpdateColumnRequest>,
) -> impl IntoResponse {
    match rg_core::board::service::update_column(&state.db, col_id, body.name, body.color).await {
        Ok(column) => (StatusCode::OK, Json(serde_json::json!(column))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("col_id" = i64, Path, description = "column id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
    ),
)]
pub async fn delete_column(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id, col_id)): Path<(String, String, i64, i64)>,
) -> impl IntoResponse {
    match rg_core::board::service::delete_column(&state.db, col_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

// ── Card handlers ────────────────────────────────────────────────────────

/// POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("col_id" = i64, Path, description = "column id"),
    ),
    request_body = CreateCardRequest,
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
    ),
)]
pub async fn create_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_owner, _name, _board_id, col_id)): Path<(String, String, i64, i64)>,
    Json(body): Json<CreateCardRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let _ = claims;
    match rg_core::board::service::create_card(&state.db, col_id, body.issue_id, body.note).await {
        Ok(card) => (StatusCode::CREATED, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("card_id" = i64, Path, description = "card id"),
    ),
    request_body = UpdateCardRequest,
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
    ),
)]
pub async fn update_card(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id, card_id)): Path<(String, String, i64, i64)>,
    Json(body): Json<UpdateCardRequest>,
) -> impl IntoResponse {
    match rg_core::board::service::update_card(&state.db, card_id, body.note, body.issue_id).await {
        Ok(card) => (StatusCode::OK, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("card_id" = i64, Path, description = "card id"),
    ),
    request_body = MoveCardRequest,
    responses(
        (status = 200, description = "Moved", body = serde_json::Value),
    ),
)]
pub async fn move_card(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id, card_id)): Path<(String, String, i64, i64)>,
    Json(body): Json<MoveCardRequest>,
) -> impl IntoResponse {
    match rg_core::board::service::move_card(&state.db, card_id, body.column_id, body.position)
        .await
    {
        Ok(card) => (StatusCode::OK, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/boards/{id}/cards/reorder",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
    ),
    request_body = ReorderCardsRequest,
    responses(
        (status = 200, description = "Reordered", body = serde_json::Value),
    ),
)]
pub async fn reorder_cards(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id)): Path<(String, String, i64)>,
    Json(body): Json<ReorderCardsRequest>,
) -> impl IntoResponse {
    match rg_core::board::service::reorder_cards(&state.db, body.positions).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
    tag = "Boards",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "board id"),
        ("card_id" = i64, Path, description = "card id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
    ),
)]
pub async fn delete_card(
    State(state): State<AppState>,
    Path((_owner, _name, _board_id, card_id)): Path<(String, String, i64, i64)>,
) -> impl IntoResponse {
    match rg_core::board::service::delete_card(&state.db, card_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}
