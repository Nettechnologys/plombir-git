//! What the repository home page shows around its file list, as GitHub shows
//! it: the last commit of every entry, the commit count, the languages, and
//! every file path for "Go to file".
//!
//! Each answer is bounded. A walk or a traversal that reaches its limit stops
//! and says so (`complete: false`) instead of holding a request open for the
//! whole history of a large repository; the page then shows what it got.

use std::collections::HashMap;

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::repo_access::{CiRead, RepoContents};
use crate::api::repo_content::{
    commit_entry, lookup_tree_path, resolve_content_ref, CommitEntry, TreeEntryKind,
};
use crate::error::AppError;
use crate::AppState;

/// Commits the last-commit walk reads before it gives up on the entries it has
/// not placed yet.
pub(crate) const LAST_COMMIT_WALK_LIMIT: usize = 10_000;
/// Commits counted before the count is reported as "at least".
pub(crate) const COMMIT_COUNT_LIMIT: usize = 100_000;
/// File paths "Go to file" receives.
pub(crate) const FILE_LIST_LIMIT: usize = 50_000;
/// Tree entries the language statistics visit.
pub(crate) const LANGUAGE_ENTRY_LIMIT: usize = 200_000;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OverviewQuery {
    /// Git ref (branch, tag, commit SHA). Default: HEAD
    #[serde(default)]
    pub r#ref: Option<String>,
    /// Directory within the tree. Default: root
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct TreeCommits {
    /// The commit that last changed each entry of the directory, by entry name.
    /// An entry the walk did not reach within its limit is absent.
    pub entries: HashMap<String, CommitEntry>,
    /// `false` when the walk stopped at its limit with entries still unplaced.
    pub complete: bool,
}

#[derive(Serialize)]
pub struct CommitCount {
    pub count: usize,
    /// `false` when counting stopped at the limit: there are at least `count`.
    pub complete: bool,
}

#[derive(Serialize)]
pub struct LanguageShare {
    pub name: &'static str,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct Languages {
    /// Languages by size, largest first.
    pub languages: Vec<LanguageShare>,
    pub complete: bool,
}

#[derive(Serialize)]
pub struct FileList {
    /// Every file path of the tree, in tree order.
    pub paths: Vec<String>,
    pub complete: bool,
}

/// The repository's storage path, or the response refusing the request.
fn repository_path(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<std::path::PathBuf, axum::response::Response> {
    if let Err(e) = rg_core::platform::validate_repo_path(owner) {
        return Err(AppError::bad_request(e.to_string()).into_response());
    }
    if let Err(e) = rg_core::platform::validate_repo_path(repo) {
        return Err(AppError::bad_request(e.to_string()).into_response());
    }
    let repo_path = state.repo_root.join(format!("{owner}/{repo}.git"));
    crate::error::ensure_repository_storage(&repo_path)
        .map_err(|e| AppError::from(e).into_response())?;
    Ok(repo_path)
}

async fn answer<T: Serialize + Send + 'static>(
    label: &'static str,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> axum::response::Response {
    match rg_core::blocking::run_blocking_git(label, work).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// The last commit of every entry of a directory.
/// GET /api/v1/repos/:owner/:name/tree/commits
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/tree/commits",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        OverviewQuery,
    ),
    responses(
        (status = 200, description = "The commit that last changed each entry, by name; `complete: false` when the bounded walk stopped first", body = serde_json::Value),
        (status = 400, description = "Invalid repository path or ref, or the path is not a directory", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository, ref or path not found", body = serde_json::Value),
    ),
)]
pub async fn tree_commits(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<OverviewQuery>,
) -> impl IntoResponse {
    let repo_path = match repository_path(&state, &owner, &repo) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());
    let path = params.path.unwrap_or_default();
    answer("list last commits of a directory", move || {
        last_commits(&repo_path, &git_ref, &path, LAST_COMMIT_WALK_LIMIT)
    })
    .await
}

/// How many commits a ref reaches.
/// GET /api/v1/repos/:owner/:name/commit-count
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/commit-count",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        OverviewQuery,
    ),
    responses(
        (status = 200, description = "Commits reachable from the ref; `complete: false` means at least `count`", body = serde_json::Value),
        (status = 400, description = "Invalid repository path or ref", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository or ref not found", body = serde_json::Value),
    ),
)]
pub async fn commit_count(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<OverviewQuery>,
) -> impl IntoResponse {
    let repo_path = match repository_path(&state, &owner, &repo) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());
    answer("count commits", move || {
        count_commits(&repo_path, &git_ref, COMMIT_COUNT_LIMIT)
    })
    .await
}

/// The languages of a tree, by size.
/// GET /api/v1/repos/:owner/:name/languages
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/languages",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        OverviewQuery,
    ),
    responses(
        (status = 200, description = "Bytes per programming or markup language, largest first; vendored and documentation paths are left out", body = serde_json::Value),
        (status = 400, description = "Invalid repository path or ref", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository or ref not found", body = serde_json::Value),
    ),
)]
pub async fn languages(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<OverviewQuery>,
) -> impl IntoResponse {
    let repo_path = match repository_path(&state, &owner, &repo) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());
    answer("measure repository languages", move || {
        measure_languages(&repo_path, &git_ref, LANGUAGE_ENTRY_LIMIT)
    })
    .await
}

/// Every file path of a tree, for "Go to file".
/// GET /api/v1/repos/:owner/:name/files
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/files",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        OverviewQuery,
    ),
    responses(
        (status = 200, description = "File paths of the tree; `complete: false` when the list stopped at its limit", body = serde_json::Value),
        (status = 400, description = "Invalid repository path or ref", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository or ref not found", body = serde_json::Value),
    ),
)]
pub async fn list_files(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<OverviewQuery>,
) -> impl IntoResponse {
    let repo_path = match repository_path(&state, &owner, &repo) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());
    answer("list repository files", move || {
        file_paths(&repo_path, &git_ref, FILE_LIST_LIMIT)
    })
    .await
}

// ── Git readers ───────────────────────────────────────────────────────

fn open(repo_path: &std::path::Path) -> anyhow::Result<gix::Repository> {
    rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))
}

/// The tree of `git_ref`.
fn ref_tree<'repo>(
    repo: &'repo gix::Repository,
    git_ref: &str,
    repo_path: &std::path::Path,
) -> anyhow::Result<gix::Tree<'repo>> {
    resolve_content_ref(repo, git_ref, repo_path)?
        .object()
        .with_context(|| format!("reading ref '{git_ref}' in {}", repo_path.display()))?
        .peel_to_tree()
        .with_context(|| {
            format!(
                "resolving the tree of '{git_ref}' in {}",
                repo_path.display()
            )
        })
}

/// The id of directory `dir` in `tree` (`tree` itself for the root), or `None`
/// when this tree has no such directory.
fn directory_id(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    dir: &str,
    repo_path: &std::path::Path,
    git_ref: &str,
) -> anyhow::Result<Option<gix::ObjectId>> {
    if dir.is_empty() {
        return Ok(Some(tree.id));
    }
    Ok(lookup_tree_path(repo, tree, dir, repo_path, git_ref)?
        .filter(|entry| entry.kind.is_tree())
        .map(|entry| entry.id))
}

/// The entries of one directory object, by name.
fn directory_entries(
    repo: &gix::Repository,
    id: gix::ObjectId,
) -> anyhow::Result<HashMap<Vec<u8>, gix::ObjectId>> {
    let tree = repo
        .find_tree(id)
        .with_context(|| format!("reading directory {id}"))?;
    let mut entries = HashMap::new();
    for entry in tree.iter() {
        let entry = entry.with_context(|| format!("decoding an entry of directory {id}"))?;
        entries.insert(entry.filename().to_vec(), entry.oid().to_owned());
    }
    Ok(entries)
}

/// The commit that last gave each entry of `dir` its current content.
///
/// Commits are read newest first. A commit places an entry when it holds the
/// entry's current object and none of its parents does — the same "not
/// TREESAME to any parent" rule the path-filtered history uses, restricted to
/// the version the listing shows, so a newer change on a branch that was
/// merged away is not mistaken for the current one. A commit whose whole
/// directory equals a parent's changed nothing there and is skipped without
/// reading the directory.
pub(crate) fn last_commits(
    repo_path: &std::path::Path,
    git_ref: &str,
    dir: &str,
    walk_limit: usize,
) -> anyhow::Result<TreeCommits> {
    let dir = dir.trim_matches('/');
    let repo = open(repo_path)?;
    let head = resolve_content_ref(&repo, git_ref, repo_path)?;
    let head_tree = ref_tree(&repo, git_ref, repo_path)?;
    // `directory_id` answers `Some` for the root, so only a named directory
    // can be missing: absent, or present as something other than a directory.
    let Some(head_dir) = directory_id(&repo, head_tree.clone(), dir, repo_path, git_ref)? else {
        return Err(
            match lookup_tree_path(&repo, head_tree, dir, repo_path, git_ref)? {
                Some(_) => rg_core::error::invalid_request("path is not a directory"),
                None => anyhow::Error::new(rg_core::error::NotFound::new("path")),
            },
        );
    };
    let mut pending = directory_entries(&repo, head_dir)?;
    let mut placed = HashMap::new();

    let walk = repo
        .rev_walk([head.detach()])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()
        .with_context(|| {
            format!(
                "starting the walk of '{git_ref}' in {}",
                repo_path.display()
            )
        })?;

    for info in walk.take(walk_limit) {
        if pending.is_empty() {
            break;
        }
        let info =
            info.with_context(|| format!("walking '{git_ref}' in {}", repo_path.display()))?;
        let commit = repo
            .find_object(info.id)
            .with_context(|| format!("reading commit {}", info.id))?
            .try_into_commit()
            .with_context(|| format!("decoding commit {}", info.id))?;
        let commit_tree = commit
            .tree()
            .with_context(|| format!("reading the tree of commit {}", info.id))?;
        let Some(own) = directory_id(&repo, commit_tree, dir, repo_path, git_ref)? else {
            continue;
        };

        let mut parent_dirs = Vec::new();
        for parent in commit.parent_ids() {
            let parent_tree = repo
                .find_object(parent)
                .with_context(|| format!("reading parent {parent} of {}", info.id))?
                .try_into_commit()
                .with_context(|| format!("decoding parent {parent} of {}", info.id))?
                .tree()
                .with_context(|| format!("reading the tree of parent {parent}"))?;
            parent_dirs.push(directory_id(&repo, parent_tree, dir, repo_path, git_ref)?);
        }
        if parent_dirs.contains(&Some(own)) {
            continue;
        }

        let own_entries = directory_entries(&repo, own)?;
        let parent_entries = parent_dirs
            .into_iter()
            .flatten()
            .map(|id| directory_entries(&repo, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let introduced: Vec<Vec<u8>> = pending
            .iter()
            .filter(|(name, current)| {
                own_entries.get(*name) == Some(current)
                    && !parent_entries
                        .iter()
                        .any(|parent| parent.get(*name) == Some(current))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if introduced.is_empty() {
            continue;
        }
        let entry = commit_entry(&commit, git_ref, repo_path)?;
        for name in introduced {
            pending.remove(&name);
            placed.insert(String::from_utf8_lossy(&name).into_owned(), entry.clone());
        }
    }

    Ok(TreeCommits {
        entries: placed,
        complete: pending.is_empty(),
    })
}

/// Commits reachable from `git_ref`, counted up to `limit`.
pub(crate) fn count_commits(
    repo_path: &std::path::Path,
    git_ref: &str,
    limit: usize,
) -> anyhow::Result<CommitCount> {
    let repo = open(repo_path)?;
    let head = resolve_content_ref(&repo, git_ref, repo_path)?;
    let walk = repo.rev_walk([head.detach()]).all().with_context(|| {
        format!(
            "starting the walk of '{git_ref}' in {}",
            repo_path.display()
        )
    })?;
    let mut count = 0;
    for info in walk {
        info.with_context(|| format!("walking '{git_ref}' in {}", repo_path.display()))?;
        if count == limit {
            return Ok(CommitCount {
                count,
                complete: false,
            });
        }
        count += 1;
    }
    Ok(CommitCount {
        count,
        complete: true,
    })
}

/// Visit the tree of `git_ref` breadth first, handing every blob's path and
/// object id to `visit`, until `visit` returns `false` or `limit` entries were
/// seen. Returns whether the whole tree was visited.
fn walk_files(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    limit: usize,
    mut visit: impl FnMut(&str, gix::ObjectId) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let mut queue = std::collections::VecDeque::from([(String::new(), tree.id)]);
    let mut seen = 0_usize;
    while let Some((prefix, id)) = queue.pop_front() {
        let tree = repo
            .find_tree(id)
            .with_context(|| format!("reading directory '{prefix}'"))?;
        for entry in tree.iter() {
            let entry = entry.with_context(|| format!("decoding an entry of '{prefix}'"))?;
            seen += 1;
            if seen > limit {
                return Ok(false);
            }
            let name = String::from_utf8_lossy(entry.filename());
            let path = if prefix.is_empty() {
                name.into_owned()
            } else {
                format!("{prefix}/{name}")
            };
            match TreeEntryKind::from_mode(entry.mode()) {
                TreeEntryKind::Tree => queue.push_back((path, entry.oid().to_owned())),
                TreeEntryKind::Blob => {
                    if !visit(&path, entry.oid().to_owned())? {
                        return Ok(false);
                    }
                }
                TreeEntryKind::Commit => {}
            }
        }
    }
    Ok(true)
}

pub(crate) fn file_paths(
    repo_path: &std::path::Path,
    git_ref: &str,
    limit: usize,
) -> anyhow::Result<FileList> {
    let repo = open(repo_path)?;
    let tree = ref_tree(&repo, git_ref, repo_path)?;
    let mut paths = Vec::new();
    let complete = walk_files(&repo, tree, usize::MAX, |path, _| {
        if paths.len() == limit {
            return Ok(false);
        }
        paths.push(path.to_string());
        Ok(true)
    })?;
    Ok(FileList { paths, complete })
}

pub(crate) fn measure_languages(
    repo_path: &std::path::Path,
    git_ref: &str,
    limit: usize,
) -> anyhow::Result<Languages> {
    let repo = open(repo_path)?;
    let tree = ref_tree(&repo, git_ref, repo_path)?;
    let mut bytes: HashMap<&'static str, u64> = HashMap::new();
    let complete = walk_files(&repo, tree, limit, |path, id| {
        if is_excluded(path) {
            return Ok(true);
        }
        if let Some(language) = language_of(path) {
            let header = repo
                .find_header(id)
                .with_context(|| format!("reading the size of '{path}'"))?;
            *bytes.entry(language).or_default() += header.size();
        }
        Ok(true)
    })?;
    let mut languages: Vec<LanguageShare> = bytes
        .into_iter()
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(name, bytes)| LanguageShare { name, bytes })
        .collect();
    languages.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(b.name)));
    Ok(Languages {
        languages,
        complete,
    })
}

/// Paths GitHub's linguist leaves out of the statistics by default: vendored
/// and generated code, and documentation.
fn is_excluded(path: &str) -> bool {
    const DIRECTORIES: &[&str] = &[
        "node_modules",
        "vendor",
        "vendors",
        "third_party",
        "third-party",
        "thirdparty",
        "bower_components",
        "Godeps",
        "docs",
        "doc",
        "Documentation",
        "dist",
        ".yarn",
    ];
    let mut components = path.split('/').collect::<Vec<_>>();
    let file = components.pop().unwrap_or_default();
    components.iter().any(|dir| DIRECTORIES.contains(dir))
        || file.ends_with(".min.js")
        || file.ends_with(".min.css")
        || file.ends_with(".d.ts")
}

/// The language of a file, by name or extension; `None` for data, prose and
/// anything unknown, which linguist does not count either.
pub(crate) fn language_of(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file {
        "Dockerfile" | "Containerfile" => return Some("Dockerfile"),
        "Makefile" | "GNUmakefile" | "makefile" => return Some("Makefile"),
        "CMakeLists.txt" => return Some("CMake"),
        "Rakefile" | "Gemfile" => return Some("Ruby"),
        "Jenkinsfile" => return Some("Groovy"),
        _ => {}
    }
    if file.starts_with("Dockerfile.") {
        return Some("Dockerfile");
    }
    let extension = file.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "Rust",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TSX",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "svelte" => "Svelte",
        "vue" => "Vue",
        "astro" => "Astro",
        "py" | "pyi" => "Python",
        "ipynb" => "Jupyter Notebook",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" | "sc" => "Scala",
        "groovy" | "gradle" => "Groovy",
        "swift" => "Swift",
        "m" | "mm" => "Objective-C",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" => "C++",
        "cs" => "C#",
        "fs" | "fsi" | "fsx" => "F#",
        "rb" => "Ruby",
        "php" => "PHP",
        "pl" | "pm" => "Perl",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" => "PowerShell",
        "bat" | "cmd" => "Batchfile",
        "lua" => "Lua",
        "dart" => "Dart",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "clj" | "cljs" | "cljc" => "Clojure",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "elm" => "Elm",
        "r" => "R",
        "jl" => "Julia",
        "zig" => "Zig",
        "nim" => "Nim",
        "nix" => "Nix",
        "tf" | "hcl" => "HCL",
        "sol" => "Solidity",
        "asm" | "s" => "Assembly",
        "vim" => "Vim Script",
        "el" => "Emacs Lisp",
        "cmake" => "CMake",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "sass" => "Sass",
        "less" => "Less",
        "tex" => "TeX",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A working repository driven through the sanctioned git gateway, with
    /// every commit one minute after the previous so commit-time order is
    /// fixed.
    struct Worktree {
        dir: tempfile::TempDir,
        clock: std::cell::Cell<i64>,
    }

    impl Worktree {
        fn new() -> Self {
            let tree = Self {
                dir: tempfile::tempdir().unwrap(),
                clock: std::cell::Cell::new(1_700_000_000),
            };
            tree.git(&["init", "--initial-branch=main"]);
            tree.git(&["config", "user.name", "Overview Test"]);
            tree.git(&["config", "user.email", "overview@example.com"]);
            tree
        }

        fn path(&self) -> &std::path::Path {
            self.dir.path()
        }

        fn git(&self, args: &[&str]) -> String {
            let when = format!("{} +0000", self.clock.get());
            let output = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .expect("git gateway must initialize")
                .run_with_env(
                    args,
                    Some(self.path()),
                    &[("GIT_AUTHOR_DATE", &when), ("GIT_COMMITTER_DATE", &when)],
                )
                .expect("git must run");
            assert!(output.success(), "git {args:?}: {}", output.stderr_str());
            output.stdout_str().trim().to_string()
        }

        fn write(&self, path: &str, content: &str) {
            let file = self.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }

        /// Commit everything staged and return the new commit's id.
        fn commit(&self, message: &str) -> String {
            self.clock.set(self.clock.get() + 60);
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    fn placed(commits: &TreeCommits, name: &str) -> String {
        commits
            .entries
            .get(name)
            .unwrap_or_else(|| panic!("{name} has no last commit"))
            .sha
            .clone()
    }

    #[test]
    fn every_entry_gets_the_commit_that_gave_it_its_current_content() {
        let repo = Worktree::new();
        repo.write("a.txt", "1");
        repo.write("lib/x.txt", "1");
        repo.write("c.txt", "1");
        let first = repo.commit("first");
        repo.write("a.txt", "2");
        let second = repo.commit("second");

        // A newer change to c.txt on a side branch that the merge throws away
        // must not become c.txt's last commit.
        repo.git(&["checkout", "-q", "-b", "side", &first]);
        repo.write("c.txt", "side");
        repo.commit("side change");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("lib/x.txt", "2");
        let third = repo.commit("third");
        repo.clock.set(repo.clock.get() + 60);
        repo.git(&["merge", "-q", "-s", "ours", "--no-edit", "side"]);

        // a.txt returns to its first content: the revert is its last commit,
        // not the commit that first wrote that content.
        repo.write("a.txt", "1");
        let revert = repo.commit("revert a");

        let commits = last_commits(repo.path(), "HEAD", "", 100).unwrap();
        assert!(commits.complete);
        assert_eq!(placed(&commits, "a.txt"), revert);
        assert_eq!(placed(&commits, "lib"), third);
        assert_eq!(placed(&commits, "c.txt"), first);
        assert_ne!(placed(&commits, "a.txt"), second);
        assert_eq!(commits.entries["a.txt"].message, "revert a");

        let inside = last_commits(repo.path(), "HEAD", "lib/", 100).unwrap();
        assert_eq!(placed(&inside, "x.txt"), third);
        assert_eq!(inside.entries.len(), 1);
    }

    #[test]
    fn a_walk_that_reaches_its_limit_reports_what_it_placed() {
        let repo = Worktree::new();
        repo.write("old.txt", "1");
        repo.commit("old");
        repo.write("new.txt", "1");
        let newest = repo.commit("new");

        let commits = last_commits(repo.path(), "HEAD", "", 1).unwrap();
        assert!(!commits.complete);
        assert_eq!(placed(&commits, "new.txt"), newest);
        assert!(!commits.entries.contains_key("old.txt"));
    }

    #[test]
    fn a_missing_directory_and_a_file_are_told_apart() {
        let repo = Worktree::new();
        repo.write("file.txt", "1");
        repo.commit("one");

        let missing = last_commits(repo.path(), "HEAD", "nope", 10)
            .err()
            .expect("a missing directory must fail");
        assert!(missing.downcast_ref::<rg_core::error::NotFound>().is_some());
        let file = last_commits(repo.path(), "HEAD", "file.txt", 10)
            .err()
            .expect("a file is not a directory");
        assert!(file
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_some());
    }

    #[test]
    fn commits_are_counted_up_to_the_limit() {
        let repo = Worktree::new();
        for n in 0..5 {
            repo.write("n.txt", &n.to_string());
            repo.commit("step");
        }
        let all = count_commits(repo.path(), "main", 100).unwrap();
        assert_eq!((all.count, all.complete), (5, true));
        let capped = count_commits(repo.path(), "main", 3).unwrap();
        assert_eq!((capped.count, capped.complete), (3, false));
        let exact = count_commits(repo.path(), "main", 5).unwrap();
        assert_eq!((exact.count, exact.complete), (5, true));
    }

    #[test]
    fn files_are_listed_with_their_paths_up_to_the_limit() {
        let repo = Worktree::new();
        repo.write("b.txt", "1");
        repo.write("src/main.rs", "fn main() {}");
        repo.write("src/deep/mod.rs", "");
        repo.commit("files");

        let all = file_paths(repo.path(), "HEAD", 100).unwrap();
        assert!(all.complete);
        assert_eq!(all.paths, ["b.txt", "src/main.rs", "src/deep/mod.rs"]);
        let capped = file_paths(repo.path(), "HEAD", 2).unwrap();
        assert!(!capped.complete);
        assert_eq!(capped.paths, ["b.txt", "src/main.rs"]);
    }

    #[test]
    fn languages_count_source_bytes_and_skip_vendored_code_and_prose() {
        let repo = Worktree::new();
        repo.write("src/lib.rs", &"x".repeat(300));
        repo.write("web/app.ts", &"x".repeat(100));
        repo.write("web/node_modules/dep/index.js", &"x".repeat(10_000));
        repo.write("README.md", &"x".repeat(10_000));
        repo.write("docs/build.sh", &"x".repeat(10_000));
        repo.commit("code");

        let measured = measure_languages(repo.path(), "HEAD", 1_000).unwrap();
        assert!(measured.complete);
        let shares: Vec<_> = measured
            .languages
            .iter()
            .map(|share| (share.name, share.bytes))
            .collect();
        assert_eq!(shares, [("Rust", 300), ("TypeScript", 100)]);
    }

    #[test]
    fn languages_come_from_names_and_extensions_and_skip_data() {
        assert_eq!(language_of("crates/rg-http/src/lib.rs"), Some("Rust"));
        assert_eq!(language_of("web/src/App.svelte"), Some("Svelte"));
        assert_eq!(language_of("ci/Dockerfile"), Some("Dockerfile"));
        assert_eq!(language_of("Dockerfile.release"), Some("Dockerfile"));
        assert_eq!(language_of("include/x.HPP"), Some("C++"));
        assert_eq!(language_of("README.md"), None);
        assert_eq!(language_of("package.json"), None);
        assert_eq!(language_of("LICENSE"), None);
        assert_eq!(language_of(".gitignore"), None);
    }

    #[test]
    fn vendored_generated_and_documentation_paths_are_left_out() {
        assert!(is_excluded("web/node_modules/svelte/index.js"));
        assert!(is_excluded("vendor/lib.go"));
        assert!(is_excluded("docs/build.sh"));
        assert!(is_excluded("static/app.min.js"));
        assert!(is_excluded("types/index.d.ts"));
        assert!(!is_excluded("src/vendored.rs"));
        assert!(!is_excluded("crates/docs-gen/src/main.rs"));
    }
}
