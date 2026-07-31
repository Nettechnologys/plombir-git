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
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
        .await
        .with_context(|| {
            format!(
                "Failed to traverse repository {} at ref '{}'",
                repo_path.display(),
                ref_name
            )
        })?;

        let count = entries.len();
        // Do not discard a previously healthy index until the complete tree has
        // been read. A corrupt object must fail this refresh, not turn search
        // results into an empty or partial snapshot.
        self.clear_index_for_repo(repo_id).await?;
        self.batch_insert_fts(&entries).await?;

        Ok(count)
    }

    /// Clear existing index for a repository.
    async fn clear_index_for_repo(&self, repo_id: i64) -> Result<()> {
        let backend = self.db.get_database_backend();
        self.db
            .execute(Statement::from_sql_and_values(
                backend,
                rg_db::prepare_sql(backend, "DELETE FROM code_fts WHERE repo_id = ?"),
                [repo_id.into()],
            ))
            .await?;
        Ok(())
    }

    /// Collect indexable file entries by traversing the Git tree iteratively.
    #[allow(
        clippy::too_many_arguments,
        reason = "the traversal keeps repository, tree identity, output, and cycle state explicit"
    )]
    async fn collect_tree_entries(
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
        )
        .await?;
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
            )
            .await?;
        }
        Ok(())
    }

    /// Collect entries from a single tree into the entries Vec.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::clone_on_copy)]
    async fn collect_tree(
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
    async fn batch_insert_fts(&self, entries: &[IndexEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let backend = self.db.get_database_backend();
        for chunk in entries.chunks(100) {
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
            self.db
                .execute(Statement::from_sql_and_values(backend, &sql, params))
                .await?;
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
    use sea_orm::{ConnectOptions, Database};
    use std::io::Write as _;

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
        CodeIndexer::new(db)
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
