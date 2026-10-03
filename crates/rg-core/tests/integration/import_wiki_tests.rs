//! The `Wiki` checkbox of an import has to reach the wiki (card_20cf2efd80c4).
//!
//! `import_tasks.import_wiki` used to travel the whole way from the UI to its
//! own database column and stop there: no runner read it, nothing ever assigned
//! `ImportStats::wiki_pages_imported`, and a user who ticked the box got a
//! `completed` task reporting zero pages — indistinguishable from a source that
//! genuinely had no wiki.
//!
//! These tests drive the step that now consumes it against a real local
//! `.wiki.git` source: the pages that come across, the files that must not, and
//! the two shapes of "there is nothing here" that may not fail an import.

use std::path::Path;

/// Through the gateway, like every other git invocation in the tree.
///
/// Spawning the binary directly here is what `test_no_raw_git_command_in_crates`
/// exists to refuse: the gateway is where the binary is resolved and where the
/// environment a subprocess inherits is decided, so a second spawn site is a
/// second policy. (That gate greps the source line by line and does not strip
/// comments, so spelling the forbidden call even in prose trips it.)
fn git(args: &[&str], cwd: &Path) {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap_or_else(|error| panic!("git gateway unavailable: {error}"));
    let output = gateway
        .run(args, Some(cwd))
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str()
    );
}

/// A wiki repository as gollum would leave it: pages at the root, an
/// attachment they link to, a page in a subdirectory, and one page written in a
/// markup a Plombir Git wiki page cannot hold.
fn source_wiki(dir: &Path) -> std::path::PathBuf {
    let source = dir.join("source.wiki");
    std::fs::create_dir_all(source.join("assets")).expect("create the source wiki");
    std::fs::create_dir_all(source.join("Guides")).expect("create the source wiki subdirectory");

    std::fs::write(
        source.join("Home.md"),
        "Welcome to the [[Getting-Started]].",
    )
    .unwrap();
    std::fs::write(source.join("Getting-Started.md"), "Install it, run it.").unwrap();
    std::fs::write(source.join("Guides/Deploying.markdown"), "Push the button.").unwrap();
    std::fs::write(source.join("Legacy.rdoc"), "= Legacy").unwrap();
    std::fs::write(source.join("assets/diagram.png"), [0x89, b'P', b'N', b'G']).unwrap();

    git(&["init", "--initial-branch=main"], &source);
    git(&["config", "user.name", "Wiki Author"], &source);
    git(&["config", "user.email", "wiki@example.invalid"], &source);
    git(&["add", "."], &source);
    git(&["commit", "-m", "wiki"], &source);

    source
}

/// A repository whose wiki is empty, ready to receive imported pages.
async fn target(
    dir: &Path,
    name: &str,
) -> (
    sea_orm::DatabaseConnection,
    rg_db::entities::user::Model,
    rg_db::entities::repository::Model,
) {
    let db = crate::common::migrated_sqlite(&dir.join("test.db"), 2).await;
    let owner =
        rg_db::ops::user_ops::create_user(&db, name, &format!("{name}@example.invalid"), "", name)
            .await
            .unwrap_or_else(|error| panic!("create user {name}: {error:#}"));
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "imported",
        None,
        false,
        &dir.join("repos"),
        None,
    )
    .await
    .expect("create the target repository");

    (db, owner, repo)
}

#[tokio::test]
async fn the_pages_of_a_source_wiki_land_in_the_target_repository_wiki() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = source_wiki(dir.path());
    let (db, owner, repo) = target(dir.path(), "wikiimport").await;
    let staging = dir
        .path()
        .join("repos/wikiimport/.imported.wiki.git.importing-1");

    let imported = rg_core::import::service::import_wiki_pages_from_local_path(
        &db,
        repo.id,
        &source,
        staging.clone(),
        None,
        Some(owner.id),
    )
    .await
    .expect("import the source wiki");

    // Three Markdown pages; the `.rdoc` page and the PNG attachment are not
    // pages this wiki can hold.
    assert_eq!(imported, 3, "the import did not carry every Markdown page");

    let mut titles: Vec<String> = rg_core::wiki::service::list_pages(&db, repo.id)
        .await
        .expect("list the target wiki")
        .into_iter()
        .map(|page| page.title)
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        vec![
            "Deploying".to_string(),
            "Getting-Started".to_string(),
            "Home".to_string(),
        ],
        "the imported titles are not the slugs the source served"
    );

    // The stem is kept verbatim, so the `[[Getting-Started]]` link the source
    // page carries still names a page that exists here.
    let home = rg_core::wiki::service::get_page(&db, repo.id, "Home")
        .await
        .expect("read the imported page")
        .expect("Home was not imported");
    assert_eq!(home.content, "Welcome to the [[Getting-Started]].");
    assert_eq!(home.author_id, Some(owner.id));

    assert!(
        !staging.exists(),
        "the wiki clone was left behind under the repository root: {}",
        staging.display()
    );
}

/// The step exists because the box promised something; it must not turn the
/// absence of a wiki into a failed import of the repository that was already
/// cloned. Both platforms answer a wiki that was never written with exactly
/// this: a clone that does not come back.
#[tokio::test]
async fn a_source_without_a_wiki_leaves_the_import_standing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (db, owner, repo) = target(dir.path(), "wikiabsent").await;
    let staging = dir
        .path()
        .join("repos/wikiabsent/.imported.wiki.git.importing-1");

    let imported = rg_core::import::service::import_wiki_pages_from_local_path(
        &db,
        repo.id,
        &dir.path().join("nothing-here.wiki.git"),
        staging.clone(),
        None,
        Some(owner.id),
    )
    .await
    .expect("an absent wiki must not fail the import");

    assert_eq!(imported, 0);
    assert!(
        !staging.exists(),
        "a failed wiki clone was left behind: {}",
        staging.display()
    );
}

/// A wiki the platform created and nobody ever wrote to clones fine and has no
/// `HEAD` to list — the second shape of "there is nothing here", and the one an
/// unguarded `ls-tree` would report as a broken import.
#[tokio::test]
async fn an_empty_source_wiki_imports_no_pages_and_does_not_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("empty.wiki.git");
    std::fs::create_dir_all(&source).expect("create the empty wiki");
    git(&["init", "--bare", "--initial-branch=main"], &source);

    let (db, owner, repo) = target(dir.path(), "wikiempty").await;
    let staging = dir
        .path()
        .join("repos/wikiempty/.imported.wiki.git.importing-1");

    let imported = rg_core::import::service::import_wiki_pages_from_local_path(
        &db,
        repo.id,
        &source,
        staging.clone(),
        None,
        Some(owner.id),
    )
    .await
    .expect("an empty wiki must not fail the import");

    assert_eq!(imported, 0);
}

/// A page the target already holds is not overwritten by the source's, and the
/// import keeps going for the rest.
#[tokio::test]
async fn a_page_the_target_already_holds_is_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = source_wiki(dir.path());
    let (db, owner, repo) = target(dir.path(), "wikiclash").await;
    rg_core::wiki::service::create_page(&db, repo.id, "Home", "ours", None, Some(owner.id))
        .await
        .expect("seed the target wiki");
    let staging = dir
        .path()
        .join("repos/wikiclash/.imported.wiki.git.importing-1");

    let imported = rg_core::import::service::import_wiki_pages_from_local_path(
        &db,
        repo.id,
        &source,
        staging.clone(),
        None,
        Some(owner.id),
    )
    .await
    .expect("a title clash must not fail the import");

    assert_eq!(imported, 2, "the clashing page was counted as imported");
    let home = rg_core::wiki::service::get_page(&db, repo.id, "Home")
        .await
        .expect("read the target page")
        .expect("Home disappeared");
    assert_eq!(
        home.content, "ours",
        "the import overwrote a page the target already held"
    );
}
