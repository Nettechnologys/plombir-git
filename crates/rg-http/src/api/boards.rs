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
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::repo_access::{RepoRead, RepoWrite};
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

// ── Repository-scoped lookups ────────────────────────────────────────────
//
// Every id below `/repos/{owner}/{name}/boards` is a global primary key, so a
// permission check on `owner/name` only guards the object it was asked about
// once that object has been read back and matched against the repository. The
// handlers used to skip both halves: none of them resolved the repository, and
// the ones that did authenticate stopped there — a valid token for any account
// was full control over the boards of every repository on the instance.
//
// A mismatch answers 404, not 403: a 403 would confirm the id exists.

async fn board_in_repo(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    board_id: i64,
) -> Result<rg_db::entities::board::Model, AppError> {
    match rg_db::ops::board_ops::find_board_by_id(&state.db, board_id).await {
        // `repo_id` is nullable because a board may instead belong to an
        // organization; such a board is not reachable through this route.
        Ok(Some(board)) if board.repo_id == Some(repo.id) => Ok(board),
        Ok(_) => Err(AppError::not_found("board not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

async fn column_in_board(
    state: &AppState,
    board_id: i64,
    column_id: i64,
) -> Result<rg_db::entities::board_column::Model, AppError> {
    match rg_db::ops::board_ops::find_column_by_id(&state.db, column_id).await {
        Ok(Some(column)) if column.board_id == board_id => Ok(column),
        Ok(_) => Err(AppError::not_found("board column not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

async fn card_in_board(
    state: &AppState,
    board_id: i64,
    card_id: i64,
) -> Result<rg_db::entities::board_card::Model, AppError> {
    let card = match rg_db::ops::board_ops::find_card_by_id(&state.db, card_id).await {
        Ok(Some(card)) => card,
        Ok(None) => return Err(AppError::not_found("board card not found")),
        Err(e) => return Err(AppError::from(e)),
    };

    match column_in_board(state, board_id, card.column_id).await {
        Ok(_) => Ok(card),
        // The card exists but hangs off another board — same answer as "no such
        // card", so the endpoint cannot be used to probe for card ids.
        Err(AppError::NotFound(_)) => Err(AppError::not_found("board card not found")),
        Err(e) => Err(e),
    }
}

/// Confirm a caller-supplied issue link points into the repository that owns
/// the board. `get_board` embeds the full issue in its response, so an
/// unchecked link turns a board the caller owns into a reader for the issues
/// of every private repository on the instance.
async fn issue_in_repo(state: &AppState, repo_id: i64, issue_id: i64) -> Result<(), AppError> {
    match rg_db::ops::issue_ops::find_by_id(&state.db, issue_id).await {
        Ok(Some(issue)) if issue.repo_id == repo_id => Ok(()),
        Ok(_) => Err(AppError::not_found("issue not found")),
        Err(e) => Err(AppError::from(e)),
    }
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
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn create_board(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoWrite {
        repo,
        actor_id: user_id,
    }: RepoWrite,
    Json(body): Json<CreateBoardRequest>,
) -> impl IntoResponse {
    // Authenticating was the whole check here: any account could add a board to
    // any repository, private ones included.

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
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn list_boards(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    // The handler did not even take `HeaderMap`, so the board list of a private
    // repository was readable by anyone who guessed the owner/name pair.

    match rg_core::board::service::list_boards_by_repo(&state.db, repo.id).await {
        Ok(boards) => (StatusCode::OK, Json(serde_json::json!(boards))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_board(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    if let Err(e) = board_in_repo(&state, &repo, id).await {
        return e.into_response();
    }

    match rg_core::board::service::get_board(&state.db, id).await {
        Ok(Some(board)) => (StatusCode::OK, Json(serde_json::json!(board))).into_response(),
        Ok(None) => AppError::not_found("board not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn update_board(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateBoardRequest>,
) -> impl IntoResponse {
    // "validated by repo existence in the board" was not a validation: the
    // board is looked up by a global id, so the route segments never met it.
    if let Err(e) = board_in_repo(&state, &repo, id).await {
        return e.into_response();
    }

    match rg_core::board::service::update_board(&state.db, id, body.name, body.description).await {
        Ok(board) => (StatusCode::OK, Json(serde_json::json!(board))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_board(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    if let Err(e) = board_in_repo(&state, &repo, id).await {
        return e.into_response();
    }

    match rg_core::board::service::delete_board(&state.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // The repository-scoping lookup above is a separate statement from the
        // DELETE, so a request that removed nothing must not confirm a deletion
        // it did not perform.
        Ok(false) => AppError::not_found("board not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn create_column(
    State(state): State<AppState>,
    Path((_, _, board_id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<CreateColumnRequest>,
) -> impl IntoResponse {
    if let Err(e) = board_in_repo(&state, &repo, board_id).await {
        return e.into_response();
    }

    match rg_core::board::service::create_column(&state.db, board_id, body.name, body.color).await {
        Ok(column) => (StatusCode::CREATED, Json(serde_json::json!(column))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn update_column(
    State(state): State<AppState>,
    Path((_, _, board_id, col_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateColumnRequest>,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = column_in_board(&state, board.id, col_id).await {
        return e.into_response();
    }

    match rg_core::board::service::update_column(&state.db, col_id, body.name, body.color).await {
        Ok(column) => (StatusCode::OK, Json(serde_json::json!(column))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_column(
    State(state): State<AppState>,
    Path((_, _, board_id, col_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = column_in_board(&state, board.id, col_id).await {
        return e.into_response();
    }

    match rg_core::board::service::delete_column(&state.db, col_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("column not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn create_card(
    State(state): State<AppState>,
    Path((_, _, board_id, col_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<CreateCardRequest>,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = column_in_board(&state, board.id, col_id).await {
        return e.into_response();
    }
    if let Some(issue_id) = body.issue_id {
        if let Err(e) = issue_in_repo(&state, repo.id, issue_id).await {
            return e.into_response();
        }
    }

    match rg_core::board::service::create_card(&state.db, col_id, body.issue_id, body.note).await {
        Ok(card) => (StatusCode::CREATED, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn update_card(
    State(state): State<AppState>,
    Path((_, _, board_id, card_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateCardRequest>,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = card_in_board(&state, board.id, card_id).await {
        return e.into_response();
    }
    if let Some(Some(issue_id)) = body.issue_id {
        if let Err(e) = issue_in_repo(&state, repo.id, issue_id).await {
            return e.into_response();
        }
    }

    match rg_core::board::service::update_card(&state.db, card_id, body.note, body.issue_id).await {
        Ok(card) => (StatusCode::OK, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn move_card(
    State(state): State<AppState>,
    Path((_, _, board_id, card_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<MoveCardRequest>,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = card_in_board(&state, board.id, card_id).await {
        return e.into_response();
    }
    // The destination column comes from the request body, so it needs the same
    // scoping as the path ids — otherwise a card can be pushed onto any board.
    if let Err(e) = column_in_board(&state, board.id, body.column_id).await {
        return e.into_response();
    }

    match rg_core::board::service::move_card(&state.db, card_id, body.column_id, body.position)
        .await
    {
        Ok(card) => (StatusCode::OK, Json(serde_json::json!(card))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn reorder_cards(
    State(state): State<AppState>,
    Path((_, _, board_id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<ReorderCardsRequest>,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    // Every card id arrives in the body, and the whole batch is rejected if one
    // of them belongs elsewhere — a partial reorder would leave the caller's
    // own board half-applied.
    for (card_id, _) in &body.positions {
        if let Err(e) = card_in_board(&state, board.id, *card_id).await {
            return e.into_response();
        }
    }

    match rg_core::board::service::reorder_cards(&state.db, body.positions).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_card(
    State(state): State<AppState>,
    Path((_, _, board_id, card_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    let board = match board_in_repo(&state, &repo, board_id).await {
        Ok(board) => board,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = card_in_board(&state, board.id, card_id).await {
        return e.into_response();
    }

    match rg_core::board::service::delete_card(&state.db, card_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("card not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}
