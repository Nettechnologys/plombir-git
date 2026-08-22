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
    ///
    /// `None` for the one credential in the tree that has no row of its own:
    /// the TOTP factor lives as columns on `users` (`mfa_enabled`,
    /// `totp_secret`), so there is no entity to recognise and the ops verbs
    /// below are the whole shape. Leaving this as a required field is how the
    /// rule would have covered passkeys and said nothing about MFA
    /// (card_7aa2870dc1e0).
    entity: Option<&'static str>,
    /// `rg_db::ops::<ops>::<verb>…` — the module that owns the table. Matched on
    /// the module plus a mutating verb rather than on each function name, so a
    /// second spelling of "write it" or "remove it" cannot appear without this
    /// rule noticing. It is the only way `ci_secrets::put` is seen at all: it
    /// calls `ci_secret_ops::upsert`, which builds the row inside `rg-db`, so
    /// there is no `ActiveModel` in the handler to recognise.
    ops: &'static str,
    /// The verbs that mean "mint or revoke" **for this table**.
    ///
    /// [`MUTATING_VERBS`] for almost every one. `runner_ops` is the exception
    /// and the reason this is a field: its table carries the runner's liveness
    /// as well as its token, so `update_status` and `update_heartbeat` are
    /// ordinary bookkeeping done by `poll_job`, `finish_job` and the
    /// authentication middleware. Matching `update` there would accuse four
    /// handlers that mint nothing of failing to journal a credential, and a
    /// rule that cries wolf is one somebody eventually deletes.
    verbs: &'static [&'static str],
}

/// What a handler does to a credential table that is not reading it.
const MUTATING_VERBS: [&str; 4] = ["create", "upsert", "update", "delete"];

const CREDENTIALS: [Credential; 12] = [
    // A personal access token authenticates as the account it belongs to.
    Credential {
        entity: Some("access_token"),
        ops: "token_ops",
        verbs: &MUTATING_VERBS,
    },
    // An SSH key is the account's push credential from a given machine.
    Credential {
        entity: Some("ssh_key"),
        ops: "ssh_key_ops",
        verbs: &MUTATING_VERBS,
    },
    // A CI secret is a value every job of the repository can read, so anyone
    // who can push a branch can read it out.
    Credential {
        entity: Some("ci_secret"),
        ops: "ci_secret_ops",
        verbs: &MUTATING_VERBS,
    },
    // A deploy key with `read_only: false` is push access to one repository for
    // whoever holds the private half. Journalled as a *grant* rather than an
    // account credential — see the module header — which this rule accepts,
    // because `record_grant` is one of the spellings it recognises.
    Credential {
        entity: Some("deploy_key"),
        ops: "deploy_key_ops",
        verbs: &MUTATING_VERBS,
    },
    // A runner token is the widest of the five: the runner polls the queue,
    // takes a job from any repository whose labels it covers, and `poll_job`
    // decrypts that repository's CI secrets into the job's environment. So it
    // is read access to the secrets of every repository whose work it can
    // claim — and it was the one credential in the tree with no journal at all
    // (card_2e514de7eefa).
    Credential {
        entity: Some("runner"),
        ops: "runner_ops",
        verbs: &["register_runner", "deregister_runner"],
    },
    // A passkey is a way into the account that needs no password at all, so
    // enrolling one is the same event as adding an SSH key and removing one is
    // the same event as revoking it. `touch_and_update` — the last-used stamp
    // `login_finish` writes — is deliberately outside the verbs: it is a use of
    // the credential, not its appearance.
    Credential {
        entity: Some("passkey_credential"),
        ops: "passkey_credential_ops",
        verbs: &["create", "delete"],
    },
    // The second factor itself. It is not a credential that grants access — it
    // is what access is PROTECTED by, which makes switching it off the sharper
    // event of the two: the classic takeover is a stolen password, then
    // `disable_mfa`, then everything else. It has no table of its own, hence
    // the `None` above; `enable_mfa` also matches
    // `enable_mfa_with_backup_codes`, which is the spelling enrolment uses.
    // `update_totp_secret` — what `POST /users/mfa/setup` writes — is out on
    // purpose: a secret nobody has confirmed protects nothing yet, and the
    // enrolment that arms it is the row above.
    Credential {
        entity: None,
        ops: "user_ops",
        verbs: &["enable_mfa", "disable_mfa"],
    },
    // Backup codes are single-use passwords for the account, and re-issuing
    // them is interesting because the old set stops working — not because the
    // new one starts. `verify_and_consume` stays out for the same reason
    // `touch_and_update` does: spending a code is a use, not a re-issue.
    Credential {
        entity: Some("mfa_backup_code"),
        ops: "mfa_backup_code_ops",
        verbs: &["set_codes"],
    },
    // The account's password, and the one-time link that replaces it. The third
    // shape this rule has had to learn (card_80f1b25cf114): the hash lives in a
    // column of `users` and is rewritten inside `rg_core::user::service`, so the
    // handler carries neither an `ActiveModel` nor a `rg_db::ops` call — the two
    // things every entry above is recognised by. What it does carry is the
    // service function, which is why `ops` here names a service module rather
    // than a table.
    //
    // Both verbs, because both are credential events. `reset_password` replaces
    // the main secret of the account; `forgot_password` *issues* a single-use
    // way back in, and the journal used to hold the login on either side of it
    // and nothing in between — so a login from an unfamiliar address read as an
    // ordinary login.
    Credential {
        entity: None,
        ops: "user::service",
        verbs: &["reset_password", "forgot_password"],
    },
    // The HMAC key ForgeKeep signs every outgoing delivery with. Not a way into
    // this instance — the opposite: it is what a receiver decides by. Which is
    // why rotation is the quiet event of the two, and why the entry exists at
    // all: the receiver goes on trusting a signature made with a different key
    // (card_c0a0339b7191).
    //
    // Named by its service module for the same reason the password above is:
    // the secret is sealed inside `rg_core::webhook::service`, so the handler
    // carries neither an `ActiveModel` nor a `rg_db::ops` call.
    Credential {
        entity: None,
        ops: "webhook::service",
        verbs: &["create_webhook", "update_webhook", "delete_webhook"],
    },
    // Somebody else's password or access token, handed to this server so it can
    // pull from their remote. Compromise does not open this instance; the
    // question the phase asks — whose credentials does this server hold, and
    // when were they replaced — covers it exactly.
    Credential {
        entity: None,
        ops: "mirror::service",
        verbs: &["create_mirror", "update_mirror", "delete_mirror"],
    },
    // The widest of them all, and the one this rule had the shape for and not
    // the inventory (card_d03f5f4b6fc2). An SSO provider's `client_secret` is
    // the door every account on the instance logs in through, and
    // `ldap_bind_password` is a read account in somebody else's directory —
    // both already judged valuable enough to be encrypted at rest, while their
    // appearance was not an event. `find_by_slug` / `find_by_id` / `list_all`
    // stay outside the verbs: reading the row is how three login handlers work.
    Credential {
        entity: Some("sso_provider"),
        ops: "sso_provider_ops",
        verbs: &MUTATING_VERBS,
    },
];

/// Handlers held out of the rule, each with the card that closes the hold.
///
/// A ratchet, not an opt-out: the assertion below requires every entry here to
/// *still* be journalling nothing. An entry that has been fixed and not removed
/// is a lie about the tree, and it fails.
const AWAITING_A_CARD: [(&str, &str); 0] = [];

/// Every spelling of "a row was written to `audit_log`".
///
/// `journal_password_reset` is a private helper of `api::users` rather than a
/// shared recorder, and it is named here because the reset has two endings —
/// a session, or a second-factor challenge — that write the same row. Inlining
/// the record twice to satisfy a source rule would be the rule choosing the
/// shape of the code, which is how a guard starts costing more than it catches.
const JOURNAL_CALLS: [&str; 5] = [
    "record_credential",
    "record_grant",
    "record_instance_credential",
    "journal_password_reset",
    "record",
];

impl Credential {
    /// Whether `body` mints or revokes this credential.
    fn written_in(&self, body: &str) -> bool {
        let code = production_rust_code_only(body);
        let builds_the_row = self
            .entity
            .is_some_and(|entity| code.contains(&format!("{entity}::ActiveModel")));
        builds_the_row
            || self
                .verbs
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
        census.len() >= 15,
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
