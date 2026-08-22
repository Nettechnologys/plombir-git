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

// ── Appending at the end of an ordered scope ─────────────────────────────

/// How many times an append may lose its transaction to a concurrent writer
/// before it gives up. Every attempt re-reads the last position, so this is a
/// runaway guard, not a concurrency budget.
///
/// # What appending guarantees
///
/// The position is read and written inside one transaction, and it is
/// `MAX(position) + 1` over the rows that are there — never a count of them.
/// That is the difference that matters most here: a count is wrong the moment
/// anything is removed from the middle, with no concurrency involved at all.
/// Three cards at `{0, 1, 2}` minus the middle one leaves `{0, 2}`, and a
/// fourth card sized off `len()` lands on top of the card at `2`
/// (card_4a340e38f0e2).
///
/// # What it does not
///
/// `position` carries no UNIQUE key, and cannot be given one: a reorder
/// permutes the positions of a whole column, and every backend here except
/// PostgreSQL with a deferred constraint checks uniqueness per statement rather
/// than at commit — so an ordinary swap would be refused halfway through its
/// own batch. Without that key, two appends into one column on PostgreSQL or
/// MySQL can still read the same maximum and both write it; no error is raised
/// and the scope holds two rows at one position.
///
/// That residue is bounded rather than eliminated, which is why both listings
/// order by `(position, id)`: a tie is then broken identically by every reader
/// instead of however the engine happened to scan, so the board stops
/// reshuffling between two reads and the next drag-and-drop — which publishes
/// absolute positions for the whole set — writes the tie away.
const MAX_APPEND_ATTEMPTS: usize = 32;

/// Yield to the writer that made this attempt retryable, using the database
/// crate's one contention policy rather than spending the next attempt at once.
async fn wait_for_contention(attempt: usize) {
    tokio::time::sleep(crate::contention::contention_backoff(attempt)).await;
}

/// The position after `max`, and the first position when there is nothing yet.
fn next_after(max: Option<i32>) -> i32 {
    max.map_or(0, |last| last.saturating_add(1))
}

/// Commit an append, or report that the attempt should be retried (`None`).
///
/// Only the backends' own retryable-transaction outcomes are retried. A foreign
/// key, a check constraint or a dead connection stays an error, because
/// re-reading the last position cannot fix any of them and the loop would spin.
async fn finish_append<T>(
    txn: DatabaseTransaction,
    appended: Result<T>,
    attempt: usize,
    what: &str,
) -> Result<Option<T>> {
    let row = match appended {
        Ok(row) => row,
        Err(error) => {
            let retryable = crate::is_retryable_transaction_error_anyhow(&error);
            if let Err(rollback_error) = txn.rollback().await {
                return Err(error).context(format!(
                    "db: board {what} append failed and its transaction could not be rolled \
                     back: {rollback_error}"
                ));
            }
            if retryable && attempt < MAX_APPEND_ATTEMPTS {
                wait_for_contention(attempt).await;
                return Ok(None);
            }
            if retryable {
                return Err(error).context(format!(
                    "db: append a board {what} after {MAX_APPEND_ATTEMPTS} concurrent conflicts"
                ));
            }
            return Err(error);
        }
    };

    match txn.commit().await {
        Ok(()) => Ok(Some(row)),
        Err(error)
            if attempt < MAX_APPEND_ATTEMPTS && crate::is_retryable_transaction_error(&error) =>
        {
            wait_for_contention(attempt).await;
            Ok(None)
        }
        Err(error) if crate::is_retryable_transaction_error(&error) => Err(error).context(format!(
            "db: commit a board {what} append after {MAX_APPEND_ATTEMPTS} concurrent conflicts"
        )),
        Err(error) => Err(error).context(format!("db: commit board {what} append transaction")),
    }
}

// ── Columns ──────────────────────────────────────────────────────────────

/// Create a new column on a board, at the position the caller chose.
pub async fn create_column(db: &DatabaseConnection, model: ColumnAM) -> Result<Column> {
    model.insert(db).await.context("db: create column")
}

/// Append a column to its board, allocating `position` from the stored rows.
///
/// Whatever the caller left in `model.position` is overwritten. See
/// [`MAX_APPEND_ATTEMPTS`] for what appending does and does not promise.
pub async fn create_column_at_end(db: &DatabaseConnection, model: ColumnAM) -> Result<Column> {
    let board_id = *model
        .board_id
        .try_as_ref()
        .context("db: append a column without saying which board")?;

    for attempt in 1..=MAX_APPEND_ATTEMPTS {
        let txn = match db.begin().await {
            Ok(txn) => txn,
            Err(error)
                if attempt < MAX_APPEND_ATTEMPTS
                    && crate::is_retryable_transaction_error(&error) =>
            {
                wait_for_contention(attempt).await;
                continue;
            }
            Err(error) => return Err(error).context("db: begin column append transaction"),
        };

        let appended: Result<Column> = async {
            let max: Option<i32> = ColumnEntity::find()
                .filter(board_column::Column::BoardId.eq(board_id))
                .select_only()
                .column_as(board_column::Column::Position.max(), "max_position")
                .into_tuple::<Option<i32>>()
                .one(&txn)
                .await
                .context("db: read the last column position")?
                .flatten();
            let mut candidate = model.clone();
            candidate.position = Set(next_after(max));
            candidate.insert(&txn).await.context("db: create column")
        }
        .await;

        match finish_append(txn, appended, attempt, "column").await? {
            Some(column) => return Ok(column),
            None => continue,
        }
    }

    unreachable!("the bounded column append loop returns or continues on every attempt")
}

/// Find a column by its ID.
pub async fn find_column_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Column>> {
    ColumnEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find column")
}

/// List columns on a board, ordered by position and then by id.
///
/// `position` is not unique (see [`MAX_APPEND_ATTEMPTS`]), so on its own it is
/// a partial order: two columns sharing a position come back in whatever order
/// the engine scanned, which can differ between two reads of an unchanged
/// board. The `id` tiebreaker makes the order total, so every reader — and
/// every consecutive read — sees the same board.
pub async fn list_columns_by_board(db: &DatabaseConnection, board_id: i64) -> Result<Vec<Column>> {
    ColumnEntity::find()
        .filter(board_column::Column::BoardId.eq(board_id))
        .order_by_asc(board_column::Column::Position)
        .order_by_asc(board_column::Column::Id)
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

/// Create a new card in a column, at the position the caller chose.
pub async fn create_card(db: &DatabaseConnection, model: CardAM) -> Result<Card> {
    model.insert(db).await.context("db: create card")
}

/// Append a card to its column, allocating `position` from the stored rows.
///
/// Whatever the caller left in `model.position` is overwritten. See
/// [`MAX_APPEND_ATTEMPTS`] for what appending does and does not promise.
pub async fn create_card_at_end(db: &DatabaseConnection, model: CardAM) -> Result<Card> {
    let column_id = *model
        .column_id
        .try_as_ref()
        .context("db: append a card without saying which column")?;

    for attempt in 1..=MAX_APPEND_ATTEMPTS {
        let txn = match db.begin().await {
            Ok(txn) => txn,
            Err(error)
                if attempt < MAX_APPEND_ATTEMPTS
                    && crate::is_retryable_transaction_error(&error) =>
            {
                wait_for_contention(attempt).await;
                continue;
            }
            Err(error) => return Err(error).context("db: begin card append transaction"),
        };

        let appended: Result<Card> = async {
            let max: Option<i32> = CardEntity::find()
                .filter(board_card::Column::ColumnId.eq(column_id))
                .select_only()
                .column_as(board_card::Column::Position.max(), "max_position")
                .into_tuple::<Option<i32>>()
                .one(&txn)
                .await
                .context("db: read the last card position")?
                .flatten();
            let mut candidate = model.clone();
            candidate.position = Set(next_after(max));
            candidate.insert(&txn).await.context("db: create card")
        }
        .await;

        match finish_append(txn, appended, attempt, "card").await? {
            Some(card) => return Ok(card),
            None => continue,
        }
    }

    unreachable!("the bounded card append loop returns or continues on every attempt")
}

/// Find a card by its ID.
pub async fn find_card_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Card>> {
    CardEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find card")
}

/// List cards in a column, ordered by position and then by id.
///
/// See [`list_columns_by_board`] for why the tiebreaker is not decoration.
pub async fn list_cards_by_column(db: &DatabaseConnection, column_id: i64) -> Result<Vec<Card>> {
    CardEntity::find()
        .filter(board_card::Column::ColumnId.eq(column_id))
        .order_by_asc(board_card::Column::Position)
        .order_by_asc(board_card::Column::Id)
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
                wait_for_contention(attempt).await;
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
                    wait_for_contention(attempt).await;
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
                wait_for_contention(attempt).await;
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

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
///
/// A file rather than `sqlite::memory:` because both test modules below need a
/// pool of real connections: an in-memory database is one connection wide, and
/// two writers that cannot actually run at once cannot race.
#[cfg(test)]
struct TempDb {
    path: std::path::PathBuf,
}

#[cfg(test)]
impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-board-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

#[cfg(test)]
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

/// Take SQLite's database-wide writer slot on a separate pool.
///
/// The no-op row update is intentional: these tests need a held writer, not a
/// data change that the operation under test has to account for.
#[cfg(test)]
async fn hold_sqlite_writer(db: &DatabaseConnection, board_id: i64) -> DatabaseTransaction {
    let txn = db.begin().await.expect("begin the holding transaction");
    txn.execute_unprepared(&format!(
        "UPDATE boards SET updated_at = updated_at WHERE id = {board_id}"
    ))
    .await
    .expect("take SQLite's writer slot");
    txn
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
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_attempts = attempts.clone();

        let error = update_card_positions_serialized(
            &db,
            board_id,
            &[(card_ids[0], 20), (card_ids[1], 21), (card_ids[2], 22)],
            // The batch is applied in card-id order, so index 1 is reached only
            // after the first card's position has already been written inside
            // the transaction.
            move |attempt, index| {
                counted_attempts.fetch_max(attempt, std::sync::atomic::Ordering::Relaxed);
                async move {
                    if index == 1 {
                        anyhow::bail!("injected failure between position writes");
                    }
                    Ok(())
                }
            },
        )
        .await
        .expect_err("an injected mid-batch failure fails the whole reorder");
        assert!(
            format!("{error:#}").contains("injected failure between position writes"),
            "the injected failure should reach the caller, got: {error:#}"
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a non-contention failure must be returned without retrying"
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

    /// card_f865972ce3a6: a held SQLite writer used to make the read-first
    /// reorder spend all thirty-two attempts before that writer could commit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_held_writer_does_not_burn_the_reorder_attempt_budget() {
        const HOLD: std::time::Duration = std::time::Duration::from_millis(100);

        let (temp, db, board_id, card_ids) = fixture("held-writer-reorder", 3).await;
        let holder = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
            .await
            .expect("connect the independent writer pool");
        let held = hold_sqlite_writer(&holder, board_id).await;

        let wanted = vec![(card_ids[0], 12), (card_ids[1], 11), (card_ids[2], 10)];
        let reorder = tokio::spawn({
            let db = db.clone();
            let wanted = wanted.clone();
            async move { update_card_positions(&db, board_id, &wanted).await }
        });

        tokio::time::sleep(HOLD).await;
        held.commit().await.expect("release SQLite's writer slot");

        assert_eq!(
            reorder
                .await
                .expect("the reorder task did not panic")
                .expect("a reorder that only had to wait for a writer must succeed"),
            ReorderOutcome::Applied
        );
        assert_eq!(positions_of(&db, &card_ids).await, vec![12, 11, 10]);
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

#[cfg(test)]
mod append_position_tests {
    //! card_4a340e38f0e2: the position of a new column or card used to be the
    //! `len()` of the list that was just read. That is not "the end" — it is a
    //! count, and a count stops naming a free position the moment anything is
    //! removed from the middle, with no concurrency involved at all.
    //!
    //! The concurrent half is bounded rather than eliminated (see
    //! [`MAX_APPEND_ATTEMPTS`]), so the second thing these tests pin down is the
    //! property that holds either way: the listing order is total, and stays
    //! the same between two reads even when two rows do share a position.

    use super::*;

    /// A board with one empty column, on a pool wide enough to race on.
    async fn fixture(label: &str) -> (TempDb, DatabaseConnection, i64, i64) {
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

        let now = chrono::Utc::now();
        let board = create_board(
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
        let column = create_column_at_end(
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
        .expect("create the first column");

        (temp, db, board.id, column.id)
    }

    fn card(column_id: i64, note: &str) -> CardAM {
        let now = chrono::Utc::now();
        CardAM {
            id: NotSet,
            column_id: Set(column_id),
            issue_id: Set(None),
            note: Set(Some(note.to_string())),
            position: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
    }

    /// The whole bug without a second thread in sight: three cards, remove the
    /// middle one, append a fourth. A count says "3" and the board already has
    /// a card at 2.
    #[tokio::test]
    async fn appending_after_a_deletion_from_the_middle_does_not_reuse_a_taken_position() {
        let (_temp, db, _board_id, column_id) = fixture("append-after-delete").await;

        let mut created = Vec::new();
        for note in ["first", "second", "third"] {
            created.push(
                create_card_at_end(&db, card(column_id, note))
                    .await
                    .expect("append a card"),
            );
        }
        assert_eq!(
            created.iter().map(|c| c.position).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the fixture did not append three consecutive positions"
        );

        assert!(
            delete_card_by_id(&db, created[1].id)
                .await
                .expect("delete the middle card"),
            "the middle card was already gone"
        );

        let fourth = create_card_at_end(&db, card(column_id, "fourth"))
            .await
            .expect("append after the deletion");
        assert_eq!(
            fourth.position, 3,
            "the new card was positioned off a count of the survivors, not off the last position"
        );

        let positions: Vec<i32> = list_cards_by_column(&db, column_id)
            .await
            .expect("list the column")
            .into_iter()
            .map(|card| card.position)
            .collect();
        let mut distinct = positions.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            positions.len(),
            "two cards share a position: {positions:?}"
        );
    }

    /// Columns are allocated by the same rule and break the same way.
    #[tokio::test]
    async fn appending_a_column_after_a_deletion_does_not_reuse_a_taken_position() {
        let (_temp, db, board_id, first_column) = fixture("append-column-after-delete").await;

        let now = chrono::Utc::now();
        let mut created = vec![first_column];
        for name in ["Doing", "Done"] {
            created.push(
                create_column_at_end(
                    &db,
                    ColumnAM {
                        id: NotSet,
                        board_id: Set(board_id),
                        name: Set(name.to_string()),
                        color: Set(None),
                        position: Set(0),
                        created_at: Set(now),
                    },
                )
                .await
                .expect("append a column")
                .id,
            );
        }

        assert!(
            delete_column_by_id(&db, created[1])
                .await
                .expect("delete the middle column"),
            "the middle column was already gone"
        );

        let appended = create_column_at_end(
            &db,
            ColumnAM {
                id: NotSet,
                board_id: Set(board_id),
                name: Set("Blocked".to_string()),
                color: Set(None),
                position: Set(0),
                created_at: Set(now),
            },
        )
        .await
        .expect("append after the deletion");
        assert_eq!(
            appended.position, 3,
            "the new column was positioned off a count of the survivors"
        );
    }

    /// Two appends into one column, started together on separate connections.
    ///
    /// On SQLite the write lock is what serializes them, and a loser retries
    /// from a fresh read rather than failing the caller — so both creates
    /// succeed and the positions differ. The assertion is deliberately on the
    /// *outcome* (two distinct positions, no error), not on which one won.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_into_one_column_take_different_positions() {
        let (_temp, db, _board_id, column_id) = fixture("append-race").await;
        create_card_at_end(&db, card(column_id, "already here"))
            .await
            .expect("seed the column");

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut racers = Vec::new();
        for note in ["racer one", "racer two"] {
            let db = db.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            racers.push(tokio::spawn(async move {
                barrier.wait().await;
                create_card_at_end(&db, card(column_id, note)).await
            }));
        }

        let mut positions = Vec::new();
        for racer in racers {
            positions.push(
                racer
                    .await
                    .expect("append task did not panic")
                    .expect("a race this server opened must not be the caller's failure")
                    .position,
            );
        }
        positions.sort_unstable();
        assert_eq!(
            positions,
            vec![1, 2],
            "two concurrent appends landed on the same position"
        );
    }

    /// card_f865972ce3a6: both append loops read before their INSERT. While a
    /// writer is held SQLite refuses that lock promotion immediately, so an
    /// unwaiting loop exhausts all thirty-two attempts inside this hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_held_writer_does_not_burn_column_or_card_append_budgets() {
        const HOLD: std::time::Duration = std::time::Duration::from_millis(100);

        let (temp, db, board_id, column_id) = fixture("held-writer-append").await;
        let holder = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
            .await
            .expect("connect the independent writer pool");

        let held = hold_sqlite_writer(&holder, board_id).await;
        let column_append = tokio::spawn({
            let db = db.clone();
            async move {
                let now = chrono::Utc::now();
                create_column_at_end(
                    &db,
                    ColumnAM {
                        id: NotSet,
                        board_id: Set(board_id),
                        name: Set("Waiting".to_string()),
                        color: Set(None),
                        position: Set(0),
                        created_at: Set(now),
                    },
                )
                .await
            }
        });
        tokio::time::sleep(HOLD).await;
        held.commit().await.expect("release SQLite's writer slot");
        let column = column_append
            .await
            .expect("the column append task did not panic")
            .expect("a column append that only had to wait for a writer must succeed");
        assert_eq!(column.position, 1);

        let held = hold_sqlite_writer(&holder, board_id).await;
        let card_append = tokio::spawn({
            let db = db.clone();
            async move { create_card_at_end(&db, card(column_id, "waiting")).await }
        });
        tokio::time::sleep(HOLD).await;
        held.commit().await.expect("release SQLite's writer slot");
        let appended = card_append
            .await
            .expect("the card append task did not panic")
            .expect("a card append that only had to wait for a writer must succeed");
        assert_eq!(appended.position, 0);
    }

    /// Even with a duplicate position planted directly in the table — which is
    /// what remains possible on the backends without a single-writer lock — the
    /// order two consecutive reads see must be the same order.
    #[tokio::test]
    async fn a_shared_position_is_still_listed_in_one_stable_order() {
        let (_temp, db, board_id, column_id) = fixture("append-tiebreak").await;

        let mut ids = Vec::new();
        for note in ["first", "second", "third"] {
            ids.push(
                create_card_at_end(&db, card(column_id, note))
                    .await
                    .expect("append a card")
                    .id,
            );
        }
        // The collision an append cannot rule out, written by hand so the
        // tiebreaker is tested rather than hoped for.
        CardEntity::update_many()
            .col_expr(
                board_card::Column::Position,
                sea_orm::sea_query::Expr::value(0),
            )
            .filter(board_card::Column::Id.eq(ids[2]))
            .exec(&db)
            .await
            .expect("plant a duplicate position");

        let first_read: Vec<i64> = list_cards_by_column(&db, column_id)
            .await
            .expect("list the column")
            .into_iter()
            .map(|card| card.id)
            .collect();
        let second_read: Vec<i64> = list_cards_by_column(&db, column_id)
            .await
            .expect("list the column again")
            .into_iter()
            .map(|card| card.id)
            .collect();

        assert_eq!(
            first_read,
            vec![ids[0], ids[2], ids[1]],
            "the tie between two cards at position 0 is not broken by id"
        );
        assert_eq!(
            first_read, second_read,
            "an unchanged board came back in two different orders"
        );

        // Columns share the rule, so they share the test.
        ColumnEntity::update_many()
            .col_expr(
                board_column::Column::Position,
                sea_orm::sea_query::Expr::value(0),
            )
            .filter(board_column::Column::BoardId.eq(board_id))
            .exec(&db)
            .await
            .expect("collapse every column onto one position");
        let columns: Vec<i64> = list_columns_by_board(&db, board_id)
            .await
            .expect("list the columns")
            .into_iter()
            .map(|column| column.id)
            .collect();
        let mut by_id = columns.clone();
        by_id.sort_unstable();
        assert_eq!(
            columns, by_id,
            "columns sharing a position are not ordered by id"
        );
    }
}
