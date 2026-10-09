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
        created_by: Set(Some(created_by)),
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
                Some(issue_id) => match rg_db::ops::issue_ops::find_by_id(db, issue_id).await? {
                    Some(issue) => Some(crate::issue::issue_with_labels(db, issue).await?),
                    None => None,
                },
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

/// Update a board's metadata.
pub async fn update_board(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    description: Option<String>,
) -> Result<Board> {
    update_board_after_read(db, id, name, description, || std::future::ready(Ok(()))).await
}

/// Testable read/write boundary behind [`update_board`].
///
/// `after_read` is a private deterministic seam: production does nothing,
/// while the regression commits a delete after this service has observed the
/// row and before the conditional update runs.
async fn update_board_after_read<F, Fut>(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    description: Option<String>,
    after_read: F,
) -> Result<Board>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    rg_db::ops::board_ops::find_board_by_id(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("board"))?;
    after_read().await?;

    rg_db::ops::board_ops::update_board(db, id, name, description, Utc::now())
        .await?
        .ok_or_else(|| crate::error::not_found("board"))
}

/// Delete a board. `false` means the row was already gone — see
/// [`rg_db::ops::board_ops::delete_board_by_id`].
pub async fn delete_board(db: &DatabaseConnection, id: i64) -> Result<bool> {
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
    // A column colour is written into a `style` attribute by the web client
    // and the CSP allows inline styles, so the stored value has to be a colour
    // and nothing else — see [`crate::validate_hex_color`].
    let color = color
        .map(|value| crate::validate_hex_color(&value))
        .transpose()?;
    let now = Utc::now();
    let model = ColumnAM {
        board_id: Set(board_id),
        name: Set(name),
        color: Set(color),
        // Overwritten by the append: the position is read and written inside
        // one transaction, from the stored rows rather than from a count of
        // them, so removing a column from the middle no longer aims the next
        // one at a position that is still occupied (card_4a340e38f0e2).
        position: Set(0),
        created_at: Set(now),
        ..Default::default()
    };

    rg_db::ops::board_ops::create_column_at_end(db, model).await
}

/// Update a column.
pub async fn update_column(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    color: Option<String>,
) -> Result<Column> {
    update_column_after_read(db, id, name, color, || std::future::ready(Ok(()))).await
}

/// Testable read/write boundary behind [`update_column`].
async fn update_column_after_read<F, Fut>(
    db: &DatabaseConnection,
    id: i64,
    name: Option<String>,
    color: Option<String>,
    after_read: F,
) -> Result<Column>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    // Validated before the read: a malformed colour is the request's fault and
    // never a reason to touch the database — see [`create_column`].
    let color = color
        .map(|value| crate::validate_hex_color(&value))
        .transpose()?;

    rg_db::ops::board_ops::find_column_by_id(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("board column"))?;
    after_read().await?;

    rg_db::ops::board_ops::update_column(db, id, name, color)
        .await?
        .ok_or_else(|| crate::error::not_found("board column"))
}

/// Delete a column. `false` means the row was already gone.
pub async fn delete_column(db: &DatabaseConnection, id: i64) -> Result<bool> {
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
    let now = Utc::now();

    let model = CardAM {
        column_id: Set(column_id),
        issue_id: Set(issue_id),
        note: Set(note),
        // Overwritten by the append — see [`create_column`].
        position: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    rg_db::ops::board_ops::create_card_at_end(db, model).await
}

/// Update a card's note or issue link.
pub async fn update_card(
    db: &DatabaseConnection,
    id: i64,
    note: Option<String>,
    issue_id: Option<Option<i64>>,
) -> Result<Card> {
    rg_db::ops::board_ops::update_card_fields(db, id, note, issue_id, Utc::now())
        .await?
        .ok_or_else(|| crate::error::not_found("board card"))
}

/// Move a card to another column with a specific position.
pub async fn move_card(
    db: &DatabaseConnection,
    card_id: i64,
    new_column_id: i64,
    new_position: i32,
) -> Result<Card> {
    rg_db::ops::board_ops::move_card(db, card_id, new_column_id, new_position, Utc::now())
        .await?
        .ok_or_else(|| crate::error::not_found("board card"))
}

/// Reorder cards within a column, as one serialized publication.
///
/// The whole batch commits or none of it does, and a card that left `board_id`
/// between the caller's scope check and the write is the same answer as no such
/// card — not a batch that applied the positions it managed to reach first.
pub async fn reorder_cards(
    db: &DatabaseConnection,
    board_id: i64,
    column_id: Option<i64>,
    positions: Vec<(i64, i32)>,
) -> Result<()> {
    match rg_db::ops::board_ops::update_card_positions(db, board_id, column_id, &positions).await? {
        rg_db::ops::board_ops::ReorderOutcome::Applied => Ok(()),
        rg_db::ops::board_ops::ReorderOutcome::NotOnBoard(_) => {
            Err(crate::error::not_found("board card"))
        }
        rg_db::ops::board_ops::ReorderOutcome::Moved(_) => Err(crate::error::conflict(
            "a board card moved while its order was being changed",
        )),
    }
}

/// Delete a card. `false` means the row was already gone.
pub async fn delete_card(db: &DatabaseConnection, id: i64) -> Result<bool> {
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
    pub issue: Option<crate::issue::IssueWithLabels>,
}

#[cfg(test)]
mod update_delete_tests {
    //! card_0ac3b290f3a9: both PATCH paths read the row before writing it. These
    //! tests commit the competing DELETE through an after-read seam, so the
    //! interleaving is guaranteed rather than left to scheduler timing.

    use super::*;
    use sea_orm::NotSet;

    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "plombir-git-board-update-{label}-{}.db",
                    uuid::Uuid::new_v4().simple()
                )),
            }
        }

        fn url(&self) -> String {
            format!("sqlite://{}?mode=rwc", self.path.display())
        }
    }

    impl Drop for TempDb {
        #[allow(
            clippy::let_underscore_must_use,
            reason = "cleanup must not mask the assertion that failed the test"
        )]
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    async fn fixture(label: &str) -> (TempDb, DatabaseConnection, i64, i64) {
        let temp = TempDb::new(label);
        let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            label,
            &format!("{label}@example.invalid"),
            "",
            label,
        )
        .await
        .expect("create board owner");
        let now = Utc::now();
        let board = rg_db::ops::board_ops::create_board(
            &db,
            BoardAM {
                id: NotSet,
                repo_id: Set(None),
                org_id: Set(None),
                name: Set("Sprint".to_string()),
                description: Set(None),
                created_by: Set(Some(user.id)),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create board");
        let column = rg_db::ops::board_ops::create_column(
            &db,
            ColumnAM {
                id: NotSet,
                board_id: Set(board.id),
                name: Set("Todo".to_string()),
                color: Set(None),
                position: Set(0),
                created_at: Set(now),
            },
        )
        .await
        .expect("create board column");

        (temp, db, board.id, column.id)
    }

    fn assert_not_found(error: &anyhow::Error, resource: &'static str) {
        let typed = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the lost race must stay classifiable as HTTP 404");
        assert_eq!(typed.resource, resource);
    }

    #[tokio::test]
    async fn board_delete_after_the_service_read_is_typed_not_found() {
        let (_temp, db, board_id, _column_id) = fixture("board-delete").await;

        let error = update_board_after_read(
            &db,
            board_id,
            Some("Too late".to_string()),
            None,
            || async {
                assert!(rg_db::ops::board_ops::delete_board_by_id(&db, board_id)
                    .await
                    .expect("the competing board delete succeeds"));
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful update");

        assert_not_found(&error, "board");
    }

    #[tokio::test]
    async fn column_delete_after_the_service_read_is_typed_not_found() {
        let (_temp, db, _board_id, column_id) = fixture("column-delete").await;

        let error = update_column_after_read(
            &db,
            column_id,
            Some("Too late".to_string()),
            Some("#000000".to_string()),
            || async {
                assert!(rg_db::ops::board_ops::delete_column_by_id(&db, column_id)
                    .await
                    .expect("the competing column delete succeeds"));
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful update");

        assert_not_found(&error, "board column");
    }
}
