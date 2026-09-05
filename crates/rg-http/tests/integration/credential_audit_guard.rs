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
//! credential row is built inside `rg-http/src/api` itself, so the shape is
//! right there in the file.
//!
//! ## Why every function, and not only the handlers
//!
//! This scan used to skip anything that was not `is_handler`, on the reasoning
//! above: the row is built in the handler, so read the handlers. That reasoning
//! holds for where the *row* is built and not for where the *function boundary*
//! falls. `find_or_create_sso_user` is a private helper one call below
//! `callback`, and both writes that create an external-identity link live in it
//! — because the decision it makes (is this a new link, or the same link
//! signing in again?) is its own and pulling it up into the handler would mean
//! threading that decision back out through a return type purely so a grep
//! could see it. So the create side of that credential was held by a behavioural
//! test and by nothing mechanical: move the `record_credential` out and only the
//! test went red (card_6f301a1a0b18).
//!
//! Reading every production function instead of only the handlers costs
//! nothing, needs no list of blessed helpers to keep in step with the tree, and
//! closes the hole for a helper nobody has written yet. Measured when it was
//! switched on: exactly one function in `rg-http/src/api/**` writes a credential
//! outside a handler body, and it journals.
//!
//! The journal has to sit in the *same* function as the write. That is the rule
//! handlers were already held to, and it is the one this file is willing to
//! impose on shape: a write and the record of it belong together. Where a
//! journal legitimately lives in a helper of its own, the helper is named in
//! [`JOURNAL_CALLS`] — see `journal_password_reset`.
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

const CREDENTIALS: [Credential; 13] = [
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
    // A runner token can read the CI secrets injected into jobs of its one
    // repository. It is still a long-lived machine credential administered at
    // the instance level, and it was once the one credential in the tree with
    // no journal at all (card_2e514de7eefa, card_174154b4ee6c).
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
    // `stage_pending_totp_secret` — what `POST /users/mfa/setup` writes — is out
    // on purpose: it parks a secret nobody has confirmed in a slot no login
    // reads, so it protects nothing yet and destroys nothing either
    // (card_08400088bb40). The enrolment that arms it, and the rotation that
    // retires the previous authenticator through the same call, are the row
    // above.
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
    // An external identity linked to an account is a way into that account
    // that needs no password of ours — the same event as enrolling a passkey —
    // and dropping somebody's link is a step of a takeover, not housekeeping.
    // Provider tokens are deliberately not persisted; the identity link itself
    // is the credential this rule guards (card_79c61ed60181).
    //
    // Both sides are reached: `unlink_oauth_account` is a handler, and the two
    // writes that create a link sit in `find_or_create_sso_user`, a private
    // helper one call below `callback`. That helper is why this scan reads every
    // production function rather than only the handlers — see the module header.
    Credential {
        entity: Some("oauth_account"),
        ops: "oauth_account_ops",
        // `link` replaced the old `upsert` when first-login callbacks became
        // deletion-safe. `touch_existing` is deliberately absent: using an
        // existing credential is not minting a new one.
        verbs: &["link", "delete"],
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

struct CredentialSite {
    key: String,
    below_a_handler: bool,
    silent: bool,
}

/// Every credential-writing production function in one source file.
///
/// Factored out so the non-handler reach is proven by a source fixture instead
/// of by requiring today's tree to retain a private credential writer forever.
fn credential_sites(path: &str, text: &str) -> Vec<CredentialSite> {
    let mut sites = Vec::new();
    for function in functions(text) {
        if !CREDENTIALS
            .iter()
            .any(|credential| credential.written_in(&function.body))
        {
            continue;
        }
        sites.push(CredentialSite {
            key: format!("{path}::{}", function.name),
            below_a_handler: !function.is_handler,
            silent: !JOURNAL_CALLS
                .iter()
                .any(|journal| calls(&function.body, journal)),
        });
    }
    sites
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
        for site in credential_sites(&path, &text) {
            census.push(site.key.clone());
            if site.silent {
                silent.push(site.key);
            }
        }
    }

    // Liveness floor. The subject of this file is an absence, so a scan that
    // stopped recognising its own shapes would report a clean tree.
    assert!(
        census.len() >= 20,
        "only {} credential write site(s) were found ({census:?}); the census, not the tree, is \
         what changed",
        census.len()
    );

    let held: Vec<&str> = AWAITING_A_CARD.iter().map(|(key, _)| *key).collect();
    for (key, reason) in AWAITING_A_CARD {
        assert!(
            census.iter().any(|found| found == key),
            "the hold list keeps `{key}` ({reason}), but no function of that name writes a \
             credential any more — delete the entry rather than leave a note about code that \
             is gone"
        );
        assert!(
            silent.iter().any(|found| found == key),
            "`{key}` now writes a journal entry, so the hold ({reason}) is stale — remove it, \
             and this rule starts guarding that write site like every other"
        );
    }

    let offenders: Vec<&String> = silent
        .iter()
        .filter(|key| !held.contains(&key.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "{} function(s) mint or revoke a credential without journalling it:\n{}",
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

/// The scan keeps reading private production helpers without relying on the
/// current tree to contain one.
#[test]
fn the_credential_scan_reads_past_the_handler_boundary() {
    let entity = CREDENTIALS
        .iter()
        .find_map(|credential| credential.entity)
        .expect("the credential inventory contains at least one row-backed credential");

    let silent =
        format!("async fn mint_credential() {{\n    let _row = {entity}::ActiveModel {{}};\n}}\n");
    let sites = credential_sites("fixture.rs", &silent);
    let [site] = sites.as_slice() else {
        panic!(
            "the private credential fixture produced {} sites, not one",
            sites.len()
        )
    };
    assert!(
        site.below_a_handler,
        "the private fixture was read as a handler, so this test cannot catch the old filter"
    );
    assert!(site.silent, "the silent private fixture was not reported");

    let journalled = format!(
        "async fn mint_credential() {{\n    let _row = {entity}::ActiveModel {{}};\n    \
         record_credential().await;\n}}\n"
    );
    let sites = credential_sites("fixture.rs", &journalled);
    let [site] = sites.as_slice() else {
        panic!(
            "the journalled private fixture produced {} sites, not one",
            sites.len()
        )
    };
    assert!(
        !site.silent,
        "a private credential writer that journals was reported as silent"
    );

    assert!(
        credential_sites("fixture.rs", "async fn read_credential() {}\n").is_empty(),
        "a function that writes no credential was counted as one"
    );
}

/// The SSO link write door that triggered `card_ba45b1b7fac4` stays both live
/// in production and classified as a credential mint.
#[test]
fn oauth_link_is_a_live_credential_write_but_touch_is_not() {
    let credential = CREDENTIALS
        .iter()
        .find(|credential| credential.ops == "oauth_account_ops")
        .expect("the credential inventory names OAuth account links");

    let mut files = Vec::new();
    rust_files(&workspace_crates().join("rg-http/src/api"), &mut files);
    let mut locations = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let path = crate_relative(&file);
        for function in functions(&text) {
            if calls(&function.body, "oauth_account_ops::link") {
                assert!(
                    credential.written_in(&function.body),
                    "{path}:{}::{} calls the live OAuth link write door, but the credential \
                     inventory does not recognise it",
                    function.line,
                    function.name
                );
                locations.push(format!("{path}:{}::{}", function.line, function.name));
            }
        }
    }
    assert!(
        !locations.is_empty(),
        "no production function calls `oauth_account_ops::link`; update this ratchet with the \
         replacement mint door"
    );
    assert!(
        !credential.written_in("async fn login() { oauth_account_ops::touch_existing().await; }"),
        "using an existing OAuth link was classified as minting a new credential"
    );
}
