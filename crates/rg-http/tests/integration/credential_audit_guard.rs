//! Source guard: an endpoint that mints or revokes a long-lived credential
//! writes a journal entry (card_4a8cb474a877).
//!
//! `users.rs` wrote exactly two rows — `user.register` and `user.login` — so the
//! journal knew an account had logged in and did not know that a minute later
//! it grew a personal token scoped `repo`. `ssh_keys.rs` and `ci_secrets.rs`
//! wrote none at all. An incident review of a compromised account begins at
//! precisely that question: which keys and tokens exist, and when did they
//! appear. `created_at` on a row does not answer it — it is not an event, it
//! sits nowhere near the login from the unfamiliar address, and a credential
//! that was *deleted* leaves no trace whatsoever.
//!
//! The sibling of `access_grant_audit_guard`, and the reason both exist rather
//! than one: they start from different shapes. A grant is written through
//! `rg-core`/`rg-db` and has to be found by closing over the call graph; a
//! credential row is built in the handler itself, so the shape is right there.
//!
//! ## The hold list
//!
//! [`AWAITING_A_CARD`] is empty, and that is a state worth keeping. It held
//! `api::deploy_keys` while card_2a9beaf7b207 was open — a deploy key with
//! `read_only: false` grants **push to one repository**, which made it a
//! repository access grant first and an account credential second, so it was
//! journalled beside the other ways of granting repository access rather than
//! here. That card has landed, both handlers journal, and the entries are gone:
//! the exemption was a ratchet, and the assertion below is what turned it.
//! Deploy keys are now held to this rule like every other credential.

use std::fs;

use crate::common::source_scan::{
    calls, crate_relative, functions, production_rust_code_only, rust_files, workspace_crates,
};

/// One kind of long-lived credential, named by the two things that identify it
/// in source: the entity whose row *is* the credential, and the ops module that
/// removes it.
struct Credential {
    /// `rg_db::entities::<entity>::ActiveModel` — building one mints the
    /// credential. The handlers build these themselves, which is why no call
    /// graph has to be walked.
    entity: &'static str,
    /// `rg_db::ops::<ops>::<verb>…` — the module that owns the table. Matched on
    /// the module plus a mutating verb rather than on each function name, so a
    /// second spelling of "write it" or "remove it" cannot appear without this
    /// rule noticing. It is the only way `ci_secrets::put` is seen at all: it
    /// calls `ci_secret_ops::upsert`, which builds the row inside `rg-db`, so
    /// there is no `ActiveModel` in the handler to recognise.
    ops: &'static str,
}

/// What a handler does to a credential table that is not reading it.
const MUTATING_VERBS: [&str; 4] = ["create", "upsert", "update", "delete"];

const CREDENTIALS: [Credential; 4] = [
    // A personal access token authenticates as the account it belongs to.
    Credential {
        entity: "access_token",
        ops: "token_ops",
    },
    // An SSH key is the account's push credential from a given machine.
    Credential {
        entity: "ssh_key",
        ops: "ssh_key_ops",
    },
    // A CI secret is a value every job of the repository can read, so anyone
    // who can push a branch can read it out.
    Credential {
        entity: "ci_secret",
        ops: "ci_secret_ops",
    },
    // A deploy key with `read_only: false` is push access to one repository for
    // whoever holds the private half. Journalled as a *grant* rather than an
    // account credential — see the module header — which this rule accepts,
    // because `record_grant` is one of the spellings it recognises.
    Credential {
        entity: "deploy_key",
        ops: "deploy_key_ops",
    },
];

/// Handlers held out of the rule, each with the card that closes the hold.
///
/// A ratchet, not an opt-out: the assertion below requires every entry here to
/// *still* be journalling nothing. An entry that has been fixed and not removed
/// is a lie about the tree, and it fails.
const AWAITING_A_CARD: [(&str, &str); 0] = [];

/// Either spelling of "a row was written to `audit_log`".
const JOURNAL_CALLS: [&str; 3] = ["record_credential", "record_grant", "record"];

impl Credential {
    /// Whether `body` mints or revokes this credential.
    fn written_in(&self, body: &str) -> bool {
        let code = production_rust_code_only(body);
        code.contains(&format!("{}::ActiveModel", self.entity))
            || MUTATING_VERBS
                .iter()
                .any(|verb| code.contains(&format!("{}::{verb}", self.ops)))
    }
}

#[test]
fn every_endpoint_that_mints_or_revokes_a_credential_writes_a_journal_entry() {
    let mut files = Vec::new();
    rust_files(&workspace_crates().join("rg-http/src/api"), &mut files);
    assert!(!files.is_empty(), "no handler module was read");

    let mut census: Vec<String> = Vec::new();
    let mut silent: Vec<String> = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let path = crate_relative(&file);
        for handler in functions(&text) {
            if !handler.is_handler {
                continue;
            }
            if !CREDENTIALS
                .iter()
                .any(|credential| credential.written_in(&handler.body))
            {
                continue;
            }
            let key = format!("{path}::{}", handler.name);
            census.push(key.clone());
            if !JOURNAL_CALLS
                .iter()
                .any(|journal| calls(&handler.body, journal))
            {
                silent.push(key);
            }
        }
    }

    // Liveness floor. The subject of this file is an absence, so a scan that
    // stopped recognising its own shapes would report a clean tree.
    assert!(
        census.len() >= 8,
        "only {} credential endpoint(s) were found ({census:?}); the census, not the tree, is \
         what changed",
        census.len()
    );

    let held: Vec<&str> = AWAITING_A_CARD.iter().map(|(key, _)| *key).collect();
    for (key, reason) in AWAITING_A_CARD {
        assert!(
            census.iter().any(|found| found == key),
            "the hold list keeps `{key}` ({reason}), but no handler of that name writes a \
             credential any more — delete the entry rather than leave a note about code that \
             is gone"
        );
        assert!(
            silent.iter().any(|found| found == key),
            "`{key}` now writes a journal entry, so the hold ({reason}) is stale — remove it, \
             and this rule starts guarding that handler like every other"
        );
    }

    let offenders: Vec<&String> = silent
        .iter()
        .filter(|key| !held.contains(&key.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "{} endpoint(s) mint or revoke a credential without journalling it:\n{}",
        offenders.len(),
        offenders
            .iter()
            .map(|key| format!(
                "  {key} — resolve the actor with `access_audit::grant_actor` before the write \
                 and call `access_audit::record_credential` (or `record_grant`, for a \
                 repository-scoped secret) after it. The name, the scopes, the fingerprint — \
                 never the secret, its ciphertext, or its hash."
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
