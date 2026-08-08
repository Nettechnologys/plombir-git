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
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::db_retry::{classify, classify_anyhow};
use crate::search::dialect::{code_fts_snippet_expr, fts_match, CODE_FTS_COLS};

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

/// Check if a file should be indexed (not binary, not too large).
fn should_index(path: &Path, content: &[u8]) -> bool {
    if content.len() > 1_048_576 {
        return false;
    }
    if content.contains(&0u8) {
        return false;
    }

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

/// A code search result.
#[derive(Debug, serde::Serialize)]
pub struct CodeSearchResult {
    pub repo_id: i64,
    pub file_path: String,
    pub file_name: String,
    pub language: String,
    pub snippet: String,
}

/// Code indexer service.
pub struct CodeIndexer {
    db: DatabaseConnection,
}

/// Entry for batch FTS insertion.
struct IndexEntry {
    repo_id: i64,
    file_path: String,
    file_name: String,
    content: String,
    language: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexWritePoint {
    Cleared,
    BatchInserted(usize),
}

impl CodeIndexer {
    /// Create a new code indexer.
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// Index a repository by traversing its Git tree.
    pub async fn index_repository(
        &self,
        repo_id: i64,
        repo_path: &Path,
        ref_name: &str,
    ) -> Result<usize> {
        // The whole Git traversal happens inside this block so that no `gix`
        // value is still alive at the `.await` below. `gix::Repository` holds a
        // `RefCell` and is therefore `!Sync`, which makes any future that keeps
        // one across a suspension point `!Send` — and axum only serves `Send`
        // futures. That is what kept the HTTP indexing endpoint unmountable
        // (card_928d72df493a); the CLI never noticed because `block_on` has no
        // such bound. Ending the borrow before the write is what makes the same
        // function usable from both callers.
        let entries = {
            let repo = gix::open(repo_path)
                .with_context(|| format!("Failed to open repository: {}", repo_path.display()))?;

            let commit_id = repo
                .rev_parse_single(ref_name)
                .with_context(|| format!("Failed to resolve ref: {}", ref_name))?;

            let commit = repo
                .find_commit(commit_id)
                .with_context(|| format!("Failed to find commit: {}", commit_id))?;

            let decoded = commit
                .decode()
                .with_context(|| "Failed to decode commit".to_string())?;
            let tree_oid = decoded.tree();

            let tree = repo
                .find_tree(tree_oid)
                .with_context(|| format!("Failed to find tree: {}", tree_oid))?;

            let mut entries: Vec<IndexEntry> = Vec::new();
            let mut visited = HashSet::new();
            self.collect_tree_entries(
                &repo,
                &tree,
                tree_oid,
                repo_id,
                PathBuf::new(),
                &mut entries,
                &mut visited,
            )
            .with_context(|| {
                format!(
                    "Failed to traverse repository {} at ref '{}'",
                    repo_path.display(),
                    ref_name
                )
            })?;
            entries
        };

        let count = entries.len();
        // Do not discard a previously healthy index until the complete tree has
        // been read. A corrupt object must fail this refresh, not turn search
        // results into an empty or partial snapshot.
        self.replace_index_entries(repo_id, &entries, |_| std::future::ready(Ok(())))
            .await?;

        Ok(count)
    }

    /// How many files this repository currently has in the code index.
    ///
    /// Zero is the "no snapshot" state, and both readers of it branch on that
    /// one fact: the AI search handler refuses the query, and the post-push
    /// refresh (`rg_core::push_hooks`) leaves the repository alone rather than
    /// building an index nobody asked for. It lives here rather than as a raw
    /// `SELECT COUNT(*)` at each call site because `code_fts` is this module's
    /// table — the HTTP layer had a hand-written copy of this query, and a
    /// second one in the hook path would have made three.
    pub async fn indexed_file_count(&self, repo_id: i64) -> Result<i64> {
        let backend = self.db.get_database_backend();
        let sql = rg_db::prepare_sql(backend, "SELECT COUNT(*) FROM code_fts WHERE repo_id = ?");
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                backend,
                &sql,
                [repo_id.into()],
            ))
            .await
            .with_context(|| format!("count code index rows for repository {repo_id}"))?
            .with_context(|| format!("COUNT(*) returned no row for repository {repo_id}"))?;
        row.try_get_by_index(0)
            .with_context(|| format!("decode code index row count for repository {repo_id}"))
    }

    /// Publish one complete repository snapshot.
    ///
    /// The transaction is the visibility boundary: a failed clear or batch
    /// rolls back to the previous snapshot. PostgreSQL and MySQL additionally
    /// lock the owning repository row before the clear, so two refreshes for
    /// the same repository cannot interleave their generations. SQLite's first
    /// DELETE takes its database-wide writer lock and provides the same ordering.
    async fn replace_index_entries<F, Fut>(
        &self,
        repo_id: i64,
        entries: &[IndexEntry],
        after_write: F,
    ) -> Result<()>
    where
        F: Fn(IndexWritePoint) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        const MAX_ATTEMPTS: usize = 32;

        for attempt in 1..=MAX_ATTEMPTS {
            let transaction = match self.db.begin().await {
                Ok(transaction) => transaction,
                Err(error) if attempt < MAX_ATTEMPTS && classify(&error).is_worthwhile() => {
                    classify(&error).wait(attempt).await;
                    continue;
                }
                Err(error) => return Err(error).context("begin atomic code index refresh"),
            };

            let write_result: Result<()> = async {
                self.lock_repository_for_refresh(&transaction, repo_id)
                    .await?;
                self.clear_index_for_repo(&transaction, repo_id).await?;
                after_write(IndexWritePoint::Cleared).await?;
                self.batch_insert_fts(&transaction, entries, &after_write)
                    .await
            }
            .await;

            if let Err(error) = write_result {
                let retry = classify_anyhow(&error);
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "code index refresh failed and its transaction could not be rolled back: \
                         {rollback_error}"
                    ));
                }
                if retry.is_worthwhile() && attempt < MAX_ATTEMPTS {
                    retry.wait(attempt).await;
                    continue;
                }
                if retry.is_worthwhile() {
                    return Err(error).context(format!(
                        "serialize code index refresh after {MAX_ATTEMPTS} concurrent conflicts"
                    ));
                }
                return Err(error);
            }

            match transaction.commit().await {
                Ok(()) => return Ok(()),
                Err(error) if attempt < MAX_ATTEMPTS && classify(&error).is_worthwhile() => {
                    classify(&error).wait(attempt).await;
                    continue;
                }
                Err(error) if rg_db::is_retryable_transaction_error(&error) => {
                    return Err(error).context(format!(
                        "commit code index refresh after {MAX_ATTEMPTS} concurrent conflicts"
                    ));
                }
                Err(error) => return Err(error).context("commit atomic code index refresh"),
            }
        }

        unreachable!("the bounded code index refresh loop returns or continues on every attempt")
    }

    /// Serialize refreshes on server databases without locking unrelated repos.
    /// SQLite has one writer for the whole database, acquired by the clear below.
    async fn lock_repository_for_refresh(
        &self,
        transaction: &DatabaseTransaction,
        repo_id: i64,
    ) -> Result<()> {
        let backend = transaction.get_database_backend();
        if backend == DatabaseBackend::Sqlite {
            return Ok(());
        }

        let sql = rg_db::prepare_sql(
            backend,
            "SELECT id FROM repositories WHERE id = ? FOR UPDATE",
        );
        transaction
            .query_one(Statement::from_sql_and_values(
                backend,
                &sql,
                [repo_id.into()],
            ))
            .await
            .with_context(|| format!("lock repository {repo_id} for code index refresh"))?
            .with_context(|| {
                format!("repository {repo_id} disappeared before code index refresh")
            })?;
        Ok(())
    }

    /// Clear existing index for a repository inside the refresh transaction.
    async fn clear_index_for_repo(
        &self,
        transaction: &DatabaseTransaction,
        repo_id: i64,
    ) -> Result<()> {
        let backend = transaction.get_database_backend();
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                rg_db::prepare_sql(backend, "DELETE FROM code_fts WHERE repo_id = ?"),
                [repo_id.into()],
            ))
            .await
            .with_context(|| format!("clear code index for repository {repo_id}"))?;
        Ok(())
    }

    /// Collect indexable file entries by traversing the Git tree iteratively.
    #[allow(
        clippy::too_many_arguments,
        reason = "the traversal keeps repository, tree identity, output, and cycle state explicit"
    )]
    fn collect_tree_entries(
        &self,
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        tree_oid: gix::ObjectId,
        repo_id: i64,
        base_path: PathBuf,
        entries: &mut Vec<IndexEntry>,
        visited: &mut HashSet<gix::ObjectId>,
    ) -> Result<()> {
        let mut stack: Vec<(gix::ObjectId, PathBuf)> = Vec::new();
        self.collect_tree(
            repo, tree, tree_oid, repo_id, base_path, entries, visited, &mut stack,
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
            self.collect_tree(
                repo, &tree, tree_oid, repo_id, path, entries, visited, &mut stack,
            )?;
        }
        Ok(())
    }

    /// Collect entries from a single tree into the entries Vec.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::clone_on_copy)]
    fn collect_tree(
        &self,
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        tree_oid: gix::ObjectId,
        repo_id: i64,
        base_path: PathBuf,
        entries: &mut Vec<IndexEntry>,
        visited: &mut HashSet<gix::ObjectId>,
        stack: &mut Vec<(gix::ObjectId, PathBuf)>,
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
                let oid = item.oid().to_owned();
                if !visited.contains(&oid) {
                    visited.insert(oid.clone());
                    stack.push((oid, path));
                }
            } else if mode.is_blob() || mode.is_executable() {
                let oid = item.oid().to_owned();
                let object = repo.find_object(oid).with_context(|| {
                    format!("Failed to read blob object {} at '{}'", oid, path.display())
                })?;
                let blob = object.try_into_blob().map_err(|_| {
                    anyhow::anyhow!("Object {} at '{}' is not a blob", oid, path.display())
                })?;
                let content = &blob.data;
                if should_index(&path, content) {
                    let file_path = path.to_string_lossy().to_string();
                    let file_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let language = infer_language(&path);
                    let content_str = String::from_utf8_lossy(content).to_string();

                    entries.push(IndexEntry {
                        repo_id,
                        file_path,
                        file_name,
                        content: content_str,
                        language,
                    });
                }
            }
        }
        Ok(())
    }

    /// Batch-insert index entries into the `code_fts` table using a
    /// parameterized multi-row INSERT. `rg_db::prepare_sql` converts portable
    /// bind markers to PostgreSQL's numbered form before execution.
    async fn batch_insert_fts<F, Fut>(
        &self,
        transaction: &DatabaseTransaction,
        entries: &[IndexEntry],
        after_write: &F,
    ) -> Result<()>
    where
        F: Fn(IndexWritePoint) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        if entries.is_empty() {
            return Ok(());
        }
        let backend = transaction.get_database_backend();
        for (batch_index, chunk) in entries.chunks(100).enumerate() {
            let mut placeholders = String::new();
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * 5);
            for (i, entry) in chunk.iter().enumerate() {
                if i > 0 {
                    placeholders.push_str(", ");
                }
                placeholders.push_str("(?, ?, ?, ?, ?)");
                params.push(Value::from(entry.repo_id));
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
                        "insert code index batch {} for repository {}",
                        batch_index + 1,
                        chunk[0].repo_id
                    )
                })?;
            after_write(IndexWritePoint::BatchInserted(batch_index)).await?;
        }
        Ok(())
    }

    /// Search code using backend-appropriate full-text search.
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

        let where_body = match (match_pred.is_empty(), repo_id.is_some()) {
            (true, true) => "repo_id = ?".to_string(),
            (true, false) => "1=1".to_string(),
            (false, true) => format!("{} AND repo_id = ?", match_pred),
            (false, false) => match_pred,
        };

        // Parameter order follows SQL order: match predicate, repo filter,
        // then the repeated ranking expression.
        let mut params: Vec<Value> = query_values
            .first()
            .cloned()
            .map(Value::from)
            .into_iter()
            .collect();
        if let Some(rid) = repo_id {
            params.push(Value::from(rid));
        }
        if let Some(order_value) = query_values.get(1) {
            params.push(Value::from(order_value.clone()));
        }
        let count_params: Vec<Value> = query_values
            .first()
            .cloned()
            .map(Value::from)
            .into_iter()
            .chain(repo_id.map(Value::from))
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
                "SELECT repo_id, file_path, file_name, language, {} as snippet \
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
                    repo_id: row.try_get_by_index(0)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectOptions, Database};
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{oneshot, Notify};

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

    fn generation_entries(repo_id: i64, generation: &str, count: usize) -> Vec<IndexEntry> {
        (0..count)
            .map(|index| {
                let file_path = format!("{generation}/{index:03}.rs");
                IndexEntry {
                    repo_id,
                    file_name: format!("{index:03}.rs"),
                    content: format!("fn {generation}_{index}() {{}}"),
                    language: "Rust".to_string(),
                    file_path,
                }
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
            .search_code("", Some(repo_id), 1_000, 0)
            .await
            .expect("read code index snapshot");
        assert_eq!(
            usize::try_from(total).expect("non-negative code index count"),
            rows.len(),
            "the count and rows must describe the same quiescent snapshot"
        );
        let mut paths = rows
            .into_iter()
            .map(|row| row.file_path)
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    async fn replace_paused_after_first_batch(
        db: DatabaseConnection,
        repo_id: i64,
        entries: Vec<IndexEntry>,
        reached_tx: oneshot::Sender<()>,
        release: Arc<Notify>,
    ) -> Result<()> {
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        CodeIndexer::new(db)
            .replace_index_entries(repo_id, &entries, move |point| {
                let reached_tx = reached_tx.clone();
                let release = release.clone();
                async move {
                    if point == IndexWritePoint::BatchInserted(0) {
                        let sender = reached_tx.lock().expect("lock refresh pause sender").take();
                        if let Some(sender) = sender {
                            sender.send(()).expect("announce paused code index refresh");
                            release.notified().await;
                        }
                    }
                    Ok(())
                }
            })
            .await
    }

    async fn exercise_atomic_refresh(db: &DatabaseConnection, repo_id: i64) {
        let indexer = CodeIndexer::new(db.clone());
        let old_entries = generation_entries(repo_id, "old", 2);
        let old_paths = entry_paths(&old_entries);
        indexer
            .replace_index_entries(repo_id, &old_entries, |_| std::future::ready(Ok(())))
            .await
            .expect("seed the old complete index snapshot");

        let replacement = generation_entries(repo_id, "replacement", 205);
        for fail_at in [
            IndexWritePoint::Cleared,
            IndexWritePoint::BatchInserted(0),
            IndexWritePoint::BatchInserted(1),
            IndexWritePoint::BatchInserted(2),
        ] {
            let error = indexer
                .replace_index_entries(repo_id, &replacement, move |point| async move {
                    if point == fail_at {
                        anyhow::bail!("injected code index failure after {point:?}");
                    }
                    Ok(())
                })
                .await
                .expect_err("an injected refresh failure must escape");
            assert!(format!("{error:#}").contains("injected code index failure"));
            assert_eq!(
                indexed_paths(&indexer, repo_id).await,
                old_paths,
                "{fail_at:?} published a partial replacement"
            );
        }

        // Readers may either keep seeing the old committed generation or wait
        // for the writer and see the new one. They must never see the first
        // committed batch while the rest of the transaction is paused. MySQL
        // can choose the blocking behavior for its FULLTEXT table, so the test
        // releases the writer after a bounded observation window rather than
        // waiting for the reader and the writer in a cycle.
        let reader_writer_entries = generation_entries(repo_id, "reader-writer", 205);
        let reader_writer_paths = entry_paths(&reader_writer_entries);
        let (reached_tx, reached_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let reader_writer = tokio::spawn(replace_paused_after_first_batch(
            db.clone(),
            repo_id,
            reader_writer_entries,
            reached_tx,
            release.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("reader-boundary refresh did not reach its first batch")
            .expect("reader-boundary refresh dropped its pause signal");

        let reader_indexer = CodeIndexer::new(db.clone());
        let mut reader = tokio::spawn(async move { indexed_paths(&reader_indexer, repo_id).await });
        let early_reader = match tokio::time::timeout(Duration::from_millis(200), &mut reader).await
        {
            Ok(result) => Some(result.expect("code index reader task panicked")),
            Err(_) => None,
        };
        if let Some(observed) = &early_reader {
            assert_eq!(
                observed, &old_paths,
                "a reader observed an uncommitted batch"
            );
        }

        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), reader_writer)
            .await
            .expect("reader-boundary refresh stayed blocked")
            .expect("reader-boundary refresh task panicked")
            .expect("reader-boundary refresh failed");
        let reader_observation = match early_reader {
            Some(observed) => observed,
            None => tokio::time::timeout(Duration::from_secs(10), reader)
                .await
                .expect("code index reader stayed blocked after commit")
                .expect("code index reader task panicked"),
        };
        assert!(
            reader_observation == old_paths || reader_observation == reader_writer_paths,
            "a reader observed neither complete generation: {reader_observation:?}"
        );
        assert_eq!(
            indexed_paths(&indexer, repo_id).await,
            reader_writer_paths,
            "reader-boundary refresh did not publish its complete generation"
        );

        // Now hold one refresh after its first batch and prove a second refresh
        // for the same repository cannot complete until the first commits.
        let first_entries = generation_entries(repo_id, "first", 205);
        let second_entries = generation_entries(repo_id, "second", 3);
        let second_paths = entry_paths(&second_entries);
        let (reached_tx, reached_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let first = tokio::spawn(replace_paused_after_first_batch(
            db.clone(),
            repo_id,
            first_entries,
            reached_tx,
            release.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("first concurrent refresh did not reach its first batch")
            .expect("first concurrent refresh dropped its pause signal");

        let second_db = db.clone();
        let mut second = tokio::spawn(async move {
            CodeIndexer::new(second_db)
                .replace_index_entries(repo_id, &second_entries, |_| std::future::ready(Ok(())))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut second)
                .await
                .is_err(),
            "a concurrent refresh crossed the first refresh's repository lock"
        );

        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), first)
            .await
            .expect("first refresh stayed blocked")
            .expect("first refresh task panicked")
            .expect("first refresh failed");
        tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .expect("second refresh stayed blocked")
            .expect("second refresh task panicked")
            .expect("second refresh failed");
        assert_eq!(
            indexed_paths(&indexer, repo_id).await,
            second_paths,
            "concurrent refreshes published a mixed generation"
        );
    }

    fn committed_repository(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temporary directory must be created");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree directory must be created");
        run_git(&worktree, &["init", "-q", "-b", "main"]);
        run_git(&worktree, &["config", "user.name", "ForgeKeep Index Test"]);
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
    async fn healthy_repository_indexes_the_complete_file_set() {
        let (_dir, worktree) = committed_repository(&[
            ("README.md", b"searchable readme\n"),
            ("src/main.rs", b"fn main() {}\n"),
            ("assets/image.png", b"binary\0payload"),
        ]);
        let indexer = test_indexer().await;

        let count = indexer
            .index_repository(TEST_REPO_ID, &worktree, "HEAD")
            .await
            .expect("healthy repository must index");
        assert_eq!(count, 2, "binary files are the only excluded fixture entry");

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
            rendered.contains("Failed to read blob object"),
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
    async fn sqlite_code_index_refresh_is_failure_atomic_and_serialized() {
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

        exercise_atomic_refresh(&db, TEST_REPO_ID).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
    async fn server_code_index_refresh_is_failure_atomic_and_serialized() {
        let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
            .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
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

        exercise_atomic_refresh(&db, repo_id).await;
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
        assert!(should_index(Path::new("src/main.rs"), b"fn main() {}"));
        assert!(!should_index(Path::new("image.png"), &[0u8; 100]));
        assert!(!should_index(Path::new("large_file.rs"), &[0u8; 2_000_000]));
    }

    #[test]
    fn test_fts_escape() {
        use crate::search::dialect::fts_phrase_escape;
        assert_eq!(fts_phrase_escape("hello world"), "\"hello world\"");
        assert_eq!(fts_phrase_escape("say \"hello\""), "\"say \"\"hello\"\"\"");
    }
}
