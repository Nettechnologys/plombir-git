//! Board service — business logic for project boards (Kanban).
//!
//! Boards belong to a repository or organization. Each board has
//! columns, and each column has cards. Cards can be linked to
//! issues or be free-text notes.

use anyhow::Result;
use chrono::Utc;
use rg_db::entities::board::{ActiveModel as BoardAM, Model as Board};
use rg_db::entities::board_card::{ActiveModel as CardAM, Model as Card};
use rg_db::entities::board_column::{ActiveModel as ColumnAM, Model as Column};
use rg_db::entities::issue::Model as Issue;
use sea_orm::{ActiveValue::Set, DatabaseConnection};

// ── Board CRUD ───────────────────────────────────────────────────────────

/// Create a new project board.
pub async fn create_board(
    db: &DatabaseConnection,
    name: String,
    description: Option<String>,
    repo_id: Option<i64>,
    org_id: Option<i64>,
    created_by: i64,
) -> Result<Board> {
    let now = Utc::now();
    let model = BoardAM {
        name: Set(name),
        description: Set(description),
        repo_id: Set(repo_id),
        org_id: Set(org_id),
        created_by: Set(created_by),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let board = rg_db::ops::board_ops::create_board(db, model).await?;

    // Auto-create default columns
    for (i, (name, color)) in ["To Do", "In Progress", "Done"]
        .iter()
        .zip(["#6366f1", "#f59e0b", "#22c55e"])
        .enumerate()
    {
        let col = ColumnAM {
            board_id: Set(board.id),
            name: Set(name.to_string()),
            color: Set(Some(color.to_string())),
            position: Set(i as i32),
            created_at: Set(now),
            ..Default::default()
        };
        rg_db::ops::board_ops::create_column(db, col).await?;
    }

    Ok(board)
}

/// Get a board by ID with all columns and cards.
pub async fn get_board(db: &DatabaseConnection, id: i64) -> Result<Option<BoardFull>> {
    let board = rg_db::ops::board_ops::find_board_by_id(db, id).await?;
    let Some(board) = board else { return Ok(None) };

    let columns = rg_db::ops::board_ops::list_columns_by_board(db, board.id).await?;
    let mut columns_full = Vec::new();

    for col in columns {
        let cards = rg_db::ops::board_ops::list_cards_by_column(db, col.id).await?;
        let mut cards_full = Vec::with_capacity(cards.len());
        for card in cards {
            let issue = match card.issue_id {
                Some(issue_id) => rg_db::ops::issue_ops::find_by_id(db, issue_id).await?,
                None => None,
            };
            cards_full.push(CardFull { card, issue });
        }
        columns_full.push(ColumnFull {
            column: col,
            cards: cards_full,
        });
    }

    Ok(Some(BoardFull {
        board,
        columns: columns_full,
    }))
}

/// List boards for a repository.
pub async fn list_boards_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Board>> {
    rg_db::ops::board_ops::list_boards_by_repo(db, repo_id).await
}

/// List boards for an organization.
pub async fn list_boards_by_org(db: &DatabaseConnection, org_id: i64) -> Result<Vec<Board>> {
    rg_db::ops::board_ops::list_boards_by_org(db, org_id).await
}

/// Update a board's metadata.
pub async fn update_board(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    description: Option<String>,
) -> Result<Board> {
    let existing = rg_db::ops::board_ops::find_board_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("board not found"))?;

    let mut model: BoardAM = existing.into();
    if let Some(v) = name {
        model.name = Set(v);
    }
    if let Some(v) = description {
        model.description = Set(Some(v));
    }
    model.updated_at = Set(Utc::now());

    rg_db::ops::board_ops::update_board(db, model).await
}

/// Delete a board.
pub async fn delete_board(db: &DatabaseConnection, id: i64) -> Result<()> {
    rg_db::ops::board_ops::delete_board_by_id(db, id).await
}

// ── Column CRUD ──────────────────────────────────────────────────────────

/// Create a new column.
pub async fn create_column(
    db: &DatabaseConnection,
    board_id: i64,
    name: String,
    color: Option<String>,
) -> Result<Column> {
    // Get the next position
    let columns = rg_db::ops::board_ops::list_columns_by_board(db, board_id).await?;
    let pos = columns.len() as i32;

    let now = Utc::now();
    let model = ColumnAM {
        board_id: Set(board_id),
        name: Set(name),
        color: Set(color),
        position: Set(pos),
        created_at: Set(now),
        ..Default::default()
    };

    rg_db::ops::board_ops::create_column(db, model).await
}

/// Update a column.
pub async fn update_column(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    color: Option<String>,
) -> Result<Column> {
    let existing = rg_db::ops::board_ops::find_column_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("column not found"))?;

    let mut model: ColumnAM = existing.into();
    if let Some(v) = name {
        model.name = Set(v);
    }
    if let Some(v) = color {
        model.color = Set(Some(v));
    }
    rg_db::ops::board_ops::update_column(db, model).await
}

/// Delete a column.
pub async fn delete_column(db: &DatabaseConnection, id: i64) -> Result<()> {
    rg_db::ops::board_ops::delete_column_by_id(db, id).await
}

// ── Card CRUD ────────────────────────────────────────────────────────────

/// Create a new card.
pub async fn create_card(
    db: &DatabaseConnection,
    column_id: i64,
    issue_id: Option<i64>,
    note: Option<String>,
) -> Result<Card> {
    let cards = rg_db::ops::board_ops::list_cards_by_column(db, column_id).await?;
    let pos = cards.len() as i32;
    let now = Utc::now();

    let model = CardAM {
        column_id: Set(column_id),
        issue_id: Set(issue_id),
        note: Set(note),
        position: Set(pos),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    rg_db::ops::board_ops::create_card(db, model).await
}

/// Update a card's note or issue link.
pub async fn update_card(
    db: &DatabaseConnection,
    id: i64,
    note: Option<String>,
    issue_id: Option<Option<i64>>,
) -> Result<Card> {
    let existing = rg_db::ops::board_ops::find_card_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("card not found"))?;

    let mut model: CardAM = existing.into();
    if let Some(v) = note {
        model.note = Set(Some(v));
    }
    if let Some(v) = issue_id {
        model.issue_id = Set(v);
    }
    model.updated_at = Set(Utc::now());

    rg_db::ops::board_ops::update_card(db, model).await
}

/// Move a card to another column with a specific position.
pub async fn move_card(
    db: &DatabaseConnection,
    card_id: i64,
    new_column_id: i64,
    new_position: i32,
) -> Result<Card> {
    let existing = rg_db::ops::board_ops::find_card_by_id(db, card_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("card not found"))?;

    let mut model: CardAM = existing.into();
    model.column_id = Set(new_column_id);
    model.position = Set(new_position);
    model.updated_at = Set(Utc::now());

    rg_db::ops::board_ops::update_card(db, model).await
}

/// Reorder cards within a column.
pub async fn reorder_cards(db: &DatabaseConnection, positions: Vec<(i64, i32)>) -> Result<()> {
    rg_db::ops::board_ops::update_card_positions(db, &positions).await
}

/// Delete a card.
pub async fn delete_card(db: &DatabaseConnection, id: i64) -> Result<()> {
    rg_db::ops::board_ops::delete_card_by_id(db, id).await
}

// ── Full response types ──────────────────────────────────────────────────

/// A full board view with columns and cards.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BoardFull {
    pub board: Board,
    pub columns: Vec<ColumnFull>,
}

/// A column with its cards.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ColumnFull {
    pub column: Column,
    pub cards: Vec<CardFull>,
}

/// A card plus optional issue metadata for frontend issue-number links.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CardFull {
    #[serde(flatten)]
    pub card: Card,
    pub issue: Option<Issue>,
}
