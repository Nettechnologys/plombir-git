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

/// Delete a board by ID, reporting whether this call removed it.
///
/// `false` means the row was already gone. The caller's repository-scoping
/// lookup is a separate statement from this delete, so two concurrent DELETEs
/// both pass it and only one of them actually deletes anything.
pub async fn delete_board_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = BoardEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete board")?;
    Ok(result.rows_affected > 0)
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

/// Delete a column by ID, reporting whether this call removed it.
///
/// See [`delete_board_by_id`] for why the boolean matters.
pub async fn delete_column_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = ColumnEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete column")?;
    Ok(result.rows_affected > 0)
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

/// Delete a card by ID, reporting whether this call removed it.
///
/// See [`delete_board_by_id`] for why the boolean matters.
pub async fn delete_card_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = CardEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete card")?;
    Ok(result.rows_affected > 0)
}

/// What a reorder did to the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReorderOutcome {
    /// One committed transaction wrote every requested position.
    Applied,
    /// The batch named cards this board does not own — listed here — so it was
    /// refused whole and nothing was written.
    NotOnBoard(Vec<i64>),
}

/// Publish a whole card reorder as one serialized transaction, scoped to the
/// board that owns the cards.
///
/// Each element of `positions` is a `(card_id, position)` tuple. A card named
/// twice takes its last position — what the statement-per-card loop this
/// replaced did — and the batch is then applied in card-id order, so two
/// concurrent reorders take the same row locks in the same order instead of
/// deadlocking on each other.
///
/// # Concurrency
///
/// A reorder is not a read-modify-write of the stored positions: the caller
/// sends absolute positions for the whole set it is publishing. The operation
/// is therefore *serialized* rather than version-checked, and that is the whole
/// contract — two concurrent reorders of one card set both succeed, and the
/// order left behind is one of them in full. Neither can leave half of its
/// positions applied, and no observer can read a blend of the two. A backend
/// that refuses the transaction outright (SQLite losing its WAL snapshot,
/// PostgreSQL aborting on serialization failure, MySQL picking a deadlock
/// victim) restarts it from a fresh read, bounded to `MAX_REORDER_ATTEMPTS`.
///
/// # Scope
///
/// Board membership is decided *inside* the transaction. A caller's own scope
/// check runs on a separate connection and can be overtaken, so a card deleted
/// or moved to another board in between comes back as
/// [`ReorderOutcome::NotOnBoard`] with nothing written — never as a batch that
/// half-applied before noticing.
pub async fn update_card_positions(
    db: &DatabaseConnection,
    board_id: i64,
    positions: &[(i64, i32)],
) -> Result<ReorderOutcome> {
    update_card_positions_serialized(db, board_id, positions, |_, _| {
        std::future::ready(Ok::<(), anyhow::Error>(()))
    })
    .await
}

/// The bounded transaction loop behind [`update_card_positions`].
///
/// `before_write` is a private test seam, called as `(attempt, write_index)`
/// before each position write: production supplies a ready `Ok`, while the
/// regression tests use it to hold two transactions on the same read snapshot,
/// or to fail one after it has already written part of its batch — neither of
/// which can be provoked by timing alone.
async fn update_card_positions_serialized<F, Fut>(
    db: &DatabaseConnection,
    board_id: i64,
    positions: &[(i64, i32)],
    before_write: F,
) -> Result<ReorderOutcome>
where
    F: Fn(usize, usize) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    const MAX_REORDER_ATTEMPTS: usize = 32;

    // Last mention of a card wins, and the batch is keyed by card id, which is
    // also the lock order every concurrent reorder will follow.
    let wanted: std::collections::BTreeMap<i64, i32> = positions.iter().copied().collect();
    if wanted.is_empty() {
        return Ok(ReorderOutcome::Applied);
    }
    let ids: Vec<i64> = wanted.keys().copied().collect();

    for attempt in 1..=MAX_REORDER_ATTEMPTS {
        let txn = match db.begin().await {
            Ok(txn) => txn,
            Err(error)
                if attempt < MAX_REORDER_ATTEMPTS
                    && crate::is_retryable_transaction_error(&error) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("db: begin card reorder transaction"),
        };

        let write_result: Result<ReorderOutcome> = async {
            let owned: Vec<i64> = CardEntity::find()
                .inner_join(ColumnEntity)
                .filter(board_column::Column::BoardId.eq(board_id))
                .filter(board_card::Column::Id.is_in(ids.iter().copied()))
                .all(&txn)
                .await
                .context("db: read cards for position update")?
                .into_iter()
                .map(|card| card.id)
                .collect();
            let missing: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|id| !owned.contains(id))
                .collect();
            if !missing.is_empty() {
                return Ok(ReorderOutcome::NotOnBoard(missing));
            }

            for (index, (card_id, position)) in wanted.iter().enumerate() {
                before_write(attempt, index).await?;
                // `update_many` writes the one column and reports how many rows
                // it touched; the membership read above already decided that
                // every id is here, so a row whose position is unchanged is not
                // an error the way `ActiveModel::update` would call it.
                CardEntity::update_many()
                    .col_expr(
                        board_card::Column::Position,
                        sea_orm::sea_query::Expr::value(*position),
                    )
                    .filter(board_card::Column::Id.eq(*card_id))
                    .exec(&txn)
                    .await
                    .context("db: update card position")?;
            }
            Ok(ReorderOutcome::Applied)
        }
        .await;

        match write_result {
            Ok(ReorderOutcome::Applied) => {}
            Ok(refused) => {
                txn.rollback()
                    .await
                    .context("db: roll back out-of-scope card reorder")?;
                return Ok(refused);
            }
            Err(error) => {
                let retryable = crate::is_retryable_transaction_error_anyhow(&error);
                if let Err(rollback_error) = txn.rollback().await {
                    return Err(error).context(format!(
                        "db: card reorder failed and its transaction could not be rolled back: \
                         {rollback_error}"
                    ));
                }
                if retryable && attempt < MAX_REORDER_ATTEMPTS {
                    continue;
                }
                if retryable {
                    return Err(error).context(format!(
                        "db: serialize card reorder after {MAX_REORDER_ATTEMPTS} concurrent \
                         conflicts"
                    ));
                }
                return Err(error);
            }
        }

        match txn.commit().await {
            Ok(()) => return Ok(ReorderOutcome::Applied),
            Err(error)
                if attempt < MAX_REORDER_ATTEMPTS
                    && crate::is_retryable_transaction_error(&error) =>
            {
                continue;
            }
            Err(error) if crate::is_retryable_transaction_error(&error) => {
                return Err(error).context(format!(
                    "db: commit card reorder after {MAX_REORDER_ATTEMPTS} concurrent conflicts"
                ));
            }
            Err(error) => return Err(error).context("db: commit card reorder transaction"),
        }
    }

    unreachable!("the bounded reorder loop returns or continues on every attempt")
}

#[cfg(test)]
mod reorder_tests {
    //! card_04054a87159b: `update_card_positions` promised "a single
    //! transaction" and delivered a `find`+`update` per card on the pool. A
    //! failure halfway left the earlier positions applied under an error
    //! response, and two concurrent reorders could both succeed with the board
    //! ending up in an order neither of them asked for.
    //!
    //! The seam these tests drive is not a timing hope: both reorders are held
    //! on the same read snapshot by a barrier, and the fault is injected at a
    //! named write index.

    use super::*;

    /// A throwaway SQLite database file, removed with its WAL siblings on drop.
    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "forgekeep-board-reorder-{label}-{}.db",
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

    /// One board with one column holding `cards` cards at positions `0..cards`.
    ///
    /// The pool holds more than one connection so concurrent reorders really do
    /// run on separate connections, which is what makes them race.
    async fn fixture(label: &str, cards: usize) -> (TempDb, DatabaseConnection, i64, Vec<i64>) {
        let temp = TempDb::new(label);
        let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        crate::run_migrations(&db).await.expect("run migrations");

        let user = crate::ops::user_ops::create_user(
            &db,
            label,
            &format!("{label}@example.invalid"),
            "",
            label,
        )
        .await
        .expect("create the account the board hangs off");

        let (board_id, card_ids) = board_with_cards(&db, user.id, "Sprint", cards).await;
        (temp, db, board_id, card_ids)
    }

    /// A board, its one column, and `cards` cards at positions `0..cards`.
    async fn board_with_cards(
        db: &DatabaseConnection,
        user_id: i64,
        name: &str,
        cards: usize,
    ) -> (i64, Vec<i64>) {
        let now = chrono::Utc::now();
        let board = create_board(
            db,
            BoardAM {
                id: NotSet,
                repo_id: Set(None),
                org_id: Set(None),
                name: Set(name.to_string()),
                description: Set(None),
                created_by: Set(Some(user_id)),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create board");

        let column = create_column(
            db,
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
        .expect("create column");

        let mut card_ids = Vec::with_capacity(cards);
        for index in 0..cards {
            let card = create_card(
                db,
                CardAM {
                    id: NotSet,
                    column_id: Set(column.id),
                    issue_id: Set(None),
                    note: Set(Some(format!("card {index}"))),
                    position: Set(index as i32),
                    created_at: Set(now),
                    updated_at: Set(now),
                },
            )
            .await
            .expect("create card");
            card_ids.push(card.id);
        }

        (board.id, card_ids)
    }

    /// The stored position of every card, in the order the ids are given.
    async fn positions_of(db: &DatabaseConnection, card_ids: &[i64]) -> Vec<i32> {
        let mut positions = Vec::with_capacity(card_ids.len());
        for card_id in card_ids {
            positions.push(
                find_card_by_id(db, *card_id)
                    .await
                    .expect("read card")
                    .expect("card still exists")
                    .position,
            );
        }
        positions
    }

    #[tokio::test]
    async fn a_reorder_applies_every_requested_position() {
        let (_temp, db, board_id, card_ids) = fixture("applies", 3).await;

        let outcome = update_card_positions(
            &db,
            board_id,
            // The middle card is named twice: the last mention decides, exactly
            // as the statement-per-card loop this replaced behaved.
            &[
                (card_ids[0], 2),
                (card_ids[1], 7),
                (card_ids[2], 0),
                (card_ids[1], 1),
            ],
        )
        .await
        .expect("reorder succeeds");

        assert_eq!(outcome, ReorderOutcome::Applied);
        assert_eq!(positions_of(&db, &card_ids).await, vec![2, 1, 0]);
    }

    #[tokio::test]
    async fn a_failure_after_the_first_write_leaves_no_position_applied() {
        let (_temp, db, board_id, card_ids) = fixture("rollback", 3).await;
        let before = positions_of(&db, &card_ids).await;

        let error = update_card_positions_serialized(
            &db,
            board_id,
            &[(card_ids[0], 20), (card_ids[1], 21), (card_ids[2], 22)],
            // The batch is applied in card-id order, so index 1 is reached only
            // after the first card's position has already been written inside
            // the transaction.
            |_, index| async move {
                if index == 1 {
                    anyhow::bail!("injected failure between position writes");
                }
                Ok(())
            },
        )
        .await
        .expect_err("an injected mid-batch failure fails the whole reorder");
        assert!(
            format!("{error:#}").contains("injected failure between position writes"),
            "the injected failure should reach the caller, got: {error:#}"
        );

        assert_eq!(
            positions_of(&db, &card_ids).await,
            before,
            "the write that already ran must roll back with the batch"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_reorders_on_one_snapshot_leave_one_of_them_whole() {
        let (_temp, db, board_id, card_ids) = fixture("snapshot", 3).await;

        // Two orders with no position in common, so a blend of the two is
        // impossible to mistake for either of them.
        let reversed: Vec<(i64, i32)> = vec![(card_ids[0], 2), (card_ids[1], 1), (card_ids[2], 0)];
        let shifted: Vec<(i64, i32)> =
            vec![(card_ids[0], 10), (card_ids[1], 11), (card_ids[2], 12)];

        let on_same_snapshot = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        // Both transactions read before the gate opens, so whichever writes
        // second is refused its snapshot and has to start over. This counter is
        // how the test knows the conflict really happened rather than the two
        // calls having politely queued up.
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_gate = on_same_snapshot.clone();
        let first_attempts = attempts.clone();
        let first =
            update_card_positions_serialized(&db, board_id, &reversed, move |attempt, index| {
                let gate = first_gate.clone();
                let attempts = first_attempts.clone();
                async move {
                    attempts.fetch_max(attempt, std::sync::atomic::Ordering::Relaxed);
                    if attempt == 1 && index == 0 {
                        gate.wait().await;
                    }
                    Ok(())
                }
            });
        let second_gate = on_same_snapshot.clone();
        let second_attempts = attempts.clone();
        let second =
            update_card_positions_serialized(&db, board_id, &shifted, move |attempt, index| {
                let gate = second_gate.clone();
                let attempts = second_attempts.clone();
                async move {
                    attempts.fetch_max(attempt, std::sync::atomic::Ordering::Relaxed);
                    if attempt == 1 && index == 0 {
                        gate.wait().await;
                    }
                    Ok(())
                }
            });

        let (first, second) = tokio::join!(first, second);
        assert_eq!(
            first.expect("the first reorder succeeds"),
            ReorderOutcome::Applied
        );
        assert_eq!(
            second.expect("the second reorder succeeds"),
            ReorderOutcome::Applied
        );

        assert!(
            attempts.load(std::sync::atomic::Ordering::Relaxed) > 1,
            "neither reorder was ever refused its snapshot, so the two never \
             actually overlapped and this test proved nothing"
        );

        let stored = positions_of(&db, &card_ids).await;
        assert!(
            stored == vec![2, 1, 0] || stored == vec![10, 11, 12],
            "the board must hold one submitted order whole, got {stored:?}"
        );
    }

    #[tokio::test]
    async fn a_card_on_another_board_refuses_the_whole_batch() {
        let (_temp, db, board_id, card_ids) = fixture("scope", 2).await;
        let before = positions_of(&db, &card_ids).await;

        let owner = crate::ops::user_ops::find_by_username(&db, "scope")
            .await
            .expect("read the fixture account")
            .expect("the fixture account exists");
        let (_other_board, other_cards) = board_with_cards(&db, owner.id, "Other", 1).await;

        let outcome = update_card_positions(
            &db,
            board_id,
            &[(card_ids[0], 5), (other_cards[0], 6), (card_ids[1], 7)],
        )
        .await
        .expect("an out-of-scope card is an answer, not a failure");

        assert_eq!(outcome, ReorderOutcome::NotOnBoard(vec![other_cards[0]]));
        assert_eq!(
            positions_of(&db, &card_ids).await,
            before,
            "a refused batch must not write the cards that were in scope"
        );
        assert_eq!(
            positions_of(&db, &other_cards).await,
            vec![0],
            "a refused batch must not write the card that was out of scope either"
        );
    }
}
