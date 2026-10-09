//! Code indexing service for code search.
//!
//! This module provides functionality to index Git repository contents
//! into the `code_fts` table for fast full-text search across all backends.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rg_core::search::code_indexer::CodeIndexer;
//!
//! let indexer = CodeIndexer::new(db.clone());
//! indexer.index_repository(repo_id, repo_path, "main").await?;
//! ```

use anyhow::{Context, Result};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, Statement,
    TransactionTrait, Value,
};
use std::path::{Path, PathBuf};

use crate::db_retry::{classify, classify_anyhow};
use crate::search::dialect::{code_fts_snippet_expr, fts_match, CODE_FTS_COLS};

/// Largest single source file retained by the repository code index.
///
/// The object header is compared with this ceiling before the blob is decoded,
/// so an intentionally huge committed file costs metadata, not its full body.
const MAX_INDEXED_FILE_BYTES: u64 = 1024 * 1024;

/// Largest complete source snapshot one repository's code index may hold.
///
/// A rebuild retains the complete next generation before it starts writing
/// (see [`CodeIndexer::index_repository`]), so this ceiling bounds that buffer
/// instead of silently publishing only the prefix that happened to fit. An
/// incremental refresh enforces it against the snapshot it would leave.
const MAX_INDEXED_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Independent backstop for repositories made of tiny or empty source files.
const MAX_INDEXED_FILE_COUNT: usize = 100_000;

/// Map file extensions to programming languages.
const EXTENSION_TO_LANGUAGE: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("py", "Python"),
    ("js", "JavaScript"),
    ("ts", "TypeScript"),
    ("jsx", "JavaScript (JSX)"),
    ("tsx", "TypeScript (TSX)"),
    ("go", "Go"),
    ("java", "Java"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("hpp", "C++"),
    ("cs", "C#"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("swift", "Swift"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("zsh", "Shell"),
    ("sql", "SQL"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("less", "LESS"),
    ("xml", "XML"),
    ("json", "JSON"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("toml", "TOML"),
    ("lock", "Lock file"),
    ("md", "Markdown"),
    ("txt", "Text"),
    ("rst", "reStructuredText"),
    ("lua", "Lua"),
    ("pl", "Perl"),
    ("pm", "Perl"),
    ("r", "R"),
    ("scala", "Scala"),
    ("clj", "Clojure"),
    ("cljs", "ClojureScript"),
    ("elm", "Elm"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("hrl", "Erlang"),
    ("ml", "OCaml"),
    ("mli", "OCaml"),
    ("fs", "F#"),
    ("fsx", "F#"),
    ("hs", "Haskell"),
    ("lhs", "Haskell"),
    ("dart", "Dart"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("astro", "Astro"),
];

/// Infer the programming language from a file path.
fn infer_language(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    for (extension, language) in EXTENSION_TO_LANGUAGE {
        if *extension == ext {
            return language.to_string();
        }
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    match file_name.as_str() {
        "makefile" | "gnumakefile" => "Makefile".to_string(),
        "dockerfile" => "Dockerfile".to_string(),
        "cmakelists.txt" => "CMake".to_string(),
        "cargo.toml" => "TOML".to_string(),
        "package.json" => "JSON".to_string(),
        "tsconfig.json" => "JSON".to_string(),
        _ => "Text".to_string(),
    }
}

/// Check whether a path can hold source text before its blob is read.
fn should_index_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    let skip_extensions = [
        "lock", "min.js", "min.css", "map", "gz", "zip", "tar", "png", "jpg", "jpeg", "gif", "bmp",
        "ico", "woff", "woff2", "ttf", "eot", "mp3", "mp4", "avi", "mov", "pdf", "doc", "docx",
        "xls", "xlsx", "ppt", "pptx", "exe", "dll", "so", "dylib",
    ];

    for ext in skip_extensions.iter() {
        if path_str.ends_with(&format!(".{}", ext)) {
            return false;
        }
    }

    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(name_str) = name.to_str() {
                if name_str.starts_with('.') {
                    return false;
                }
            }
        }
    }

    true
}

/// Check the one content property that the Git object header cannot answer.
fn should_index_content(content: &[u8]) -> bool {
    !content.contains(&0u8)
}

/// A code search result.
#[derive(Debug, serde::Serialize)]
pub struct CodeSearchResult {
    pub repo_id: i64,
    pub file_path: String,
    pub file_name: String,
    pub language: String,
    pub snippet: String,
}

/// What to say when a code-index write has spent its whole contention budget.
///
/// Names the wall-clock time and the number of tries, not a bare attempt count:
/// the point of the deadline is that "we waited this long" is the fact an
/// operator needs, and a count alone reads as a small number of instant
/// refusals even when it stands for half a minute of a busy database.
fn spent_budget(stage: &str, budget: &rg_db::contention::ContentionBudget) -> String {
    format!(
        "{stage} code index refresh: the database stayed contended for {:.1?} across {} attempts",
        budget.spent(),
        budget.attempts()
    )
}

/// The table that says which `code_fts` generation a repository reads.
///
/// Created by `m20261009_000901_code_index_snapshots`; its module comment
/// explains every column.
const SNAPSHOTS: &str = "code_index_snapshots";

/// The `code_fts.repo_id` value a repository's readers resolve to, as one SQL
/// expression binding the repository id twice.
///
/// A repository without a snapshot row reads the rows keyed by its own id — the
/// layout of every snapshot taken before generations existed. Readers resolve
/// the key and read the rows in ONE statement, so a publish that commits
/// between the two cannot hand a reader a key whose rows are being retired.
const PUBLISHED_KEY: &str =
    "COALESCE((SELECT s.published_key FROM code_index_snapshots s WHERE s.repo_id = ?), ?)";

/// The predicate that selects the `code_fts` rows stored under the key `key_sql`
/// evaluates to, and how many times the caller binds that key's parameters.
///
/// SQLite's `code_fts` is an FTS5 table, which has no B-tree on any column:
/// `repo_id = ?` alone reads the stored row of every file of every repository
/// on the instance to keep one repository's. `repo_id` is, however, one of the
/// table's *indexed* columns, so the rows of one key are a single token lookup
/// in the full-text index — `MATCH 'repo_id : <digits>'`. The tokenizer drops
/// the sign of a generation key and so cannot tell `41` from `-41`; the
/// equality that follows runs only over the rows that lookup returned and
/// keeps them apart (card_9ca44c148b8f).
///
/// The two `BETWEEN` ranges — every generation of a deleted repository, and
/// the abandoned generations a rebuild retires — stay scans: a range of keys is
/// no token, and both run once per deletion or rebuild, not per read or push.
fn under_key(backend: DatabaseBackend, key_sql: &str) -> (String, usize) {
    match backend {
        DatabaseBackend::Sqlite => (
            format!(
                "code_fts MATCH ('repo_id : ' || abs({key_sql})) AND code_fts.repo_id = {key_sql}"
            ),
            2,
        ),
        DatabaseBackend::Postgres | DatabaseBackend::MySql => {
            (format!("code_fts.repo_id = {key_sql}"), 1)
        }
    }
}

/// `values`, once for every time [`under_key`] spelled the key out.
fn key_values(values: &[Value], times: usize) -> Vec<Value> {
    std::iter::repeat_n(values, times)
        .flatten()
        .cloned()
        .collect()
}

/// Low bits of a generation key that carry the generation; the rest is the
/// repository id. Keys are negative so they can never be mistaken for the
/// positive repository ids the pre-generation layout used as keys.
const GENERATION_BITS: u32 = 20;
const GENERATIONS_PER_REPO: i64 = 1 << GENERATION_BITS;

/// The `code_fts.repo_id` value generation `generation` of `repo_id` writes under.
fn generation_key(repo_id: i64, generation: i64) -> Result<i64> {
    if repo_id <= 0 || !(1..GENERATIONS_PER_REPO).contains(&generation) {
        anyhow::bail!(
            "code index generation {generation} of repository {repo_id} has no key: repository \
             ids must be positive and generations within 1..{GENERATIONS_PER_REPO}"
        );
    }
    repo_id
        .checked_mul(GENERATIONS_PER_REPO)
        .and_then(|base| base.checked_add(generation))
        .map(|key| -key)
        .with_context(|| format!("repository id {repo_id} is too large for a code index key"))
}

/// Every key any generation of `repo_id` can occupy, as an inclusive range.
fn generation_key_range(repo_id: i64) -> Result<(i64, i64)> {
    Ok((
        generation_key(repo_id, GENERATIONS_PER_REPO - 1)?,
        generation_key(repo_id, 1)?,
    ))
}

/// The repository a stored `code_fts.repo_id` key belongs to.
fn repository_of_key(key: i64) -> i64 {
    if key > 0 {
        key
    } else {
        key.saturating_neg() >> GENERATION_BITS
    }
}

/// The generation after `generation`, wrapping inside `1..GENERATIONS_PER_REPO`.
fn following_generation(generation: i64) -> i64 {
    generation % (GENERATIONS_PER_REPO - 1) + 1
}

/// The column that identifies one `code_fts` row on this backend.
fn row_id_column(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Sqlite => "rowid",
        DatabaseBackend::Postgres | DatabaseBackend::MySql => "id",
    }
}

/// The byte length of a stored row's source text — what the snapshot budget
/// counts. SQLite's `length()` counts characters of a TEXT value.
fn content_bytes_expr(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Sqlite => "length(CAST(content AS BLOB))",
        // PostgreSQL answers `integer`, which does not decode as `i64`.
        DatabaseBackend::Postgres => "CAST(OCTET_LENGTH(content) AS BIGINT)",
        DatabaseBackend::MySql => "OCTET_LENGTH(content)",
    }
}

/// Code indexer service.
pub struct CodeIndexer {
    db: DatabaseConnection,
}

/// One file's row in `code_fts`, before it knows which generation it joins.
struct IndexEntry {
    file_path: String,
    file_name: String,
    content: String,
    language: String,
}

impl IndexEntry {
    fn new(path: &Path, content: String) -> Self {
        Self {
            file_path: path.to_string_lossy().to_string(),
            file_name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            language: infer_language(path),
            content,
        }
    }
}

/// One path whose row must change: `entry` is the new row, `None` removes it.
struct PathChange {
    file_path: String,
    entry: Option<IndexEntry>,
}

/// What a refresh found when it compared the branch with the published snapshot.
enum TreePlan {
    /// The published generation already describes this tree.
    Current,
    /// A small diff the published generation can absorb in one transaction.
    Changes {
        tree: String,
        changes: Vec<PathChange>,
    },
    /// The complete file set of the tree, for a new generation.
    Snapshot {
        tree: String,
        entries: Vec<IndexEntry>,
    },
}

/// How a refresh may reach the branch's tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshMode {
    /// Diff against the published tree when one is recorded and the diff is small.
    Incremental,
    /// Always traverse the whole tree into a new generation.
    Rebuild,
}

/// Ceilings one refresh works under.
#[derive(Clone, Copy, Debug)]
struct IndexLimits {
    /// Total source text a snapshot may hold.
    max_total_bytes: u64,
    /// Candidate files a traversal may meet, and rows a snapshot may hold.
    max_file_count: usize,
    /// Changed paths a push may apply in place; more becomes a rebuild.
    max_incremental_paths: usize,
    /// Changed source text a push may apply in place; more becomes a rebuild.
    max_incremental_bytes: u64,
}

/// Largest number of changed paths one push applies to the published snapshot
/// in place. A larger diff — a big merge, a vendored dependency — is rebuilt
/// into a new generation instead, whose writes are chunked.
const MAX_INCREMENTAL_PATHS: usize = 512;

/// Largest amount of changed source text one push applies in place. The apply
/// is a single transaction so readers switch atomically; this keeps that
/// transaction short.
const MAX_INCREMENTAL_BYTES: u64 = 4 * 1024 * 1024;

/// The ceilings every production refresh runs under.
fn production_limits() -> IndexLimits {
    IndexLimits {
        max_total_bytes: MAX_INDEXED_TOTAL_BYTES,
        max_file_count: MAX_INDEXED_FILE_COUNT,
        max_incremental_paths: MAX_INCREMENTAL_PATHS,
        max_incremental_bytes: MAX_INCREMENTAL_BYTES,
    }
}

/// Rows one INSERT statement carries.
const INSERT_BATCH_ROWS: usize = 100;

/// Rows one rebuild transaction writes before it commits and lets every other
/// writer of the instance in.
const BUILD_CHUNK_ROWS: usize = 400;

/// Source text one rebuild transaction writes before it commits.
const BUILD_CHUNK_BYTES: usize = 2 * 1024 * 1024;

/// Row ids one cleanup statement names, and paths one lookup names.
const ID_BATCH: usize = 500;

/// How many times one refresh re-reads the branch after it published.
///
/// A refresh resolves the branch before it writes, so a slower refresh of an
/// older push could publish after a faster refresh of a newer one. Each
/// refresh therefore looks again once it has published, and catches up with
/// an incremental round if the branch moved meanwhile. The last refresh to
/// publish is the one that sees the final tip.
const MAX_CONVERGENCE_ROUNDS: usize = 4;

/// Mutable accounting for one complete repository traversal.
struct IndexBudget {
    retained_bytes: u64,
    candidate_files: usize,
    visited_trees: usize,
    max_total_bytes: u64,
    max_file_count: usize,
}

impl IndexBudget {
    fn new(max_total_bytes: u64, max_file_count: usize) -> Self {
        Self {
            retained_bytes: 0,
            candidate_files: 0,
            visited_trees: 0,
            max_total_bytes,
            max_file_count,
        }
    }

    fn observe_candidate(&mut self, path: &Path) -> Result<()> {
        self.candidate_files = self
            .candidate_files
            .checked_add(1)
            .filter(|count| *count <= self.max_file_count)
            .ok_or_else(|| {
                crate::error::invalid_request(format!(
                    "code index contains more than {} candidate files; limit reached at '{}'",
                    self.max_file_count,
                    path.display()
                ))
            })?;
        Ok(())
    }

    /// Count one tree read. A directory that repeats a subtree under another
    /// name is walked again — its files are files at that path too — so the
    /// walk is bounded by this count rather than by remembering subtrees.
    fn observe_tree(&mut self, path: &Path) -> Result<()> {
        self.visited_trees = self
            .visited_trees
            .checked_add(1)
            .filter(|count| *count <= self.max_file_count)
            .ok_or_else(|| {
                crate::error::invalid_request(format!(
                    "code index contains more than {} directories; limit reached at '{}'",
                    self.max_file_count,
                    path.display()
                ))
            })?;
        Ok(())
    }

    fn retain(&mut self, path: &Path, bytes: usize) -> Result<()> {
        let bytes = u64::try_from(bytes).map_err(|_| {
            crate::error::invalid_request(format!(
                "code index source file '{}' has a size this platform cannot represent",
                path.display()
            ))
        })?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.max_total_bytes)
            .ok_or_else(|| {
                crate::error::invalid_request(format!(
                    "code index source files exceed the {}-byte total limit at '{}'",
                    self.max_total_bytes,
                    path.display()
                ))
            })?;
        Ok(())
    }
}

/// Reuse a valid UTF-8 blob's allocation instead of copying every source file.
fn index_content(content: Vec<u8>) -> String {
    match String::from_utf8(content) {
        Ok(content) => content,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

/// The published state of one repository's code index.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotState {
    /// A `code_index_snapshots` row exists.
    recorded: bool,
    published_key: i64,
    building_key: Option<i64>,
    indexed_tree: Option<String>,
    revision: i64,
    next_generation: i64,
    indexed_files: i64,
    indexed_bytes: i64,
}

/// Read a repository's snapshot state; `lock` takes the row for the rest of
/// the transaction on the backends that can spell it.
///
/// SQLite cannot, and does not need to: a transaction that read this row and
/// then writes is refused with `SQLITE_BUSY_SNAPSHOT` if anyone wrote in
/// between, and the retry starts from the fresh row.
async fn read_state<C>(connection: &C, repo_id: i64, lock: bool) -> Result<SnapshotState>
where
    C: ConnectionTrait,
{
    let backend = connection.get_database_backend();
    let lock_clause = if lock && backend != DatabaseBackend::Sqlite {
        " FOR UPDATE"
    } else {
        ""
    };
    let sql = rg_db::prepare_sql(
        backend,
        &format!(
            "SELECT published_key, building_key, indexed_tree, revision, next_generation, \
             indexed_files, indexed_bytes FROM {SNAPSHOTS} WHERE repo_id = ?{lock_clause}"
        ),
    );
    let row = connection
        .query_one(Statement::from_sql_and_values(
            backend,
            &sql,
            [repo_id.into()],
        ))
        .await
        .with_context(|| format!("read code index snapshot state of repository {repo_id}"))?;
    let Some(row) = row else {
        return Ok(SnapshotState {
            recorded: false,
            published_key: repo_id,
            building_key: None,
            indexed_tree: None,
            revision: 0,
            next_generation: 1,
            indexed_files: 0,
            indexed_bytes: 0,
        });
    };
    let decode = || format!("decode code index snapshot state of repository {repo_id}");
    Ok(SnapshotState {
        recorded: true,
        published_key: row.try_get_by_index(0).with_context(decode)?,
        building_key: row.try_get_by_index(1).with_context(decode)?,
        indexed_tree: row.try_get_by_index(2).with_context(decode)?,
        revision: row.try_get_by_index(3).with_context(decode)?,
        next_generation: row.try_get_by_index(4).with_context(decode)?,
        indexed_files: row.try_get_by_index(5).with_context(decode)?,
        indexed_bytes: row.try_get_by_index(6).with_context(decode)?,
    })
}

/// A stored row the refresh is about to replace or retire.
struct StoredRow {
    id: i64,
    bytes: i64,
}

/// Where a rebuild can be paused or failed by a test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildPoint {
    /// The rebuild owns its generation key and has cleared abandoned rows.
    Allocated,
    /// Chunk `n` of the new generation has committed.
    ChunkCommitted(usize),
    /// The new generation is what readers see.
    Published,
}

/// How a rebuild ended.
enum BuildOutcome {
    Published,
    /// A newer rebuild took the repository over; it publishes instead.
    Superseded,
}

/// How one refresh round ended.
enum RoundOutcome {
    /// Nothing to do: the published generation describes the branch.
    Current { files: i64 },
    /// This round changed what readers see.
    Changed { files: i64 },
    /// Someone else changed the published generation first; look again.
    Raced,
    /// A newer rebuild owns the repository and will publish.
    Superseded { files: i64 },
}

fn file_count(files: i64) -> usize {
    usize::try_from(files).unwrap_or(0)
}

impl CodeIndexer {
    /// Create a new code indexer.
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// Rebuild a repository's code index from the tree `ref_name` names.
    ///
    /// This is the explicit act — the index endpoint and `index-repo` — so it
    /// always reads the whole tree, whatever the published snapshot says. It
    /// writes the new generation in chunks under its own key and publishes it
    /// by moving one row, so readers keep the previous snapshot until the new
    /// one is complete and other writers are never queued behind the walk.
    /// Returns the number of files in the published snapshot.
    pub async fn index_repository(
        &self,
        repo_id: i64,
        repo_path: &Path,
        ref_name: &str,
    ) -> Result<usize> {
        self.index_repository_with_limits(
            repo_id,
            repo_path,
            ref_name,
            RefreshMode::Rebuild,
            production_limits(),
        )
        .await
    }

    /// Bring an existing code index up to the tree `ref_name` names.
    ///
    /// What a push does. A repository nobody has indexed is left alone and
    /// answers `None`: taking the first snapshot reads every blob of the tree
    /// and stays an explicit act. Otherwise only the paths that differ between
    /// the published tree and the new one are read and rewritten, in one short
    /// transaction; a diff too large for that, an unknown published tree, or a
    /// published tree the repository no longer has falls back to a rebuild.
    pub async fn refresh_repository(
        &self,
        repo_id: i64,
        repo_path: &Path,
        ref_name: &str,
    ) -> Result<Option<usize>> {
        let state = read_state(&self.db, repo_id, false).await?;
        if !state.recorded && self.indexed_file_count(repo_id).await? == 0 {
            return Ok(None);
        }
        self.index_repository_with_limits(
            repo_id,
            repo_path,
            ref_name,
            RefreshMode::Incremental,
            production_limits(),
        )
        .await
        .map(Some)
    }

    async fn index_repository_with_limits(
        &self,
        repo_id: i64,
        repo_path: &Path,
        ref_name: &str,
        mode: RefreshMode,
        limits: IndexLimits,
    ) -> Result<usize> {
        let mut mode = mode;
        let mut files = 0;
        for _ in 0..MAX_CONVERGENCE_ROUNDS {
            match self
                .refresh_round(repo_id, repo_path, ref_name, mode, limits)
                .await?
            {
                RoundOutcome::Current { files } | RoundOutcome::Superseded { files } => {
                    return Ok(file_count(files))
                }
                RoundOutcome::Changed { files: published } => files = published,
                RoundOutcome::Raced => {}
            }
            // Whatever this round did, the next one only has to catch up.
            mode = RefreshMode::Incremental;
        }
        // The branch kept moving faster than the refresh could follow. What is
        // published is a complete tree of it, and the next push resumes.
        tracing::debug!(
            repo_id,
            "code index refresh stopped following a branch that kept moving"
        );
        Ok(file_count(files))
    }

    /// One look at the branch and the published snapshot, and the write that
    /// brings them together.
    async fn refresh_round(
        &self,
        repo_id: i64,
        repo_path: &Path,
        ref_name: &str,
        mode: RefreshMode,
        limits: IndexLimits,
    ) -> Result<RoundOutcome> {
        let base = read_state(&self.db, repo_id, false).await?;
        let base_tree = match mode {
            RefreshMode::Incremental => base.indexed_tree.clone(),
            RefreshMode::Rebuild => None,
        };

        // The walk and every blob read run on the blocking pool: a tree of a
        // hundred thousand files is seconds of synchronous I/O that would
        // otherwise park an async worker and everything scheduled on it.
        let path = repo_path.to_path_buf();
        let reference = ref_name.to_string();
        let plan = tokio::task::spawn_blocking(move || {
            plan_refresh(&path, &reference, base_tree.as_deref(), limits)
        })
        .await
        .context("code index traversal task failed")??;

        match plan {
            TreePlan::Current => Ok(RoundOutcome::Current {
                files: base.indexed_files,
            }),
            TreePlan::Changes { tree, changes } => {
                self.apply_changes(repo_id, &base, &tree, changes, limits)
                    .await
            }
            TreePlan::Snapshot { tree, entries } => {
                let files = i64::try_from(entries.len()).unwrap_or(i64::MAX);
                match self
                    .build_snapshot(repo_id, &tree, &entries, |_| std::future::ready(Ok(())))
                    .await?
                {
                    BuildOutcome::Published => Ok(RoundOutcome::Changed { files }),
                    BuildOutcome::Superseded => Ok(RoundOutcome::Superseded { files }),
                }
            }
        }
    }

    /// How many files this repository currently has in the code index.
    ///
    /// Zero is the "no snapshot" state the AI search handler refuses a query
    /// on. It lives here rather than as a raw `SELECT COUNT(*)` at each call
    /// site because `code_fts` is this module's table, and which of its rows a
    /// repository reads is this module's decision.
    pub async fn indexed_file_count(&self, repo_id: i64) -> Result<i64> {
        let backend = self.db.get_database_backend();
        let (published, times) = under_key(backend, PUBLISHED_KEY);
        let sql = rg_db::prepare_sql(
            backend,
            &format!("SELECT COUNT(*) FROM code_fts WHERE {published}"),
        );
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                backend,
                &sql,
                key_values(&[repo_id.into(), repo_id.into()], times),
            ))
            .await
            .with_context(|| format!("count code index rows for repository {repo_id}"))?
            .with_context(|| format!("COUNT(*) returned no row for repository {repo_id}"))?;
        row.try_get_by_index(0)
            .with_context(|| format!("decode code index row count for repository {repo_id}"))
    }

    /// Remove every code-search generation of a deleted repository.
    ///
    /// Repository deletion calls this only after the metadata row has left the
    /// live set. Every write a refresh makes — an incremental apply, a rebuild
    /// chunk, a publish — first locks that row and refuses a repository that is
    /// no longer live (see [`Self::lock_repository_for_refresh`]), so nothing
    /// can recreate rows after this delete: a write that got the lock first
    /// commits before the soft-delete does, and is removed here with the rest.
    pub(crate) async fn delete_repository_index(&self, repo_id: i64) -> Result<()> {
        let (low, high) = generation_key_range(repo_id)?;
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "delete").await? else {
                continue;
            };
            let outcome = async {
                let backend = transaction.get_database_backend();
                transaction
                    .execute(Statement::from_sql_and_values(
                        backend,
                        rg_db::prepare_sql(
                            backend,
                            "DELETE FROM code_fts WHERE repo_id = ? OR repo_id BETWEEN ? AND ?",
                        ),
                        [repo_id.into(), low.into(), high.into()],
                    ))
                    .await
                    .with_context(|| format!("clear code index for repository {repo_id}"))?;
                transaction
                    .execute(Statement::from_sql_and_values(
                        backend,
                        rg_db::prepare_sql(
                            backend,
                            &format!("DELETE FROM {SNAPSHOTS} WHERE repo_id = ?"),
                        ),
                        [repo_id.into()],
                    ))
                    .await
                    .with_context(|| {
                        format!("forget code index snapshot state of repository {repo_id}")
                    })?;
                Ok(())
            }
            .await;
            if let Some(()) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "delete")
                .await?
            {
                return Ok(());
            }
        }
    }

    /// Open a write transaction, waiting out contention within `budget`.
    ///
    /// `None` means "contended, already waited — begin the attempt again".
    ///
    /// # Why the retry budget is a deadline and not an attempt count
    ///
    /// Every refresh transaction reads (`lock_repository_for_refresh`, the
    /// snapshot row) before it writes, so a competitor that commits in between
    /// makes SQLite refuse it with `SQLITE_BUSY_SNAPSHOT` — *immediately*,
    /// without consulting `busy_timeout`. A budget of thirty-two attempts is
    /// therefore thirty-two instant refusals plus their jittered waits: about
    /// a third of a second, no matter how long the writer ahead actually needs
    /// (card_0b936c68e1ea). [`rg_db::contention::ContentionBudget`] replaces
    /// the count with a deadline sized against the holder's runtime.
    async fn begin_write(
        &self,
        budget: &mut rg_db::contention::ContentionBudget,
        attempt: usize,
        stage: &str,
    ) -> Result<Option<DatabaseTransaction>> {
        match self.db.begin().await {
            Ok(transaction) => Ok(Some(transaction)),
            Err(error) if budget.may_retry() && classify(&error).is_worthwhile() => {
                classify(&error).wait(attempt).await;
                Ok(None)
            }
            Err(error) if classify(&error).is_worthwhile() => {
                Err(error).context(spent_budget(&format!("begin {stage}"), budget))
            }
            Err(error) => Err(error).context(format!("begin {stage} code index transaction")),
        }
    }

    /// Commit `transaction` after a successful body, roll it back after a
    /// failed one. `None` means "contended, already waited — run the attempt
    /// again from the start".
    async fn finish_write<T>(
        &self,
        transaction: DatabaseTransaction,
        outcome: Result<T>,
        budget: &mut rg_db::contention::ContentionBudget,
        attempt: usize,
        stage: &str,
    ) -> Result<Option<T>> {
        let value = match outcome {
            Ok(value) => value,
            Err(error) => {
                let retry = classify_anyhow(&error);
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "code index {stage} failed and its transaction could not be rolled \
                         back: {rollback_error}"
                    ));
                }
                if retry.is_worthwhile() && budget.may_retry() {
                    retry.wait(attempt).await;
                    return Ok(None);
                }
                if retry.is_worthwhile() {
                    return Err(error).context(spent_budget(stage, budget));
                }
                return Err(error);
            }
        };
        match transaction.commit().await {
            Ok(()) => Ok(Some(value)),
            Err(error) if budget.may_retry() && classify(&error).is_worthwhile() => {
                classify(&error).wait(attempt).await;
                Ok(None)
            }
            Err(error) if rg_db::is_retryable_transaction_error(&error) => {
                Err(error).context(spent_budget(&format!("commit {stage}"), budget))
            }
            Err(error) => Err(error).context(format!("commit code index {stage}")),
        }
    }

    /// Apply a small diff to the published generation in ONE transaction, so
    /// readers move from the old tree to the new one at its commit.
    ///
    /// The rows to replace are looked up before the transaction opens: on
    /// SQLite `code_fts` is an FTS5 table with no index on `repo_id`, so
    /// finding them is a scan, and a scan inside the transaction would hold the
    /// instance's only write lock for it. The transaction then deletes them by
    /// row id. `revision` is the compare-and-swap that makes the early lookup
    /// safe: if anyone changed the published generation in between, this
    /// round reports [`RoundOutcome::Raced`] and the caller looks again.
    async fn apply_changes(
        &self,
        repo_id: i64,
        base: &SnapshotState,
        tree: &str,
        changes: Vec<PathChange>,
        limits: IndexLimits,
    ) -> Result<RoundOutcome> {
        let key = base.published_key;
        let paths = changes
            .iter()
            .map(|change| change.file_path.clone())
            .collect::<Vec<_>>();
        let stored = self.stored_rows_at_paths(key, &paths).await?;

        let removed_bytes: i64 = stored.iter().map(|row| row.bytes).sum();
        let removed_files = i64::try_from(stored.len()).unwrap_or(i64::MAX);
        let entries = changes
            .into_iter()
            .filter_map(|change| change.entry)
            .collect::<Vec<_>>();
        let added_bytes = entries
            .iter()
            .map(|entry| i64::try_from(entry.content.len()).unwrap_or(i64::MAX))
            .fold(0i64, i64::saturating_add);
        let added_files = i64::try_from(entries.len()).unwrap_or(i64::MAX);
        let files = base
            .indexed_files
            .saturating_sub(removed_files)
            .saturating_add(added_files)
            .max(0);
        let bytes = base
            .indexed_bytes
            .saturating_sub(removed_bytes)
            .saturating_add(added_bytes)
            .max(0);
        // The same ceilings a full traversal enforces, against the snapshot
        // this apply would leave — not just against the diff.
        if u64::try_from(bytes).unwrap_or(u64::MAX) > limits.max_total_bytes {
            return Err(crate::error::invalid_request(format!(
                "code index source files exceed the {}-byte total limit after this push",
                limits.max_total_bytes
            )));
        }
        if file_count(files) > limits.max_file_count {
            return Err(crate::error::invalid_request(format!(
                "code index contains more than {} files after this push",
                limits.max_file_count
            )));
        }
        let ids = stored.iter().map(|row| row.id).collect::<Vec<_>>();

        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "apply").await? else {
                continue;
            };
            let outcome = async {
                self.lock_repository_for_refresh(&transaction, repo_id)
                    .await?;
                let current = read_state(&transaction, repo_id, true).await?;
                if !current.recorded
                    || current.revision != base.revision
                    || current.published_key != key
                {
                    return Ok(false);
                }
                delete_rows(&transaction, key, &ids).await?;
                insert_entries(&transaction, key, &entries).await?;
                let backend = transaction.get_database_backend();
                transaction
                    .execute(Statement::from_sql_and_values(
                        backend,
                        rg_db::prepare_sql(
                            backend,
                            &format!(
                                "UPDATE {SNAPSHOTS} SET indexed_tree = ?, indexed_files = ?, \
                                 indexed_bytes = ?, revision = revision + 1 WHERE repo_id = ?"
                            ),
                        ),
                        [tree.into(), files.into(), bytes.into(), repo_id.into()],
                    ))
                    .await
                    .with_context(|| {
                        format!("record code index tree {tree} of repository {repo_id}")
                    })?;
                Ok(true)
            }
            .await;
            match self
                .finish_write(transaction, outcome, &mut budget, attempt, "apply")
                .await?
            {
                Some(true) => return Ok(RoundOutcome::Changed { files }),
                Some(false) => return Ok(RoundOutcome::Raced),
                None => continue,
            }
        }
    }

    /// Write a complete tree as a new generation and publish it.
    ///
    /// 1. **Allocate** a key no reader resolves to, and take the repository's
    ///    `building_key` — a rebuild still writing under an older key stops at
    ///    its next chunk. Rows abandoned by earlier rebuilds (a crash, a
    ///    takeover) are retired here.
    /// 2. **Write** the rows in chunks, each its own short transaction. Between
    ///    chunks every other writer of the instance gets the database; readers
    ///    keep reading the previous generation the whole time.
    /// 3. **Publish** by moving `published_key` in one tiny transaction.
    /// 4. **Retire** the previous generation's rows in chunks.
    ///
    /// Every write of steps 1–3 locks the live repository row first, so a
    /// repository deleted mid-rebuild refuses the next chunk instead of
    /// recreating rows behind [`Self::delete_repository_index`].
    async fn build_snapshot<F, Fut>(
        &self,
        repo_id: i64,
        tree: &str,
        entries: &[IndexEntry],
        after_write: F,
    ) -> Result<BuildOutcome>
    where
        F: Fn(BuildPoint) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let key = self.allocate_generation(repo_id).await?;
        if !self.retire_abandoned_generations(repo_id, key).await? {
            return Ok(BuildOutcome::Superseded);
        }
        after_write(BuildPoint::Allocated).await?;

        for (index, chunk) in build_chunks(entries).enumerate() {
            if !self.write_chunk(repo_id, key, chunk).await? {
                return Ok(BuildOutcome::Superseded);
            }
            after_write(BuildPoint::ChunkCommitted(index)).await?;
        }

        let files = i64::try_from(entries.len()).unwrap_or(i64::MAX);
        let bytes = entries
            .iter()
            .map(|entry| i64::try_from(entry.content.len()).unwrap_or(i64::MAX))
            .fold(0i64, i64::saturating_add);
        let Some(retired) = self
            .publish_generation(repo_id, key, tree, files, bytes)
            .await?
        else {
            return Ok(BuildOutcome::Superseded);
        };
        after_write(BuildPoint::Published).await?;

        if retired != key {
            let ids = self.stored_row_ids(retired).await?;
            for batch in ids.chunks(ID_BATCH) {
                self.delete_row_batch(retired, batch).await?;
            }
        }
        Ok(BuildOutcome::Published)
    }

    /// Take a fresh generation key and make it the one allowed to write.
    async fn allocate_generation(&self, repo_id: i64) -> Result<i64> {
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "allocate").await?
            else {
                continue;
            };
            let outcome = async {
                self.lock_repository_for_refresh(&transaction, repo_id)
                    .await?;
                let state = read_state(&transaction, repo_id, true).await?;
                let backend = transaction.get_database_backend();
                if !state.recorded {
                    // Rows from before generations existed stay what readers
                    // see until this rebuild publishes.
                    transaction
                        .execute(Statement::from_sql_and_values(
                            backend,
                            rg_db::prepare_sql(
                                backend,
                                &format!(
                                    "INSERT INTO {SNAPSHOTS} (repo_id, published_key, revision, \
                                     next_generation, indexed_files, indexed_bytes) \
                                     VALUES (?, ?, 0, 1, 0, 0)"
                                ),
                            ),
                            [repo_id.into(), repo_id.into()],
                        ))
                        .await
                        .with_context(|| {
                            format!("record code index snapshot state of repository {repo_id}")
                        })?;
                }
                let mut generation = state.next_generation.clamp(1, GENERATIONS_PER_REPO - 1);
                let mut key = generation_key(repo_id, generation)?;
                // After a million rebuilds the counter wraps; never reuse the
                // key readers are reading right now.
                if key == state.published_key {
                    generation = following_generation(generation);
                    key = generation_key(repo_id, generation)?;
                }
                transaction
                    .execute(Statement::from_sql_and_values(
                        backend,
                        rg_db::prepare_sql(
                            backend,
                            &format!(
                                "UPDATE {SNAPSHOTS} SET building_key = ?, next_generation = ? \
                                 WHERE repo_id = ?"
                            ),
                        ),
                        [
                            key.into(),
                            following_generation(generation).into(),
                            repo_id.into(),
                        ],
                    ))
                    .await
                    .with_context(|| {
                        format!("claim code index generation of repository {repo_id}")
                    })?;
                Ok(key)
            }
            .await;
            if let Some(key) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "allocate")
                .await?
            {
                return Ok(key);
            }
        }
    }

    /// Delete rows that earlier rebuilds of this repository left under keys no
    /// reader resolves to. `false`: a newer rebuild took over meanwhile.
    async fn retire_abandoned_generations(&self, repo_id: i64, key: i64) -> Result<bool> {
        let (low, high) = generation_key_range(repo_id)?;
        let state = read_state(&self.db, repo_id, false).await?;
        let backend = self.db.get_database_backend();
        let sql = rg_db::prepare_sql(
            backend,
            &format!(
                "SELECT {}, repo_id FROM code_fts WHERE repo_id BETWEEN ? AND ?",
                row_id_column(backend)
            ),
        );
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                backend,
                &sql,
                [low.into(), high.into()],
            ))
            .await
            .with_context(|| format!("find abandoned code index rows of repository {repo_id}"))?;
        let mut abandoned: Vec<(i64, i64)> = Vec::new();
        for row in rows {
            let id: i64 = row.try_get_by_index(0)?;
            let row_key: i64 = row.try_get_by_index(1)?;
            if row_key != key && row_key != state.published_key {
                abandoned.push((row_key, id));
            }
        }
        abandoned.sort_unstable();
        for (row_key, ids) in group_by_key(&abandoned) {
            for batch in ids.chunks(ID_BATCH) {
                // Each batch re-checks ownership: once a newer rebuild has
                // claimed the repository, the rows it is writing are no longer
                // this rebuild's to judge.
                if !self
                    .delete_owned_batch(repo_id, key, row_key, batch)
                    .await?
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn delete_owned_batch(
        &self,
        repo_id: i64,
        owner_key: i64,
        row_key: i64,
        ids: &[i64],
    ) -> Result<bool> {
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "retire").await? else {
                continue;
            };
            let outcome = async {
                let state = read_state(&transaction, repo_id, true).await?;
                if state.building_key != Some(owner_key) {
                    return Ok(false);
                }
                delete_rows(&transaction, row_key, ids).await?;
                Ok(true)
            }
            .await;
            if let Some(owned) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "retire")
                .await?
            {
                return Ok(owned);
            }
        }
    }

    /// Write one chunk of a new generation. `false`: a newer rebuild took over.
    async fn write_chunk(&self, repo_id: i64, key: i64, chunk: &[IndexEntry]) -> Result<bool> {
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "write").await? else {
                continue;
            };
            let outcome = async {
                self.lock_repository_for_refresh(&transaction, repo_id)
                    .await?;
                let state = read_state(&transaction, repo_id, true).await?;
                if state.building_key != Some(key) {
                    return Ok(false);
                }
                insert_entries(&transaction, key, chunk).await?;
                Ok(true)
            }
            .await;
            if let Some(owned) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "write")
                .await?
            {
                return Ok(owned);
            }
        }
    }

    /// Make generation `key` the one readers see. Answers the key it replaced,
    /// or `None` when a newer rebuild took over and this one must not publish.
    async fn publish_generation(
        &self,
        repo_id: i64,
        key: i64,
        tree: &str,
        files: i64,
        bytes: i64,
    ) -> Result<Option<i64>> {
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "publish").await? else {
                continue;
            };
            let outcome = async {
                self.lock_repository_for_refresh(&transaction, repo_id)
                    .await?;
                let state = read_state(&transaction, repo_id, true).await?;
                if state.building_key != Some(key) {
                    return Ok(None);
                }
                let backend = transaction.get_database_backend();
                transaction
                    .execute(Statement::from_sql_and_values(
                        backend,
                        rg_db::prepare_sql(
                            backend,
                            &format!(
                                "UPDATE {SNAPSHOTS} SET published_key = ?, building_key = NULL, \
                                 indexed_tree = ?, indexed_files = ?, indexed_bytes = ?, \
                                 revision = revision + 1 WHERE repo_id = ?"
                            ),
                        ),
                        [
                            key.into(),
                            tree.into(),
                            files.into(),
                            bytes.into(),
                            repo_id.into(),
                        ],
                    ))
                    .await
                    .with_context(|| {
                        format!("publish code index generation of repository {repo_id}")
                    })?;
                Ok(Some(state.published_key))
            }
            .await;
            if let Some(retired) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "publish")
                .await?
            {
                return Ok(retired);
            }
        }
    }

    /// Delete one batch of a retired generation's rows. Nobody reads or writes
    /// a retired key, so this needs no ownership check.
    async fn delete_row_batch(&self, key: i64, ids: &[i64]) -> Result<()> {
        let mut budget = rg_db::contention::ContentionBudget::for_bulk_write();
        loop {
            let attempt = budget.begin_attempt();
            let Some(transaction) = self.begin_write(&mut budget, attempt, "retire").await? else {
                continue;
            };
            let outcome = delete_rows(&transaction, key, ids).await;
            if let Some(()) = self
                .finish_write(transaction, outcome, &mut budget, attempt, "retire")
                .await?
            {
                return Ok(());
            }
        }
    }

    /// The row ids stored under `key`.
    async fn stored_row_ids(&self, key: i64) -> Result<Vec<i64>> {
        let backend = self.db.get_database_backend();
        let (under, times) = under_key(backend, "?");
        let sql = rg_db::prepare_sql(
            backend,
            &format!(
                "SELECT {} FROM code_fts WHERE {under}",
                row_id_column(backend)
            ),
        );
        self.db
            .query_all(Statement::from_sql_and_values(
                backend,
                &sql,
                key_values(&[key.into()], times),
            ))
            .await
            .with_context(|| format!("list code index rows under key {key}"))?
            .into_iter()
            .map(|row| row.try_get_by_index(0).map_err(anyhow::Error::from))
            .collect()
    }

    /// The rows stored under `key` at any of `paths`.
    async fn stored_rows_at_paths(&self, key: i64, paths: &[String]) -> Result<Vec<StoredRow>> {
        let backend = self.db.get_database_backend();
        let mut stored = Vec::new();
        for batch in paths.chunks(ID_BATCH) {
            let placeholders = vec!["?"; batch.len()].join(", ");
            let (under, times) = under_key(backend, "?");
            let sql = rg_db::prepare_sql(
                backend,
                &format!(
                    "SELECT {}, {} FROM code_fts WHERE {under} AND file_path IN ({placeholders})",
                    row_id_column(backend),
                    content_bytes_expr(backend)
                ),
            );
            let mut values = key_values(&[key.into()], times);
            values.extend(batch.iter().map(|path| Value::from(path.clone())));
            let rows = self
                .db
                .query_all(Statement::from_sql_and_values(backend, &sql, values))
                .await
                .with_context(|| format!("find changed code index rows under key {key}"))?;
            for row in rows {
                stored.push(StoredRow {
                    id: row.try_get_by_index(0)?,
                    bytes: row.try_get_by_index::<Option<i64>>(1)?.unwrap_or(0),
                });
            }
        }
        Ok(stored)
    }

    /// Refuse to write for a repository that is no longer live, and serialize
    /// with its deletion on server databases without locking unrelated repos.
    ///
    /// SQLite cannot spell `FOR UPDATE`. Its live-row read followed by a write
    /// is still safe: a concurrent soft-delete between those statements makes
    /// the transaction's first write fail with `SQLITE_BUSY_SNAPSHOT`, and the
    /// retry starts from a snapshot where the repository is no longer live.
    async fn lock_repository_for_refresh(
        &self,
        transaction: &DatabaseTransaction,
        repo_id: i64,
    ) -> Result<()> {
        let backend = transaction.get_database_backend();
        let sql = if backend == DatabaseBackend::Sqlite {
            rg_db::prepare_sql(
                backend,
                "SELECT id FROM repositories WHERE id = ? AND deleted_at IS NULL",
            )
        } else {
            rg_db::prepare_sql(
                backend,
                "SELECT id FROM repositories WHERE id = ? AND deleted_at IS NULL FOR UPDATE",
            )
        };
        transaction
            .query_one(Statement::from_sql_and_values(
                backend,
                &sql,
                [repo_id.into()],
            ))
            .await
            .with_context(|| format!("lock repository {repo_id} for code index refresh"))?
            .with_context(|| {
                format!("repository {repo_id} is not live; refusing code index refresh")
            })?;
        Ok(())
    }

    /// Search code using backend-appropriate full-text search.
    ///
    /// Each repository is read through its published generation; without a
    /// repository filter, only published generations are searched.
    pub async fn search_code(
        &self,
        query: &str,
        repo_id: Option<i64>,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<CodeSearchResult>, i64)> {
        let backend = self.db.get_database_backend();
        let raw = query.trim();
        let has_query = !raw.is_empty();

        let (match_pred, order_clause, query_values) = if raw.is_empty() {
            (String::new(), String::new(), Vec::new())
        } else {
            fts_match(backend, "code_fts", CODE_FTS_COLS, raw)
        };

        let snippet_expr = code_fts_snippet_expr(backend, "code_fts", has_query);

        let (published, key_times) = under_key(backend, PUBLISHED_KEY);
        let generation_filter = match repo_id {
            Some(_) => published,
            // Generation keys are negative; a positive key is the
            // pre-generation layout, readable while no snapshot row moved it.
            None => format!(
                "(code_fts.repo_id IN (SELECT s.published_key FROM {SNAPSHOTS} s) \
                 OR (code_fts.repo_id > 0 AND code_fts.repo_id NOT IN \
                 (SELECT s.repo_id FROM {SNAPSHOTS} s)))"
            ),
        };
        let where_body = if match_pred.is_empty() {
            generation_filter
        } else {
            format!("{} AND {}", match_pred, generation_filter)
        };
        let repo_values: Vec<Value> = repo_id
            .map(|rid| key_values(&[Value::from(rid), Value::from(rid)], key_times))
            .unwrap_or_default();

        // Parameter order follows SQL order: match predicate, repo filter,
        // then the repeated ranking expression.
        let mut params: Vec<Value> = query_values
            .first()
            .cloned()
            .map(Value::from)
            .into_iter()
            .collect();
        params.extend(repo_values.iter().cloned());
        if let Some(order_value) = query_values.get(1) {
            params.push(Value::from(order_value.clone()));
        }
        let count_params: Vec<Value> = query_values
            .first()
            .cloned()
            .map(Value::from)
            .into_iter()
            .chain(repo_values)
            .collect();

        let count_sql = rg_db::prepare_sql(
            backend,
            &format!("SELECT COUNT(*) as cnt FROM code_fts WHERE {}", where_body),
        );
        let count_result = self
            .db
            .query_one(Statement::from_sql_and_values(
                backend,
                &count_sql,
                count_params,
            ))
            .await?
            .with_context(|| "Failed to get count")?;
        let total: i64 = count_result.try_get_by_index(0)?;

        let results_sql = rg_db::prepare_sql(
            backend,
            &format!(
                "SELECT code_fts.repo_id, file_path, file_name, language, {} as snippet \
             FROM code_fts \
             WHERE {} {} \
             LIMIT {} OFFSET {}",
                snippet_expr, where_body, order_clause, limit, offset
            ),
        );
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                backend,
                &results_sql,
                params,
            ))
            .await?;

        let results = rows
            .into_iter()
            .map(|row| {
                Ok(CodeSearchResult {
                    repo_id: repository_of_key(row.try_get_by_index(0)?),
                    file_path: row.try_get_by_index(1)?,
                    file_name: row.try_get_by_index(2)?,
                    language: row.try_get_by_index(3)?,
                    snippet: row.try_get_by_index(4)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok((results, total))
    }
}

/// Split `(key, id)` pairs sorted by key into one id list per key.
fn group_by_key(pairs: &[(i64, i64)]) -> Vec<(i64, Vec<i64>)> {
    let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
    for &(key, id) in pairs {
        match groups.last_mut() {
            Some((last, ids)) if *last == key => ids.push(id),
            _ => groups.push((key, vec![id])),
        }
    }
    groups
}

/// The transactions one rebuild writes: at most [`BUILD_CHUNK_ROWS`] rows and,
/// unless a single file is larger, at most [`BUILD_CHUNK_BYTES`] of text each.
fn build_chunks(entries: &[IndexEntry]) -> impl Iterator<Item = &[IndexEntry]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= entries.len() {
            return None;
        }
        let mut end = start;
        let mut bytes = 0usize;
        while end < entries.len() && end - start < BUILD_CHUNK_ROWS {
            let next = entries[end].content.len();
            if end > start && bytes.saturating_add(next) > BUILD_CHUNK_BYTES {
                break;
            }
            bytes = bytes.saturating_add(next);
            end += 1;
        }
        let chunk = &entries[start..end];
        start = end;
        Some(chunk)
    })
}

/// Delete rows by id, never touching a row stored under another key.
async fn delete_rows(transaction: &DatabaseTransaction, key: i64, ids: &[i64]) -> Result<()> {
    let backend = transaction.get_database_backend();
    for batch in ids.chunks(ID_BATCH) {
        let placeholders = vec!["?"; batch.len()].join(", ");
        let sql = rg_db::prepare_sql(
            backend,
            &format!(
                "DELETE FROM code_fts WHERE {} IN ({placeholders}) AND repo_id = ?",
                row_id_column(backend)
            ),
        );
        let mut values: Vec<Value> = batch.iter().map(|id| Value::from(*id)).collect();
        values.push(key.into());
        transaction
            .execute(Statement::from_sql_and_values(backend, &sql, values))
            .await
            .with_context(|| format!("delete code index rows under key {key}"))?;
    }
    Ok(())
}

/// Insert entries under `key` with parameterized multi-row INSERTs.
/// `rg_db::prepare_sql` converts portable bind markers to PostgreSQL's
/// numbered form before execution.
async fn insert_entries(
    transaction: &DatabaseTransaction,
    key: i64,
    entries: &[IndexEntry],
) -> Result<()> {
    let backend = transaction.get_database_backend();
    for (batch_index, chunk) in entries.chunks(INSERT_BATCH_ROWS).enumerate() {
        let mut placeholders = String::new();
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * 5);
        for (i, entry) in chunk.iter().enumerate() {
            if i > 0 {
                placeholders.push_str(", ");
            }
            placeholders.push_str("(?, ?, ?, ?, ?)");
            params.push(Value::from(key));
            params.push(Value::from(entry.file_path.clone()));
            params.push(Value::from(entry.file_name.clone()));
            params.push(Value::from(entry.content.clone()));
            params.push(Value::from(entry.language.clone()));
        }
        let sql = rg_db::prepare_sql(
            backend,
            &format!(
                "INSERT INTO code_fts(repo_id, file_path, file_name, content, language) VALUES {}",
                placeholders
            ),
        );
        transaction
            .execute(Statement::from_sql_and_values(backend, &sql, params))
            .await
            .with_context(|| {
                format!(
                    "insert code index batch {} under key {key}",
                    batch_index + 1
                )
            })?;
    }
    Ok(())
}

/// Resolve `ref_name` to the tree it names.
fn resolve_tree(repo: &gix::Repository, ref_name: &str) -> Result<gix::ObjectId> {
    let commit_id = repo
        .rev_parse_single(ref_name)
        .with_context(|| format!("Failed to resolve ref: {}", ref_name))?;
    let commit = repo
        .find_commit(commit_id)
        .with_context(|| format!("Failed to find commit: {}", commit_id))?;
    let decoded = commit
        .decode()
        .with_context(|| "Failed to decode commit".to_string())?;
    Ok(decoded.tree())
}

/// Decide what a refresh has to write. Synchronous: runs on the blocking pool.
fn plan_refresh(
    repo_path: &Path,
    ref_name: &str,
    base_tree: Option<&str>,
    limits: IndexLimits,
) -> Result<TreePlan> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("Failed to open repository: {}", repo_path.display()))?;
    let tree_oid = resolve_tree(&repo, ref_name)?;
    let tree = tree_oid.to_string();

    if let Some(base) = base_tree {
        if base == tree {
            return Ok(TreePlan::Current);
        }
        // Any trouble on this path — the published tree was rewritten away by
        // a force-push, the diff is too large to apply in one transaction —
        // only costs a rebuild, which reports a broken NEW tree with full
        // context of its own.
        match gix::ObjectId::from_hex(base.as_bytes()) {
            Ok(base_oid) => match diff_trees(&repo, base_oid, tree_oid, limits) {
                Ok(Some(changes)) => return Ok(TreePlan::Changes { tree, changes }),
                Ok(None) => tracing::debug!(
                    repo = %repo_path.display(),
                    "code index diff exceeds the in-place ceilings; rebuilding"
                ),
                Err(error) => tracing::debug!(
                    repo = %repo_path.display(),
                    error = %format!("{error:#}"),
                    "code index diff against the published tree failed; rebuilding"
                ),
            },
            Err(error) => tracing::debug!(
                repo = %repo_path.display(),
                error = %error,
                "published code index tree is not an object id; rebuilding"
            ),
        }
    }

    let tree_object = repo
        .find_tree(tree_oid)
        .with_context(|| format!("Failed to find tree: {}", tree_oid))?;
    let mut entries: Vec<IndexEntry> = Vec::new();
    let mut budget = IndexBudget::new(limits.max_total_bytes, limits.max_file_count);
    collect_tree_entries(&repo, &tree_object, tree_oid, &mut entries, &mut budget).with_context(
        || {
            format!(
                "Failed to traverse repository {} at ref '{}'",
                repo_path.display(),
                ref_name
            )
        },
    )?;
    Ok(TreePlan::Snapshot { tree, entries })
}

/// What a tree entry is to the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Tree,
    /// A regular or executable file. Links and submodules are not indexed.
    File,
}

/// The indexable entries of one tree, by name. `None` stands for "no tree".
fn tree_entries(
    repo: &gix::Repository,
    tree_oid: Option<gix::ObjectId>,
    path: &Path,
) -> Result<std::collections::BTreeMap<String, (EntryKind, gix::ObjectId)>> {
    let mut entries = std::collections::BTreeMap::new();
    let Some(tree_oid) = tree_oid else {
        return Ok(entries);
    };
    let object = repo.find_object(tree_oid).with_context(|| {
        format!(
            "Failed to read tree object {} at '{}'",
            tree_oid,
            path.display()
        )
    })?;
    let tree = object.try_into_tree().map_err(|_| {
        anyhow::anyhow!("Object {} at '{}' is not a tree", tree_oid, path.display())
    })?;
    for item in tree.iter() {
        let item = item.with_context(|| {
            format!(
                "Failed to read tree entry from object {} at '{}'",
                tree_oid,
                path.display()
            )
        })?;
        let mode = item.mode();
        let kind = if mode.is_tree() {
            EntryKind::Tree
        } else if mode.is_blob() || mode.is_executable() {
            EntryKind::File
        } else {
            continue;
        };
        let name = String::from_utf8_lossy(item.filename()).into_owned();
        entries.insert(name, (kind, item.oid().to_owned()));
    }
    Ok(entries)
}

/// A directory no file of which can ever be indexed.
fn is_hidden_directory(name: &str) -> bool {
    name.starts_with('.')
}

/// The paths whose rows differ between `old_tree` and `new_tree`, with the new
/// row of each. `None` when the diff is past the in-place ceilings.
///
/// Subtrees whose object id did not change are skipped without being read, so
/// a push touching one file reads the trees along its path and that one blob.
fn diff_trees(
    repo: &gix::Repository,
    old_tree: gix::ObjectId,
    new_tree: gix::ObjectId,
    limits: IndexLimits,
) -> Result<Option<Vec<PathChange>>> {
    let mut changes: Vec<PathChange> = Vec::new();
    let mut changed_bytes = 0u64;
    let mut visited_trees = 0usize;
    let mut stack: Vec<(Option<gix::ObjectId>, Option<gix::ObjectId>, PathBuf)> =
        vec![(Some(old_tree), Some(new_tree), PathBuf::new())];

    while let Some((old, new, path)) = stack.pop() {
        let old_entries = tree_entries(repo, old, &path)?;
        let new_entries = tree_entries(repo, new, &path)?;
        let names = old_entries
            .keys()
            .chain(new_entries.keys())
            .collect::<std::collections::BTreeSet<_>>();
        for name in names {
            let child = path.join(name);
            let before = old_entries.get(name).copied();
            let after = new_entries.get(name).copied();
            let mut old_subtree = None;
            let mut new_subtree = None;
            let mut file_changed = None;
            match (before, after) {
                (Some((EntryKind::Tree, a)), Some((EntryKind::Tree, b))) => {
                    if a != b {
                        old_subtree = Some(a);
                        new_subtree = Some(b);
                    }
                }
                (Some((EntryKind::File, a)), Some((EntryKind::File, b))) => {
                    if a != b {
                        file_changed = Some(Some(b));
                    }
                }
                _ => {
                    match before {
                        Some((EntryKind::Tree, a)) => old_subtree = Some(a),
                        Some((EntryKind::File, _)) => file_changed = Some(None),
                        None => {}
                    }
                    match after {
                        Some((EntryKind::Tree, b)) => new_subtree = Some(b),
                        Some((EntryKind::File, b)) => file_changed = Some(Some(b)),
                        None => {}
                    }
                }
            }
            if (old_subtree.is_some() || new_subtree.is_some()) && !is_hidden_directory(name) {
                visited_trees += 1;
                if visited_trees > limits.max_file_count {
                    return Ok(None);
                }
                stack.push((old_subtree, new_subtree, child.clone()));
            }
            let Some(new_blob) = file_changed else {
                continue;
            };
            if !should_index_path(&child) {
                continue;
            }
            if changes.len() >= limits.max_incremental_paths {
                return Ok(None);
            }
            let entry = match new_blob {
                Some(oid) => read_indexable_blob(repo, oid, &child)?
                    .map(|content| IndexEntry::new(&child, content)),
                None => None,
            };
            if let Some(entry) = &entry {
                changed_bytes = changed_bytes
                    .saturating_add(u64::try_from(entry.content.len()).unwrap_or(u64::MAX));
                if changed_bytes > limits.max_incremental_bytes {
                    return Ok(None);
                }
            }
            changes.push(PathChange {
                file_path: child.to_string_lossy().to_string(),
                entry,
            });
        }
    }
    Ok(Some(changes))
}

/// Collect indexable file entries by traversing the Git tree iteratively.
fn collect_tree_entries(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    tree_oid: gix::ObjectId,
    entries: &mut Vec<IndexEntry>,
    budget: &mut IndexBudget,
) -> Result<()> {
    let mut stack: Vec<(gix::ObjectId, PathBuf)> = Vec::new();
    budget.observe_tree(Path::new(""))?;
    collect_tree(
        repo,
        tree,
        tree_oid,
        PathBuf::new(),
        entries,
        &mut stack,
        budget,
    )?;
    while let Some((tree_oid, path)) = stack.pop() {
        let object = repo.find_object(tree_oid).with_context(|| {
            format!(
                "Failed to read tree object {} at '{}'",
                tree_oid,
                path.display()
            )
        })?;
        let tree = object.try_into_tree().map_err(|_| {
            anyhow::anyhow!("Object {} at '{}' is not a tree", tree_oid, path.display())
        })?;
        collect_tree(repo, &tree, tree_oid, path, entries, &mut stack, budget)?;
    }
    Ok(())
}

/// Read a committed file's text if the index keeps it: its object header is
/// checked against [`MAX_INDEXED_FILE_BYTES`] before the blob is decoded, so an
/// intentionally huge committed file costs metadata, not its full body.
fn read_indexable_blob(
    repo: &gix::Repository,
    oid: gix::ObjectId,
    path: &Path,
) -> Result<Option<String>> {
    let header = repo
        .find_header(oid)
        .with_context(|| format!("Failed to read blob header {} at '{}'", oid, path.display()))?;
    if header.kind() != gix::object::Kind::Blob {
        anyhow::bail!(
            "Object header {} at '{}' is not a blob",
            oid,
            path.display()
        );
    }
    if header.size() > MAX_INDEXED_FILE_BYTES {
        return Ok(None);
    }

    let object = repo
        .find_object(oid)
        .with_context(|| format!("Failed to read blob object {} at '{}'", oid, path.display()))?;
    let mut blob = object
        .try_into_blob()
        .map_err(|_| anyhow::anyhow!("Object {} at '{}' is not a blob", oid, path.display()))?;
    if !should_index_content(&blob.data) {
        return Ok(None);
    }
    Ok(Some(index_content(std::mem::take(&mut blob.data))))
}

/// Collect entries from a single tree into the entries Vec.
fn collect_tree(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    tree_oid: gix::ObjectId,
    base_path: PathBuf,
    entries: &mut Vec<IndexEntry>,
    stack: &mut Vec<(gix::ObjectId, PathBuf)>,
    budget: &mut IndexBudget,
) -> Result<()> {
    let tree_path = if base_path.as_os_str().is_empty() {
        "<root>".to_string()
    } else {
        base_path.display().to_string()
    };
    for item in tree.iter() {
        let item = item.with_context(|| {
            format!(
                "Failed to read tree entry from object {} at '{}'",
                tree_oid, tree_path
            )
        })?;

        let name = String::from_utf8_lossy(item.filename());
        let path = base_path.join(name.as_ref());
        let mode = item.mode();

        if mode.is_tree() {
            if !is_hidden_directory(&name) {
                // Counted when queued, not when read: a tree listing the same
                // subtree a million times must not queue a million entries.
                budget.observe_tree(&path)?;
                stack.push((item.oid().to_owned(), path));
            }
        } else if mode.is_blob() || mode.is_executable() {
            if !should_index_path(&path) {
                continue;
            }
            budget.observe_candidate(&path)?;

            if let Some(content) = read_indexable_blob(repo, item.oid().to_owned(), &path)? {
                budget.retain(&path, content.len())?;
                entries.push(IndexEntry::new(&path, content));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectOptions, Database};
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{oneshot, Notify};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    const TEST_OWNER_ID: i64 = 40;
    const TEST_REPO_ID: i64 = 41;

    async fn test_indexer() -> CodeIndexer {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1).sqlx_logging(false);
        let db = Database::connect(options)
            .await
            .expect("test database must connect");
        rg_db::run_migrations(&db)
            .await
            .expect("test database migrations must run");
        seed_repository(&db, Some(TEST_OWNER_ID), Some(TEST_REPO_ID), "unit").await;
        CodeIndexer::new(db)
    }

    async fn seed_repository(
        db: &DatabaseConnection,
        owner_id: Option<i64>,
        repo_id: Option<i64>,
        suffix: &str,
    ) -> i64 {
        let now = chrono::Utc::now();
        let mut owner = rg_db::entities::user::ActiveModel {
            username: Set(format!("index-owner-{suffix}")),
            email: Set(format!("index-owner-{suffix}@example.test")),
            password_hash: Set(String::new()),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set("local".to_string()),
            mfa_enabled: Set(false),
            login_attempts: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        if let Some(owner_id) = owner_id {
            owner.id = Set(owner_id);
        }
        let owner = owner
            .insert(db)
            .await
            .expect("insert code index fixture owner");

        let mut repository = rg_db::entities::repository::ActiveModel {
            owner_id: Set(owner.id),
            name: Set(format!("index-repository-{suffix}")),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            stars_count: Set(0),
            forks_count: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        if let Some(repo_id) = repo_id {
            repository.id = Set(repo_id);
        }
        repository
            .insert(db)
            .await
            .expect("insert code index fixture repository")
            .id
    }

    fn limits(max_total_bytes: u64, max_file_count: usize) -> IndexLimits {
        IndexLimits {
            max_total_bytes,
            max_file_count,
            ..production_limits()
        }
    }

    fn generation_entries(generation: &str, count: usize) -> Vec<IndexEntry> {
        (0..count)
            .map(|index| IndexEntry {
                file_path: format!("{generation}/{index:04}.rs"),
                file_name: format!("{index:04}.rs"),
                content: format!("fn {generation}_{index}() {{}}"),
                language: "Rust".to_string(),
            })
            .collect()
    }

    fn entry_paths(entries: &[IndexEntry]) -> Vec<String> {
        let mut paths = entries
            .iter()
            .map(|entry| entry.file_path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    async fn indexed_paths(indexer: &CodeIndexer, repo_id: i64) -> Vec<String> {
        let (rows, total) = indexer
            .search_code("", Some(repo_id), 10_000, 0)
            .await
            .expect("read code index snapshot");
        assert_eq!(
            usize::try_from(total).expect("non-negative code index count"),
            rows.len(),
            "the count and rows must describe the same quiescent snapshot"
        );
        assert!(
            rows.iter().all(|row| row.repo_id == repo_id),
            "a reader saw a generation key instead of its repository id"
        );
        let mut paths = rows
            .into_iter()
            .map(|row| row.file_path)
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    /// Every stored row of `repo_id` in any generation, as `(key, row id, path)`
    /// ordered by path — what the database holds, not what readers see.
    async fn stored_rows(db: &DatabaseConnection, repo_id: i64) -> Vec<(i64, i64, String)> {
        let backend = db.get_database_backend();
        let (low, high) = generation_key_range(repo_id).expect("fixture repository has keys");
        let sql = rg_db::prepare_sql(
            backend,
            &format!(
                "SELECT repo_id, {}, file_path FROM code_fts \
                 WHERE repo_id = ? OR repo_id BETWEEN ? AND ? ORDER BY file_path",
                row_id_column(backend)
            ),
        );
        db.query_all(Statement::from_sql_and_values(
            backend,
            &sql,
            [repo_id.into(), low.into(), high.into()],
        ))
        .await
        .expect("read stored code index rows")
        .into_iter()
        .map(|row| {
            (
                row.try_get_by_index(0).expect("key"),
                row.try_get_by_index(1).expect("row id"),
                row.try_get_by_index(2).expect("path"),
            )
        })
        .collect()
    }

    async fn state(indexer: &CodeIndexer, repo_id: i64) -> SnapshotState {
        read_state(&indexer.db, repo_id, false)
            .await
            .expect("read snapshot state")
    }

    async fn build(indexer: &CodeIndexer, repo_id: i64, tree: &str, entries: &[IndexEntry]) {
        let outcome = indexer
            .build_snapshot(repo_id, tree, entries, |_| std::future::ready(Ok(())))
            .await
            .expect("build a code index generation");
        assert!(matches!(outcome, BuildOutcome::Published));
    }

    async fn build_paused_after_first_chunk(
        db: DatabaseConnection,
        repo_id: i64,
        entries: Vec<IndexEntry>,
        reached_tx: oneshot::Sender<()>,
        release: Arc<Notify>,
    ) -> Result<BuildOutcome> {
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        CodeIndexer::new(db)
            .build_snapshot(repo_id, "paused", &entries, move |point| {
                let reached_tx = reached_tx.clone();
                let release = release.clone();
                async move {
                    if point == BuildPoint::ChunkCommitted(0) {
                        let sender = reached_tx.lock().expect("lock rebuild pause sender").take();
                        if let Some(sender) = sender {
                            sender.send(()).expect("announce paused code index rebuild");
                            release.notified().await;
                        }
                    }
                    Ok(())
                }
            })
            .await
    }

    /// The card's property, on whichever backend `db` is: a rebuild publishes
    /// atomically for readers WITHOUT keeping other writers out while it works.
    async fn exercise_generation_rebuild(db: &DatabaseConnection, repo_id: i64) {
        let indexer = CodeIndexer::new(db.clone());
        let old_entries = generation_entries("old", 2);
        let old_paths = entry_paths(&old_entries);
        build(&indexer, repo_id, "old", &old_entries).await;

        // Three chunks, so a failure can land between any two of them.
        let replacement = generation_entries("replacement", BUILD_CHUNK_ROWS * 2 + 5);
        assert_eq!(build_chunks(&replacement).count(), 3);
        for fail_at in [
            BuildPoint::Allocated,
            BuildPoint::ChunkCommitted(0),
            BuildPoint::ChunkCommitted(1),
            BuildPoint::ChunkCommitted(2),
        ] {
            let error = indexer
                .build_snapshot(
                    repo_id,
                    "replacement",
                    &replacement,
                    move |point| async move {
                        if point == fail_at {
                            anyhow::bail!("injected code index failure after {point:?}");
                        }
                        Ok(())
                    },
                )
                .await
                .err()
                .expect("an injected rebuild failure must escape");
            assert!(format!("{error:#}").contains("injected code index failure"));
            assert_eq!(
                indexed_paths(&indexer, repo_id).await,
                old_paths,
                "{fail_at:?} published a partial replacement"
            );
        }

        // Pause a rebuild between two committed chunks. Readers keep the old
        // generation, and — the point of chunking — another writer of the
        // instance is not queued behind the rebuild.
        let paused_entries = generation_entries("paused", BUILD_CHUNK_ROWS * 2);
        let paused_paths = entry_paths(&paused_entries);
        let (reached_tx, reached_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let paused = tokio::spawn(build_paused_after_first_chunk(
            db.clone(),
            repo_id,
            paused_entries,
            reached_tx,
            release.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("paused rebuild did not reach its first chunk")
            .expect("paused rebuild dropped its pause signal");

        let observed =
            tokio::time::timeout(Duration::from_secs(2), indexed_paths(&indexer, repo_id))
                .await
                .expect("a reader waited for a rebuild that is between chunks");
        assert_eq!(
            observed, old_paths,
            "a reader observed an unpublished chunk"
        );

        let backend = db.get_database_backend();
        tokio::time::timeout(
            Duration::from_secs(2),
            db.execute(Statement::from_sql_and_values(
                backend,
                rg_db::prepare_sql(
                    backend,
                    "UPDATE repositories SET updated_at = updated_at WHERE id = ?",
                ),
                [repo_id.into()],
            )),
        )
        .await
        .expect("an unrelated writer was queued behind a rebuild that is between chunks")
        .expect("the unrelated write failed");

        release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(10), paused)
            .await
            .expect("paused rebuild stayed blocked")
            .expect("paused rebuild task panicked")
            .expect("paused rebuild failed");
        assert!(matches!(outcome, BuildOutcome::Published));
        assert_eq!(indexed_paths(&indexer, repo_id).await, paused_paths);

        // Hold one rebuild between chunks and let a second one run start to
        // finish. The second takes the repository over; the first must not
        // publish once released, and none of its rows may survive.
        let first_entries = generation_entries("first", BUILD_CHUNK_ROWS * 2);
        let second_entries = generation_entries("second", 3);
        let second_paths = entry_paths(&second_entries);
        let (reached_tx, reached_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let first = tokio::spawn(build_paused_after_first_chunk(
            db.clone(),
            repo_id,
            first_entries,
            reached_tx,
            release.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("first rebuild did not reach its first chunk")
            .expect("first rebuild dropped its pause signal");

        tokio::time::timeout(
            Duration::from_secs(10),
            build(&indexer, repo_id, "second", &second_entries),
        )
        .await
        .expect("a rebuild waited for an older rebuild that is between chunks");

        release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(10), first)
            .await
            .expect("first rebuild stayed blocked")
            .expect("first rebuild task panicked")
            .expect("first rebuild failed");
        assert!(
            matches!(outcome, BuildOutcome::Superseded),
            "a rebuild that was taken over still reported publishing"
        );
        assert_eq!(
            indexed_paths(&indexer, repo_id).await,
            second_paths,
            "a superseded rebuild published over the newer one"
        );
        // The same takeover after the older rebuild's LAST chunk: nothing is
        // left to write, so only the publish can notice it lost the repository.
        let (reached_tx, reached_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let last = tokio::spawn(build_paused_after_first_chunk(
            db.clone(),
            repo_id,
            generation_entries("single-chunk", 3),
            reached_tx,
            release.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("single-chunk rebuild did not reach its only chunk")
            .expect("single-chunk rebuild dropped its pause signal");
        build(&indexer, repo_id, "second", &second_entries).await;
        release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(10), last)
            .await
            .expect("single-chunk rebuild stayed blocked")
            .expect("single-chunk rebuild task panicked")
            .expect("single-chunk rebuild failed");
        assert!(
            matches!(outcome, BuildOutcome::Superseded),
            "a rebuild taken over after its last chunk still published"
        );
        assert_eq!(indexed_paths(&indexer, repo_id).await, second_paths);

        let stored = stored_rows(db, repo_id).await;
        assert_eq!(
            stored.iter().map(|row| row.2.clone()).collect::<Vec<_>>(),
            second_paths,
            "rows of failed, superseded or retired generations survived: {stored:?}"
        );
    }

    async fn exercise_deleted_repository_rejects_refresh(indexer: &CodeIndexer, repo_id: i64) {
        build(
            indexer,
            repo_id,
            "before-delete",
            &generation_entries("old", 2),
        )
        .await;

        let retirement = rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
            &indexer.db,
            repo_id,
            chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
        )
        .await
        .expect("soft-delete the repository");
        assert!(matches!(
            retirement,
            rg_db::ops::repo_ops::RepositoryRetirement::Deleted
        ));
        indexer
            .delete_repository_index(repo_id)
            .await
            .expect("clear the retired repository's code index");

        let error = indexer
            .build_snapshot(repo_id, "late", &generation_entries("late", 1), |_| {
                std::future::ready(Ok(()))
            })
            .await
            .err()
            .expect("a late rebuild must not recreate a deleted repository's index");
        assert!(
            format!("{error:#}").contains("is not live; refusing code index refresh"),
            "unexpected late-refresh error: {error:#}"
        );
        assert_eq!(
            indexer
                .indexed_file_count(repo_id)
                .await
                .expect("count the retired repository's code index"),
            0,
            "the late refresh resurrected a deleted repository's source snapshot"
        );
        assert!(
            stored_rows(&indexer.db, repo_id).await.is_empty(),
            "a deleted repository kept rows in some generation"
        );
        assert!(!state(indexer, repo_id).await.recorded);
    }

    fn committed_repository(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temporary directory must be created");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree directory must be created");
        run_git(&worktree, &["init", "-q", "-b", "main"]);
        run_git(
            &worktree,
            &["config", "user.name", "Plombir Git Index Test"],
        );
        run_git(&worktree, &["config", "user.email", "indexer@example.test"]);
        for (path, content) in files {
            let file = worktree.join(path);
            std::fs::create_dir_all(file.parent().expect("fixture file must have a parent"))
                .expect("fixture parent directory must be created");
            std::fs::write(file, content).expect("fixture file must be written");
        }
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-q", "-m", "index fixture"]);
        (dir, worktree)
    }

    fn run_git(worktree: &Path, args: &[&str]) -> String {
        let output = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize")
            .run(args, Some(worktree))
            .expect("git must run");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
        output.stdout_str().trim().to_string()
    }

    fn loose_object_path(repo_path: &Path, oid: &str) -> PathBuf {
        repo_path.join("objects").join(&oid[..2]).join(&oid[2..])
    }

    fn overwrite_loose_object(repo_path: &Path, oid: &str, kind: &str, data: &[u8]) {
        let object_path = loose_object_path(repo_path, oid);
        std::fs::remove_file(&object_path).expect("existing loose object must be removable");
        let file = std::fs::File::create(object_path).expect("loose object must be writable");
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        write!(encoder, "{kind} {}\0", data.len()).expect("object header must compress");
        encoder
            .write_all(data)
            .expect("object payload must compress");
        encoder.finish().expect("object must finish compressing");
    }

    #[tokio::test]
    async fn a_soft_deleted_repository_cannot_publish_a_late_index_refresh() {
        let indexer = test_indexer().await;
        exercise_deleted_repository_rejects_refresh(&indexer, TEST_REPO_ID).await;
    }

    fn commit_all(worktree: &Path, message: &str) -> String {
        run_git(worktree, &["add", "-A"]);
        run_git(worktree, &["commit", "-q", "-m", message]);
        run_git(worktree, &["rev-parse", "HEAD^{tree}"])
    }

    /// Forty source files in four directories.
    fn forty_files() -> Vec<(String, Vec<u8>)> {
        (0..40)
            .map(|index| {
                (
                    format!("dir{}/file{index:02}.rs", index % 4),
                    format!("fn original_{index}() {{}}\n").into_bytes(),
                )
            })
            .collect()
    }

    fn committed_files(files: &[(String, Vec<u8>)]) -> (tempfile::TempDir, PathBuf) {
        let borrowed = files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_slice()))
            .collect::<Vec<_>>();
        committed_repository(&borrowed)
    }

    async fn search_paths(indexer: &CodeIndexer, query: &str, repo_id: Option<i64>) -> Vec<String> {
        let (rows, total) = indexer
            .search_code(query, repo_id, 100, 0)
            .await
            .expect("search the code index");
        let mut paths = rows
            .into_iter()
            .map(|row| row.file_path)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            total,
            i64::try_from(paths.len()).unwrap(),
            "count and page disagree"
        );
        paths
    }

    /// card_9ca44c148b8f: `repo_id` is an indexed column of the SQLite FTS5
    /// table, and an unfiltered `MATCH` matched it — a search for `41` inside
    /// repository 41 returned every file it has, and a search for the digits of
    /// a generation key returned that whole generation.
    #[tokio::test]
    async fn a_search_matches_file_text_and_never_the_repository_key() {
        let indexer = test_indexer().await;
        // The layout from before generations: rows keyed by the repository id.
        for (path, content) in [
            ("forty_one.rs", "const ANSWER: u32 = 41;"),
            ("other.rs", "fn main() {}"),
        ] {
            indexer
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "INSERT INTO code_fts(repo_id, file_path, file_name, content, language) \
                     VALUES (?, ?, ?, ?, 'Rust')",
                    [
                        TEST_REPO_ID.into(),
                        path.into(),
                        path.into(),
                        content.into(),
                    ],
                ))
                .await
                .expect("seed a pre-generation row");
        }
        assert_eq!(
            search_paths(&indexer, &TEST_REPO_ID.to_string(), Some(TEST_REPO_ID)).await,
            ["forty_one.rs"]
        );
        assert_eq!(
            search_paths(&indexer, &TEST_REPO_ID.to_string(), None).await,
            ["forty_one.rs"]
        );

        build(
            &indexer,
            TEST_REPO_ID,
            "tree-one",
            &generation_entries("one", 3),
        )
        .await;
        let key = state(&indexer, TEST_REPO_ID).await.published_key;
        assert!(key < 0, "the build did not publish a generation key");
        let digits = key.unsigned_abs().to_string();
        assert_eq!(
            search_paths(&indexer, &digits, Some(TEST_REPO_ID)).await,
            Vec::<String>::new()
        );
        assert_eq!(
            search_paths(&indexer, &digits, None).await,
            Vec::<String>::new()
        );
        // The file text is still what a query finds.
        assert_eq!(
            search_paths(&indexer, "one_1", Some(TEST_REPO_ID)).await,
            ["one/0001.rs"]
        );
    }

    /// The other half of card_9ca44c148b8f. FTS5 has no B-tree on a column, so
    /// `WHERE repo_id = ?` on SQLite read every stored file of every repository
    /// to keep one repository's. Every statement a read or an incremental push
    /// sends to `code_fts` has to find its rows through the full-text index —
    /// an FTS5 plan whose index string is empty is that full scan.
    #[tokio::test]
    async fn per_repository_reads_find_their_rows_through_the_full_text_index() {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1).sqlx_logging(false);
        let mut db = Database::connect(options).await.expect("connect");
        rg_db::run_migrations(&db).await.expect("migrate");
        seed_repository(&db, Some(TEST_OWNER_ID), Some(TEST_REPO_ID), "plan").await;
        let other = seed_repository(&db, None, None, "plan-other").await;

        type Sent = Vec<(String, Vec<Value>)>;
        let statements: Arc<Mutex<Sent>> = Arc::default();
        let recorder = Arc::clone(&statements);
        db.set_metric_callback(move |info: &sea_orm::metric::Info<'_>| {
            let values = info
                .statement
                .values
                .as_ref()
                .map(|values| values.0.clone())
                .unwrap_or_default();
            recorder
                .lock()
                .unwrap()
                .push((info.statement.sql.clone(), values));
        });
        let indexer = CodeIndexer::new(db.clone());
        build(
            &indexer,
            TEST_REPO_ID,
            "tree-one",
            &generation_entries("one", 3),
        )
        .await;
        build(
            &indexer,
            other,
            "tree-other",
            &generation_entries("other", 3),
        )
        .await;
        let key = state(&indexer, TEST_REPO_ID).await.published_key;

        statements.lock().unwrap().clear();
        assert_eq!(indexer.indexed_file_count(TEST_REPO_ID).await.unwrap(), 3);
        assert_eq!(
            search_paths(&indexer, "one_1", Some(TEST_REPO_ID)).await,
            ["one/0001.rs"]
        );
        assert_eq!(
            search_paths(&indexer, "", Some(TEST_REPO_ID)).await.len(),
            3
        );
        assert_eq!(indexer.stored_row_ids(key).await.unwrap().len(), 3);
        assert_eq!(
            indexer
                .stored_rows_at_paths(key, &["one/0000.rs".to_string()])
                .await
                .unwrap()
                .len(),
            1
        );
        let sent: Sent = statements
            .lock()
            .unwrap()
            .drain(..)
            .filter(|(sql, _)| sql.contains("code_fts"))
            .collect();
        assert!(
            sent.len() >= 6,
            "the reads sent no code_fts statement: {sent:?}"
        );

        for (sql, values) in sent {
            let plan = db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    format!("EXPLAIN QUERY PLAN {sql}"),
                    values,
                ))
                .await
                .expect("explain a code_fts statement");
            let details: Vec<String> = plan
                .iter()
                .map(|row| row.try_get_by_index::<String>(3).expect("plan detail"))
                .collect();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("VIRTUAL TABLE INDEX")),
                "no code_fts step in the plan of {sql}: {details:?}"
            );
            assert!(
                !details
                    .iter()
                    .any(|detail| detail.ends_with("VIRTUAL TABLE INDEX 0:")),
                "{sql} scans every repository's rows: {details:?}"
            );
        }
    }

    /// The card's acceptance: a push that changes one file of many rewrites
    /// that file's row and nothing else, and a deleted path leaves the index.
    #[tokio::test]
    async fn a_push_rewrites_only_the_paths_it_changed() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        assert_eq!(
            indexer
                .index_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("take the first snapshot"),
            40
        );
        let before = stored_rows(&indexer.db, TEST_REPO_ID).await;

        std::fs::write(worktree.join("dir1/file05.rs"), b"fn zzchanged() {}\n")
            .expect("change one file");
        std::fs::remove_file(worktree.join("dir2/file06.rs")).expect("delete one file");
        std::fs::write(worktree.join("dir3/added.rs"), b"fn zzadded() {}\n").expect("add one file");
        let tree = commit_all(&worktree, "one of each");

        assert_eq!(
            indexer
                .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("refresh after the push"),
            Some(40)
        );
        let after = stored_rows(&indexer.db, TEST_REPO_ID).await;
        let untouched = |rows: &[(i64, i64, String)]| {
            rows.iter()
                .filter(|row| {
                    !["dir1/file05.rs", "dir2/file06.rs", "dir3/added.rs"].contains(&row.2.as_str())
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(untouched(&before).len(), 38);
        assert_eq!(
            untouched(&after),
            untouched(&before),
            "rows of files the push did not touch were rewritten"
        );
        let paths = indexed_paths(&indexer, TEST_REPO_ID).await;
        assert!(!paths.contains(&"dir2/file06.rs".to_string()));
        assert!(paths.contains(&"dir3/added.rs".to_string()));
        let (hits, _) = indexer
            .search_code("zzchanged", Some(TEST_REPO_ID), 10, 0)
            .await
            .expect("search the refreshed file");
        assert_eq!(
            hits.iter()
                .map(|hit| hit.file_path.as_str())
                .collect::<Vec<_>>(),
            ["dir1/file05.rs"]
        );
        let state = state(&indexer, TEST_REPO_ID).await;
        assert_eq!(state.indexed_tree.as_deref(), Some(tree.as_str()));
        assert_eq!(state.indexed_files, 40);
    }

    #[tokio::test]
    async fn a_refresh_of_an_unchanged_tree_writes_nothing() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("take the first snapshot");
        let before = state(&indexer, TEST_REPO_ID).await;

        assert_eq!(
            indexer
                .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("refresh an unchanged tree"),
            Some(40)
        );
        assert_eq!(state(&indexer, TEST_REPO_ID).await, before);
    }

    /// The rows an incremental apply deletes were looked up before its
    /// transaction. If anyone changed the published generation in between,
    /// those row ids are stale, and the apply must step back rather than write.
    #[tokio::test]
    async fn an_apply_against_a_moved_snapshot_steps_back() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("take the first snapshot");
        let stale = state(&indexer, TEST_REPO_ID).await;

        std::fs::write(worktree.join("dir0/file00.rs"), b"fn zzmoved() {}\n").expect("edit");
        commit_all(&worktree, "moved");
        indexer
            .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("move the published snapshot");
        let moved = state(&indexer, TEST_REPO_ID).await;

        let outcome = indexer
            .apply_changes(
                TEST_REPO_ID,
                &stale,
                "late",
                vec![PathChange {
                    file_path: "late.rs".to_string(),
                    entry: Some(IndexEntry::new(Path::new("late.rs"), "fn late() {}".into())),
                }],
                production_limits(),
            )
            .await
            .expect("a raced apply is not an error");
        assert!(matches!(outcome, RoundOutcome::Raced));
        assert_eq!(state(&indexer, TEST_REPO_ID).await, moved);
        assert!(!indexed_paths(&indexer, TEST_REPO_ID)
            .await
            .contains(&"late.rs".to_string()));
    }

    /// Past the in-place ceiling the refresh rebuilds — every row is new — and
    /// the result is still exactly the tree.
    #[tokio::test]
    async fn a_diff_past_the_in_place_ceiling_is_rebuilt() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("take the first snapshot");
        let before = stored_rows(&indexer.db, TEST_REPO_ID).await;

        std::fs::write(worktree.join("dir0/file00.rs"), b"fn zzone() {}\n").expect("change");
        std::fs::write(worktree.join("dir1/file01.rs"), b"fn zztwo() {}\n").expect("change");
        commit_all(&worktree, "two changes");

        let tight = IndexLimits {
            max_incremental_paths: 1,
            ..production_limits()
        };
        indexer
            .index_repository_with_limits(
                TEST_REPO_ID,
                &worktree,
                "HEAD",
                RefreshMode::Incremental,
                tight,
            )
            .await
            .expect("refresh past the in-place ceiling");
        let after = stored_rows(&indexer.db, TEST_REPO_ID).await;
        assert_eq!(after.len(), 40);
        assert!(
            after.iter().all(|row| row.0 != before[0].0),
            "a diff past the ceiling was applied in place"
        );
        let (hits, _) = indexer
            .search_code("zztwo", Some(TEST_REPO_ID), 10, 0)
            .await
            .expect("search the rebuilt snapshot");
        assert_eq!(hits.len(), 1);
    }

    /// A force-push can take the published tree out of the repository. The
    /// refresh then rebuilds instead of failing or diffing against nothing.
    #[tokio::test]
    async fn a_published_tree_the_repository_lost_is_rebuilt() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("take the first snapshot");
        indexer
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "UPDATE code_index_snapshots SET indexed_tree = ? WHERE repo_id = ?",
                [
                    "0123456789012345678901234567890123456789".into(),
                    TEST_REPO_ID.into(),
                ],
            ))
            .await
            .expect("point the snapshot at a tree the repository does not have");
        std::fs::write(worktree.join("dir0/file00.rs"), b"fn zzrewritten() {}\n").expect("edit");
        let tree = commit_all(&worktree, "rewritten");

        assert_eq!(
            indexer
                .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("refresh against a lost tree"),
            Some(40)
        );
        assert_eq!(indexed_paths(&indexer, TEST_REPO_ID).await.len(), 40);
        assert_eq!(
            state(&indexer, TEST_REPO_ID).await.indexed_tree.as_deref(),
            Some(tree.as_str())
        );
    }

    /// Git stores two identical directories as one tree object. Both are still
    /// directories of the repository, on a rebuild and on a push alike.
    #[tokio::test]
    async fn a_repeated_directory_is_indexed_at_every_path() {
        let (_dir, worktree) = committed_repository(&[
            ("left/same.rs", b"fn zzsame() {}\n"),
            ("right/same.rs", b"fn zzsame() {}\n"),
        ]);
        let indexer = test_indexer().await;
        assert_eq!(
            indexer
                .index_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("index two identical directories"),
            2
        );

        std::fs::create_dir_all(worktree.join("third")).expect("third directory");
        std::fs::write(worktree.join("third/same.rs"), b"fn zzsame() {}\n").expect("third copy");
        commit_all(&worktree, "third copy");
        indexer
            .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("refresh with a third copy");
        assert_eq!(
            indexed_paths(&indexer, TEST_REPO_ID).await,
            ["left/same.rs", "right/same.rs", "third/same.rs"]
        );
    }

    #[tokio::test]
    async fn a_repository_nobody_indexed_is_left_alone() {
        let (_dir, worktree) = committed_files(&forty_files());
        let indexer = test_indexer().await;
        assert_eq!(
            indexer
                .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("refresh an unindexed repository"),
            None
        );
        assert!(!state(&indexer, TEST_REPO_ID).await.recorded);
        assert!(stored_rows(&indexer.db, TEST_REPO_ID).await.is_empty());
    }

    /// Rows written before generations existed are keyed by the repository id
    /// and have no state row. They stay readable, a push replaces them with a
    /// generation, and none of them survives that.
    #[tokio::test]
    async fn a_snapshot_from_before_generations_is_replaced_on_the_next_push() {
        let (_dir, worktree) = committed_repository(&[("kept.rs", b"fn zzkept() {}\n")]);
        let indexer = test_indexer().await;
        indexer
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "INSERT INTO code_fts(repo_id, file_path, file_name, content, language) \
                 VALUES (?, 'legacy.rs', 'legacy.rs', 'fn zzlegacy() {}', 'Rust')",
                [TEST_REPO_ID.into()],
            ))
            .await
            .expect("seed a pre-generation row");
        assert_eq!(indexed_paths(&indexer, TEST_REPO_ID).await, ["legacy.rs"]);

        assert_eq!(
            indexer
                .refresh_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("refresh a pre-generation snapshot"),
            Some(1)
        );
        assert_eq!(indexed_paths(&indexer, TEST_REPO_ID).await, ["kept.rs"]);
        let stored = stored_rows(&indexer.db, TEST_REPO_ID).await;
        assert_eq!(
            stored.len(),
            1,
            "the pre-generation row survived: {stored:?}"
        );
        assert!(stored[0].0 < 0, "the new row is not under a generation key");
    }

    /// An incremental push is held to the same snapshot ceiling a rebuild is,
    /// measured on the snapshot it would leave behind.
    #[tokio::test]
    async fn a_push_that_grows_the_snapshot_past_its_ceiling_is_refused() {
        let (_dir, worktree) = committed_repository(&[("small.rs", b"fn a() {}\n")]);
        let indexer = test_indexer().await;
        indexer
            .index_repository_with_limits(
                TEST_REPO_ID,
                &worktree,
                "HEAD",
                RefreshMode::Rebuild,
                limits(40, MAX_INDEXED_FILE_COUNT),
            )
            .await
            .expect("take a snapshot under the ceiling");
        std::fs::write(
            worktree.join("grown.rs"),
            b"fn grown_past_the_ceiling() {}\n",
        )
        .expect("grow the tree");
        commit_all(&worktree, "grown");

        let error = indexer
            .index_repository_with_limits(
                TEST_REPO_ID,
                &worktree,
                "HEAD",
                RefreshMode::Incremental,
                limits(40, MAX_INDEXED_FILE_COUNT),
            )
            .await
            .expect_err("an incremental push past the ceiling must fail");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "{error:#}"
        );
        assert!(
            format!("{error:#}").contains("40-byte total limit"),
            "{error:#}"
        );
        assert_eq!(indexed_paths(&indexer, TEST_REPO_ID).await, ["small.rs"]);
    }

    #[tokio::test]
    async fn healthy_repository_indexes_the_complete_file_set() {
        let oversized = vec![b'x'; MAX_INDEXED_FILE_BYTES as usize + 1];
        let (_dir, worktree) = committed_repository(&[
            ("README.md", b"searchable readme\n"),
            ("src/main.rs", b"fn main() {}\n"),
            ("assets/image.png", b"binary\0payload"),
            ("src/generated.rs", &oversized),
        ]);
        let indexer = test_indexer().await;

        let count = indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("healthy repository must index");
        assert_eq!(count, 2, "binary and oversized files must be excluded");

        let (results, total) = indexer
            .search_code("", Some(TEST_REPO_ID), 10, 0)
            .await
            .expect("indexed files must be searchable");
        let mut paths = results
            .into_iter()
            .map(|result| result.file_path)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(total, 2);
        assert_eq!(paths, ["README.md", "src/main.rs"]);
    }

    #[tokio::test]
    async fn aggregate_limit_fails_refresh_and_preserves_the_previous_index() {
        let (_dir, worktree) = committed_repository(&[("old.rs", b"fn old() {}\n")]);
        let indexer = test_indexer().await;
        indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("seed the previous complete code index");

        std::fs::remove_file(worktree.join("old.rs")).expect("remove the old fixture source");
        std::fs::write(worktree.join("first.rs"), b"fn first() {}\n")
            .expect("write first replacement source");
        std::fs::write(worktree.join("second.rs"), b"fn second() {}\n")
            .expect("write second replacement source");
        run_git(&worktree, &["add", "-A"]);
        run_git(&worktree, &["commit", "-q", "-m", "replacement"]);

        let max_total_bytes = 20;
        let error = indexer
            .index_repository_with_limits(
                TEST_REPO_ID,
                &worktree,
                "HEAD",
                RefreshMode::Rebuild,
                limits(max_total_bytes, MAX_INDEXED_FILE_COUNT),
            )
            .await
            .expect_err("a complete source snapshot over its total budget must fail");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "the repository owner must receive a client-correctable refusal: {error:#}"
        );
        assert!(
            format!("{error:#}").contains(&format!("{max_total_bytes}-byte total limit")),
            "unexpected aggregate-limit error: {error:#}"
        );
        assert_eq!(
            indexed_paths(&indexer, TEST_REPO_ID).await,
            ["old.rs"],
            "a rejected traversal replaced the previous complete index"
        );
    }

    #[tokio::test]
    async fn aggregate_limit_counts_the_text_retained_after_lossy_decoding() {
        let (_dir, worktree) = committed_repository(&[("invalid.rs", &[0xff])]);
        let error = test_indexer()
            .await
            .index_repository_with_limits(
                TEST_REPO_ID,
                &worktree,
                "HEAD",
                RefreshMode::Rebuild,
                limits(2, MAX_INDEXED_FILE_COUNT),
            )
            .await
            .expect_err("one invalid byte expands to a three-byte replacement character");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "lossy UTF-8 expansion must spend the same client-correctable budget: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("2-byte total limit"),
            "{error:#}"
        );
    }

    /// A valid oversized blob is skipped either way, so an ordinary behavior
    /// test cannot distinguish the safe order from the old read-then-check
    /// order. Assert that order at the source boundary where it matters — and
    /// that every blob the index reads, on a rebuild or on a push, goes
    /// through that one boundary.
    #[test]
    fn the_file_ceiling_is_spent_before_the_blob_is_read() {
        let code = rust_source::production_rust_code_only(include_str!("code_indexer.rs"));
        let start = code
            .find("fn read_indexable_blob(")
            .expect("`read_indexable_blob` must remain the blob-reading boundary");
        let body = &code[start..];
        let end = body
            .find("\nfn collect_tree(")
            .expect("`read_indexable_blob` must end before the tree collector");
        let body = &body[..end];

        let header = body
            .find("repo\n        .find_header(")
            .or_else(|| body.find("repo.find_header("))
            .expect("the blob reader must inspect the committed object's header before reading");
        let ceiling = body
            .find("MAX_INDEXED_FILE_BYTES")
            .expect("the blob reader no longer names the per-file code-index ceiling");
        let read = body
            .find(".find_object(")
            .expect("the blob reader no longer reads blobs directly; the ordering anchor moved");
        assert!(
            header < ceiling && ceiling < read,
            "the blob reader must compare the header size with MAX_INDEXED_FILE_BYTES before \
             `find_object` materializes the blob"
        );
        assert_eq!(
            code.matches(".try_into_blob()").count(),
            1,
            "a second blob read bypasses the per-file ceiling"
        );
    }

    /// Both production entry points must install the aggregate budget, and
    /// every retained source must spend it before entering the snapshot Vec.
    #[test]
    fn the_complete_snapshot_spends_the_named_aggregate_budget() {
        let code = rust_source::production_rust_code_only(include_str!("code_indexer.rs"));
        let limits_start = code
            .find("fn production_limits(")
            .expect("the production ceilings must stay named in one place");
        let limits = &code[limits_start..];
        let limits = &limits[..limits.find("\n}\n").expect("end of production_limits")];
        assert!(
            limits.contains("MAX_INDEXED_TOTAL_BYTES"),
            "the production ceilings no longer install the named total budget"
        );
        assert!(
            limits.contains("MAX_INDEXED_FILE_COUNT"),
            "the production ceilings no longer install the file-count backstop"
        );
        for entry in [
            "pub async fn index_repository(",
            "pub async fn refresh_repository(",
        ] {
            let start = code.find(entry).expect("public code-index entry point");
            let body = &code[start..];
            let body = &body[..body.find("\n    }\n").expect("end of entry point")];
            assert!(
                body.contains("production_limits()"),
                "{entry} no longer runs under the production ceilings"
            );
        }

        let collect_start = code
            .find("fn collect_tree(")
            .expect("`collect_tree` must remain the traversal boundary");
        let collect = &code[collect_start..];
        let collect = &collect[..collect.find("\n}\n").expect("end of collect_tree")];
        let decode = collect
            .find("read_indexable_blob(")
            .expect("source contents are no longer read at the aggregate-budget boundary");
        let spend = collect
            .find("budget.retain(")
            .expect("retained source bytes no longer spend the aggregate code-index budget");
        let retain = collect
            .find("entries.push(")
            .expect("`collect_tree` no longer retains index entries at the asserted boundary");
        assert!(
            decode < spend && spend < retain,
            "the decoded text must spend the aggregate budget before entering the snapshot"
        );
        assert!(
            collect[spend..retain].contains("content.len()"),
            "the aggregate budget must count retained UTF-8 bytes, including lossy expansion"
        );
    }

    #[tokio::test]
    async fn unreadable_tree_entry_fails_the_index_job_with_context() {
        let (_dir, worktree) = committed_repository(&[("src/main.rs", b"fn main() {}\n")]);
        let repo_path = worktree.join(".git");
        let tree_oid = run_git(&worktree, &["rev-parse", "HEAD^{tree}"]);
        overwrite_loose_object(&repo_path, &tree_oid, "tree", b"x");

        let error = test_indexer()
            .await
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect_err("a malformed tree entry must fail the complete index job");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&worktree.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("HEAD"), "{rendered}");
        assert!(rendered.contains(&tree_oid), "{rendered}");
        assert!(rendered.contains("Failed to read tree entry"), "{rendered}");
    }

    #[tokio::test]
    async fn unreadable_subtree_fails_the_index_job_with_context() {
        let (_dir, worktree) = committed_repository(&[("src/main.rs", b"fn main() {}\n")]);
        let repo_path = worktree.join(".git");
        let tree_oid = run_git(&worktree, &["rev-parse", "HEAD:src"]);
        std::fs::remove_file(loose_object_path(&repo_path, &tree_oid))
            .expect("fixture subtree must be removable");

        let error = test_indexer()
            .await
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect_err("a missing subtree must fail the complete index job");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&worktree.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("HEAD"), "{rendered}");
        assert!(rendered.contains("src"), "{rendered}");
        assert!(rendered.contains(&tree_oid), "{rendered}");
        assert!(
            rendered.contains("Failed to read tree object"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn unreadable_blob_fails_refresh_and_preserves_the_previous_index() {
        let (_dir, worktree) = committed_repository(&[("src/main.rs", b"fn main() {}\n")]);
        let repo_path = worktree.join(".git");
        let blob_oid = run_git(&worktree, &["rev-parse", "HEAD:src/main.rs"]);
        let indexer = test_indexer().await;
        assert_eq!(
            indexer
                .index_repository(TEST_REPO_ID, &worktree, "HEAD")
                .await
                .expect("initial healthy index must succeed"),
            1
        );
        std::fs::remove_file(loose_object_path(&repo_path, &blob_oid))
            .expect("fixture blob must be removable");

        let error = indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect_err("a missing blob must fail the complete index job");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&worktree.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("HEAD"), "{rendered}");
        assert!(rendered.contains("src/main.rs"), "{rendered}");
        assert!(rendered.contains(&blob_oid), "{rendered}");
        assert!(
            rendered.contains("Failed to read blob header"),
            "{rendered}"
        );

        let (results, total) = indexer
            .search_code("", Some(TEST_REPO_ID), 10, 0)
            .await
            .expect("the previous index must remain readable");
        assert_eq!(total, 1);
        assert_eq!(results[0].file_path, "src/main.rs");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sqlite_code_index_rebuild_publishes_atomically_without_holding_writers() {
        let directory = tempfile::tempdir().expect("create code index database directory");
        let database_path = directory.path().join("code-index.db");
        let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
        let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway SQLite database");
        rg_db::run_migrations(&db)
            .await
            .expect("run SQLite code index migrations");
        seed_repository(
            &db,
            Some(TEST_OWNER_ID),
            Some(TEST_REPO_ID),
            "sqlite-atomic",
        )
        .await;

        exercise_generation_rebuild(&db, TEST_REPO_ID).await;
        exercise_deleted_repository_rejects_refresh(&CodeIndexer::new(db.clone()), TEST_REPO_ID)
            .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
    async fn server_code_index_rebuild_publishes_atomically_without_holding_writers() {
        let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
            .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
        assert!(
            database_url.starts_with("postgres://") || database_url.starts_with("mysql://"),
            "this proof exercises PostgreSQL or MySQL repository-row locking"
        );
        let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to disposable server database");
        rg_db::run_migrations(&db)
            .await
            .expect("run server code index migrations");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let repo_id = seed_repository(&db, None, None, &suffix[..12]).await;

        exercise_generation_rebuild(&db, repo_id).await;
        exercise_deleted_repository_rejects_refresh(&CodeIndexer::new(db), repo_id).await;
    }

    #[test]
    fn generation_keys_belong_to_their_repository_and_never_to_another() {
        let key = generation_key(41, 7).expect("key");
        assert!(key < 0);
        assert_eq!(repository_of_key(key), 41);
        assert_eq!(repository_of_key(41), 41);
        let (low, high) = generation_key_range(41).expect("range");
        assert!((low..=high).contains(&key));
        let (next_low, next_high) = generation_key_range(42).expect("range");
        assert!(
            next_high < low || next_low > high,
            "two repositories share keys"
        );
        assert_eq!(following_generation(GENERATIONS_PER_REPO - 1), 1);
        assert!(generation_key(0, 1).is_err());
        assert!(generation_key(41, 0).is_err());
        assert!(generation_key(i64::MAX, 1).is_err());
    }

    #[test]
    fn test_infer_language() {
        assert_eq!(infer_language(Path::new("main.rs")), "Rust");
        assert_eq!(infer_language(Path::new("app.js")), "JavaScript");
        assert_eq!(infer_language(Path::new("README.md")), "Markdown");
        assert_eq!(infer_language(Path::new("unknown.xyz")), "Text");
    }

    #[test]
    fn test_should_index() {
        assert!(should_index_path(Path::new("src/main.rs")));
        assert!(!should_index_path(Path::new("image.png")));
        assert!(should_index_content(b"fn main() {}"));
        assert!(!should_index_content(b"binary\0payload"));
        assert_eq!(index_content(b"valid utf-8".to_vec()), "valid utf-8");
        assert_eq!(index_content(vec![b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn test_fts_escape() {
        use crate::search::dialect::fts_phrase_escape;
        assert_eq!(fts_phrase_escape("hello world"), "\"hello world\"");
        assert_eq!(fts_phrase_escape("say \"hello\""), "\"say \"\"hello\"\"\"");
    }
}
