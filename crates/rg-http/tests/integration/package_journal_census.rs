//! Census: every route that changes what a registry client gets is named here
//! together with the journal row it writes (card_8bdfea8689fd).
//!
//! The phase this guards exists because two such operations wrote nothing: an
//! npm dist-tag moved `latest` without a trace while re-tagging an OCI image
//! journaled (card_44ce692c5b9b), and a file added to an already-published
//! version — a new wheel pip prefers — left neither a row nor an author
//! (card_8c4269a5f5cf). Both were found by reading the doors one by one, which
//! is not a method. This file is that reading, mechanised.
//!
//! ## What is counted
//!
//! Every route the router really serves — [`RouteFact`]s recorded by the build
//! that produced it, not a copy of `routes.rs` — whose method changes state
//! (`POST` / `PUT` / `PATCH` / `DELETE`) and whose path is a registry path:
//! under `/packages/` or the OCI distribution root `/v2/`. Each one's handler
//! has to have an entry in [`CENSUS`]; a door added without one is red, and so
//! is an entry whose door is gone.
//!
//! ## What an entry promises, and how that is checked
//!
//! [`Journal::Writes`] names audit actions. Each one has to be spelled as a
//! string literal somewhere the handler reaches, directly or through the
//! helpers of the registry modules (`api/packages/*.rs`, `oci.rs`) — a table
//! that only *named* the action would go on claiming a row after the
//! `audit::record` call was deleted. [`Journal::Publishes`] is the publish
//! funnel: a new version is attributed by its row's `author_id` and writes no
//! journal row on purpose, a file added to an existing version is not
//! attributed by anything else and writes `package.file.add`, which is checked
//! like a `Writes`. [`Journal::Inert`] is a door that accepts a request without
//! changing what any client resolves, with the reason why.
//!
//! The reach is followed by name across those files, which can over-approximate
//! (two helpers of one name would both count); it cannot miss a call made
//! through a helper, which is the failure that matters here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use rg_http::route_table::{Access, RouteFact};

use crate::common::source_scan::{calls, functions, rust_files, src_root};
use crate::common::spawn_test_app_with_routes;

/// What a registry door leaves behind.
#[derive(Debug, Clone, Copy)]
enum Journal {
    /// One of these audit actions on every successful call.
    Writes(&'static [&'static str]),
    /// The publish funnel: a new version is attributed by `author_id` on its
    /// row, a file added to an existing version writes `package.file.add`.
    Publishes,
    /// Accepts the request without changing what any client resolves.
    Inert(&'static str),
}

/// The action the publish funnel writes for a file added to an existing version.
const FILE_ADD: &str = "package.file.add";

/// Every state-changing registry handler, keyed by `RouteFact::handler`.
const CENSUS: &[(&str, Journal)] = &[
    // ── The publish funnel: every door reaches `persist_package` ──────────
    ("rg_http::api::packages::publish", Journal::Publishes),
    ("rg_http::api::packages::publish_npm", Journal::Publishes),
    (
        "rg_http::api::packages::publish_npm_packument",
        Journal::Publishes,
    ),
    (
        "rg_http::api::packages::pypi_legacy_upload",
        Journal::Publishes,
    ),
    (
        "rg_http::api::packages::maven::maven_upload",
        Journal::Publishes,
    ),
    (
        "rg_http::api::packages::rubygems::rubygems_push",
        Journal::Publishes,
    ),
    (
        "rg_http::api::packages::cargo::cargo_publish_new",
        Journal::Publishes,
    ),
    (
        "rg_http::api::packages::nuget::nuget_publish",
        Journal::Publishes,
    ),
    // ── Removing or hiding what a client resolves ───────────────────────────
    (
        "rg_http::api::packages::delete_version",
        Journal::Writes(&["package.delete"]),
    ),
    (
        "rg_http::api::packages::yank_version",
        Journal::Writes(&["package.yank", "package.unyank"]),
    ),
    (
        "rg_http::api::packages::cargo::cargo_yank",
        Journal::Writes(&["package.yank"]),
    ),
    (
        "rg_http::api::packages::cargo::cargo_unyank",
        Journal::Writes(&["package.unyank"]),
    ),
    // ── Moving a selector without a version changing ───────────────────────
    (
        "rg_http::api::packages::set_npm_dist_tag",
        Journal::Writes(&["package.dist_tag.set"]),
    ),
    (
        "rg_http::api::packages::delete_npm_dist_tag",
        Journal::Writes(&["package.dist_tag.remove"]),
    ),
    (
        "rg_http::oci::put_manifest",
        Journal::Writes(&["oci.manifest.push"]),
    ),
    (
        "rg_http::oci::delete_manifest",
        Journal::Writes(&["oci.manifest.delete"]),
    ),
    (
        "rg_http::api::packages::oci::delete_oci_image",
        Journal::Writes(&["oci.manifest.delete"]),
    ),
    // ── Doors that change nothing a client resolves ────────────────────────
    (
        "rg_http::api::packages::maven::maven_upload_metadata",
        Journal::Inert(
            "the registry derives maven-metadata.xml itself; the uploaded one is \
             accepted so `mvn deploy` does not fail, and is not stored",
        ),
    ),
    (
        "rg_http::oci::start_upload",
        Journal::Inert(
            "a blob is content-addressed and reachable only through a manifest; \
             the manifest push is what journals",
        ),
    ),
    (
        "rg_http::oci::chunk_upload",
        Journal::Inert("part of a blob upload; see `start_upload`"),
    ),
    (
        "rg_http::oci::complete_upload",
        Journal::Inert("part of a blob upload; see `start_upload`"),
    ),
];

/// Whether a route is a state-changing registry door.
fn is_registry_mutation(fact: &RouteFact) -> bool {
    matches!(fact.method, "POST" | "PUT" | "PATCH" | "DELETE")
        && (fact.path.contains("/packages/") || fact.path.starts_with("/v2/"))
}

/// The registry doors the census does not account for, and the census entries
/// no served door answers to — both as human-readable lines.
fn census_gaps(facts: &[RouteFact], census: &[(&str, Journal)]) -> (Vec<String>, Vec<String>) {
    let named: BTreeSet<&str> = census.iter().map(|(handler, _)| *handler).collect();
    let served: BTreeSet<&str> = facts
        .iter()
        .filter(|fact| is_registry_mutation(fact))
        .map(|fact| fact.handler)
        .collect();
    let unaccounted = facts
        .iter()
        .filter(|fact| is_registry_mutation(fact) && !named.contains(fact.handler))
        .map(|fact| format!("{} → {}", fact.label(), fact.handler))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let stale = named
        .difference(&served)
        .map(|handler| handler.to_string())
        .collect();
    (unaccounted, stale)
}

/// `rg_http::api::packages::maven::maven_upload` ⇒ (`api/packages/maven.rs`,
/// `maven_upload`), resolving a module to `<module>.rs` or `<module>/mod.rs`.
fn handler_source(handler: &str) -> (String, &str) {
    let (module, name) = handler.rsplit_once("::").expect("a qualified handler");
    let module = module
        .strip_prefix("rg_http::")
        .expect("an rg-http handler")
        .replace("::", "/");
    let file = if src_root().join(format!("{module}.rs")).exists() {
        format!("{module}.rs")
    } else {
        format!("{module}/mod.rs")
    };
    (file, name)
}

/// Every production function of the registry modules, as `(file, name) → body`.
fn registry_functions() -> BTreeMap<(String, String), String> {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root.join("api/packages"), &mut files);
    files.push(root.join("oci.rs"));
    let mut out = BTreeMap::new();
    for path in files {
        let text = fs::read_to_string(&path).expect("read a registry module");
        let file = path
            .strip_prefix(&root)
            .expect("under src/")
            .to_string_lossy()
            .replace('\\', "/");
        for function in functions(&text) {
            out.insert((file.clone(), function.name), function.body);
        }
    }
    out
}

/// Whether `body` spells `"action"` as a string literal on a code line — a
/// comment naming the action writes nothing.
fn spells_action(body: &str, action: &str) -> bool {
    let literal = format!("\"{action}\"");
    body.lines().any(|line| {
        line.find(&literal)
            .is_some_and(|at| line.find("//").is_none_or(|comment| at < comment))
    })
}

/// Whether anything the handler reaches through the registry modules spells
/// `action`. `None` when the handler is not declared where its type name says.
fn reaches_action(
    registry: &BTreeMap<(String, String), String>,
    handler: &str,
    action: &str,
) -> Option<bool> {
    let (file, name) = handler_source(handler);
    let start = (file, name.to_string());
    registry.get(&start)?;
    let mut seen = BTreeSet::new();
    let mut queue = vec![start];
    while let Some(key) = queue.pop() {
        if !seen.insert(key.clone()) {
            continue;
        }
        let body = &registry[&key];
        if spells_action(body, action) {
            return Some(true);
        }
        queue.extend(
            registry
                .keys()
                .filter(|candidate| !seen.contains(*candidate) && calls(body, &candidate.1))
                .cloned(),
        );
    }
    Some(false)
}

#[tokio::test]
async fn every_registry_door_is_named_with_the_journal_row_it_writes() {
    let (_base, facts) = spawn_test_app_with_routes().await;
    assert!(
        facts
            .iter()
            .filter(|fact| is_registry_mutation(fact))
            .count()
            >= CENSUS.len(),
        "the route table served fewer registry doors than the census names — \
         the filter no longer recognises them"
    );

    let (unaccounted, stale) = census_gaps(&facts, CENSUS);
    assert!(
        unaccounted.is_empty(),
        "registry doors with no census entry — name the audit action each one \
         writes, or why it changes nothing a client resolves:\n  {}",
        unaccounted.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "census entries no served route answers to: {stale:?}"
    );

    let registry = registry_functions();
    let mut silent = Vec::new();
    for (handler, journal) in CENSUS {
        let actions: &[&str] = match journal {
            Journal::Writes(actions) => actions,
            Journal::Publishes => &[FILE_ADD],
            Journal::Inert(reason) => {
                assert!(
                    !reason.trim().is_empty(),
                    "{handler} is declared inert without saying why"
                );
                continue;
            }
        };
        for action in actions {
            match reaches_action(&registry, handler, action) {
                Some(true) => {}
                Some(false) => silent.push(format!("{handler} never writes {action}")),
                None => silent.push(format!("{handler} is not declared where its name says")),
            }
        }
    }
    assert!(
        silent.is_empty(),
        "census entries the source does not back:\n  {}",
        silent.join("\n  ")
    );
}

/// The census has teeth against the door it exists for: a new mutating route
/// under `/packages/` whose handler nobody entered.
#[test]
fn an_unnamed_registry_door_is_reported() {
    let fact = |method, path: &str, handler| RouteFact {
        method,
        path: path.to_string(),
        access: Access::RepoWrite,
        credential: None,
        handler,
    };
    let facts = [
        fact(
            "PUT",
            "/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}",
            "rg_http::api::packages::set_npm_dist_tag",
        ),
        fact(
            "PUT",
            "/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/retag",
            "rg_http::api::packages::retag_version",
        ),
        // A read is not a door, and a path outside the registry is not counted.
        fact(
            "GET",
            "/api/v1/repos/{owner}/{name}/packages/{pkg_type}/list",
            "rg_http::api::packages::list_packages",
        ),
        fact(
            "POST",
            "/api/v1/repos/{owner}/{name}/issues",
            "rg_http::api::issues::create_issue",
        ),
    ];
    let census = [(
        "rg_http::api::packages::set_npm_dist_tag",
        Journal::Writes(&["package.dist_tag.set"]),
    )];
    let (unaccounted, stale) = census_gaps(&facts, &census);
    assert_eq!(
        unaccounted,
        [
            "PUT /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/retag \
          → rg_http::api::packages::retag_version"
        ]
    );
    assert!(stale.is_empty(), "{stale:?}");

    let (_, stale) = census_gaps(&facts[2..], &census);
    assert_eq!(stale, ["rg_http::api::packages::set_npm_dist_tag"]);
}

/// A comment naming the action is not the action being written.
#[test]
fn an_action_in_a_comment_does_not_count() {
    assert!(spells_action(
        "    record(&db, \"package.yank\", None);\n",
        "package.yank"
    ));
    assert!(!spells_action(
        "    // journals as \"package.yank\"\n    record_nothing();\n",
        "package.yank"
    ));
    assert!(!spells_action(
        "    noop(); // used to write \"package.yank\"\n",
        "package.yank"
    ));
}
