//! Database operations for project boards, columns, and cards.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::board::{self, ActiveModel as BoardAM, Entity as BoardEntity, Model as Board};
use crate::entities::board_card::{
    self, ActiveModel as CardAM, Entity as CardEntity, Model as Card,
};
use crate::entities::board_column::{
    self, ActiveModel as ColumnAM, Entity as ColumnEntity, Model as Column,
};

// ── Board ────────────────────────────────────────────────────────────────

/// Create a new board.
pub async fn create_board(db: &DatabaseConnection, model: BoardAM) -> Result<Board> {
    model.insert(db).await.context("db: create board")
}

/// Find a board by its ID.
pub async fn find_board_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Board>> {
    BoardEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find board")
}

/// List boards belonging to a repository, ordered by name.
pub async fn list_boards_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Board>> {
    BoardEntity::find()
        .filter(board::Column::RepoId.eq(repo_id))
        .order_by_asc(board::Column::Name)
        .all(db)
        .await
        .context("db: list boards by repo")
}

/// List boards belonging to an organization.
pub async fn list_boards_by_org(db: &DatabaseConnection, org_id: i64) -> Result<Vec<Board>> {
    BoardEntity::find()
        .filter(board::Column::OrgId.eq(org_id))
        .order_by_asc(board::Column::Name)
        .all(db)
        .await
        .context("db: list boards by org")
}

/// Update a board's metadata (name, description).
pub async fn update_board(db: &DatabaseConnection, model: BoardAM) -> Result<Board> {
    model.update(db).await.context("db: update board")
}

/// Delete a board by ID.
pub async fn delete_board_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    BoardEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete board")?;
    Ok(())
}

// ── Columns ──────────────────────────────────────────────────────────────

/// Create a new column on a board.
pub async fn create_column(db: &DatabaseConnection, model: ColumnAM) -> Result<Column> {
    model.insert(db).await.context("db: create column")
}

/// Find a column by its ID.
pub async fn find_column_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Column>> {
    ColumnEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find column")
}

/// List columns on a board, ordered by position.
pub async fn list_columns_by_board(db: &DatabaseConnection, board_id: i64) -> Result<Vec<Column>> {
    ColumnEntity::find()
        .filter(board_column::Column::BoardId.eq(board_id))
        .order_by_asc(board_column::Column::Position)
        .all(db)
        .await
        .context("db: list columns by board")
}

/// Update a column's name.
pub async fn update_column(db: &DatabaseConnection, model: ColumnAM) -> Result<Column> {
    model.update(db).await.context("db: update column")
}

/// Delete a column by ID.
pub async fn delete_column_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    ColumnEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete column")?;
    Ok(())
}

// ── Cards ────────────────────────────────────────────────────────────────

/// Create a new card in a column.
pub async fn create_card(db: &DatabaseConnection, model: CardAM) -> Result<Card> {
    model.insert(db).await.context("db: create card")
}

/// Find a card by its ID.
pub async fn find_card_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Card>> {
    CardEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find card")
}

/// List cards in a column, ordered by position.
pub async fn list_cards_by_column(db: &DatabaseConnection, column_id: i64) -> Result<Vec<Card>> {
    CardEntity::find()
        .filter(board_card::Column::ColumnId.eq(column_id))
        .order_by_asc(board_card::Column::Position)
        .all(db)
        .await
        .context("db: list cards by column")
}

/// Update a card's title or note.
pub async fn update_card(db: &DatabaseConnection, model: CardAM) -> Result<Card> {
    model.update(db).await.context("db: update card")
}

/// Delete a card by ID.
pub async fn delete_card_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    CardEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete card")?;
    Ok(())
}

/// Update positions of multiple cards in a single transaction.
///
/// Each element in `positions` is a `(card_id, position)` tuple.
pub async fn update_card_positions(
    db: &DatabaseConnection,
    positions: &[(i64, i32)],
) -> Result<()> {
    for (card_id, pos) in positions {
        let mut am: CardAM = CardEntity::find_by_id(*card_id)
            .one(db)
            .await
            .context("db: find card for position update")?
            .ok_or_else(|| anyhow::anyhow!("card {} not found", card_id))?
            .into();
        am.position = Set(*pos);
        am.update(db).await.context("db: update card position")?;
    }
    Ok(())
}
