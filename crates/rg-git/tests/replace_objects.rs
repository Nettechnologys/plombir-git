//! What the server reads for an object is the object, never its replacement.
//!
//! `refs/replace/<X>` tells git to hand out another object whenever `X` is
//! read. The server reads commits to decide whether they may land — the
//! signature a protected branch requires, the paths an LFS lock covers — and
//! the branch then receives `X` itself, so a replacement in the repository
//! would answer those checks for a commit that is not the one admitted
//! (card_03ed757463d4). Both ways the server reads a repository are measured:
//! a `git` child of the gateway, and [`rg_git::repository::open`].
//!
//! The fixture is checked to hold a well-formed replacement, and the in-process
//! half carries a control that reads through it, so neither test can pass
//! because nothing was replaced. A `git` that follows the replacement cannot be
//! started from here — every child goes through the gateway, which is the
//! thing under test — so the CLI half's teeth are the paired mutation run that
//! drops `GIT_NO_REPLACE_OBJECTS` from the gateway and turns it red.
#![cfg(unix)]

use std::path::Path;

use rg_git::cli_gateway::global_gateway;

/// A bare repository holding `original` (adds `original.txt`) and `decoy`
/// (adds `decoy.txt`), and `refs/replace/<original>` pointing at the decoy.
/// The trees are read in the working repository, which has no replacement.
struct Replaced {
    _dir: tempfile::TempDir,
    bare: std::path::PathBuf,
    original: String,
    original_tree: String,
    decoy_tree: String,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = global_gateway()
        .as_ref()
        .expect("git gateway")
        .run_with_env(
            args,
            Some(dir),
            &[
                ("GIT_AUTHOR_NAME", "fixture"),
                ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
                ("GIT_COMMITTER_NAME", "fixture"),
                ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
            ],
        )
        .expect("run git");
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str()
    );
    output.stdout_str().trim().to_string()
}

impl Replaced {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let bare = dir.path().join("served.git");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("base.txt"), "base\n").unwrap();
        git(&work, &["add", "base.txt"]);
        git(&work, &["commit", "-q", "-m", "base"]);
        let base = git(&work, &["rev-parse", "HEAD"]);

        let commit = |file: &str| {
            git(&work, &["reset", "-q", "--hard", &base]);
            std::fs::write(work.join(file), "content\n").unwrap();
            git(&work, &["add", file]);
            git(&work, &["commit", "-q", "-m", file]);
            let sha = git(&work, &["rev-parse", "HEAD"]);
            git(
                &work,
                &["branch", "-f", file.trim_end_matches(".txt"), &sha],
            );
            sha
        };
        let original = commit("original.txt");
        let decoy = commit("decoy.txt");
        let tree = |sha: &str| git(&work, &["rev-parse", &format!("{sha}^{{tree}}")]);
        let (original_tree, decoy_tree) = (tree(&original), tree(&decoy));
        assert_ne!(original_tree, decoy_tree);

        git(dir.path(), &["init", "-q", "--bare", "served.git"]);
        git(
            &work,
            &["push", "-q", &bare.to_string_lossy(), "original", "decoy"],
        );
        git(
            &bare,
            &["update-ref", &format!("refs/replace/{original}"), &decoy],
        );
        assert_eq!(
            git(&bare, &["replace", "--list", "--format=long"]),
            format!("{original} (commit) -> {decoy} (commit)"),
            "the fixture holds no well-formed replacement"
        );
        Self {
            _dir: dir,
            bare,
            original,
            original_tree,
            decoy_tree,
        }
    }
}

#[test]
fn a_gateway_child_reads_the_object_not_its_replacement() {
    let repo = Replaced::new();
    let read = global_gateway()
        .as_ref()
        .unwrap()
        .run(
            &["rev-parse", &format!("{}^{{tree}}", repo.original)],
            Some(&repo.bare),
        )
        .unwrap();
    assert!(read.success(), "{}", read.stderr_str());
    assert_eq!(
        read.stdout_str().trim(),
        repo.original_tree,
        "a gateway child answered for the replacement"
    );
}

/// The tree id of `commit` as `repo` reads it.
fn tree_of(repo: &gix::Repository, commit: &str) -> gix::ObjectId {
    repo.find_commit(gix::ObjectId::from_hex(commit.as_bytes()).unwrap())
        .expect("commit")
        .tree_id()
        .expect("tree")
        .detach()
}

/// gix decides whether to load replacements from `core.useReplaceRefs`, and
/// gix 0.84 reads that key with the opposite sense git gives it. Both values
/// are planted, so the test holds whichever way a gix upgrade turns.
#[test]
fn repository_open_reads_the_object_not_its_replacement() {
    let mut armed = 0;
    for use_replace_refs in ["true", "false"] {
        let repo = Replaced::new();
        git(
            &repo.bare,
            &["config", "core.useReplaceRefs", use_replace_refs],
        );
        let opened = rg_git::repository::open(&repo.bare).expect("open");
        assert_eq!(
            tree_of(&opened, &repo.original).to_string(),
            repo.original_tree,
            "repository::open answered for the replacement \
             (core.useReplaceRefs = {use_replace_refs})"
        );

        // Control: a fresh handle with replacements switched back on reads the
        // decoy whenever gix loaded the replacement for this config.
        let mut control = rg_git::repository::open(&repo.bare).expect("open");
        control.objects.ignore_replacements = false;
        if tree_of(&control, &repo.original).to_string() == repo.decoy_tree {
            armed += 1;
        }
    }
    assert!(
        armed > 0,
        "gix loaded the replacement under neither value of core.useReplaceRefs, \
         so this test would prove nothing"
    );
}
