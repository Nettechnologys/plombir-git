#!/usr/bin/env node

// Mutation stand for authz-gate-dialect-contract-check.mjs.
//
// A green repository proves only that no decision-named `pub fn` is orphaned
// today. It says nothing about whether the sweep would notice the next one —
// and a census that has stopped reading its subject the way the compiler does
// passes exactly the same way.
//
// Four properties of the census are pinned here, none of which the check had a
// fixture for — they were argued in a comment and never shown
// (card_d59ec1b23c68, card_8c0d1652e26c). It reads `productionRustCode`, which
// blanks a complete `#[cfg(test)]` item whatever its shape, blanks it *with
// spaces*, blanks literals too, and lexes a raw string before it counts a brace:
//
//   - shape: the local stripper this replaced skipped only `mod … { … }` by its
//     own comment, so `#[cfg(test)] pub async fn check_…` entered the census as
//     a production authz declaration and its test call sites counted as
//     callers;
//   - byte alignment: that stripper deleted the span instead of blanking it, so
//     every line below the first inline test module shifted, and the check
//     prints `file:line` — a diagnostic pointing at the wrong line sends the
//     reader looking for a declaration that is not there;
//   - literals: neither half of the sweep reads a value, so the code-only view
//     is the right one, and a diagnostic string spelling `check_repo_access(`
//     cannot answer for the caller a gate does not have;
//   - raw strings: that stripper tracked `"` only, so the inner quote of
//     `r#"… " …"#` ended its string state and the `}` in that literal's data
//     closed the test module — the sweep then read the rest of the file as
//     production and answered about the wrong declaration.
//
// Each case drives the real check over a private fixture tree and judges it by
// exit code and by what the diagnostic names, because a check that goes red for
// the wrong reason is not evidence about the reason it was written for.

import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'authz-gate-dialect-contract-check.mjs');

let failed = 0;

/** Run the real check over a fixture whose `crates/demo/src/lib.rs` is `body`. */
function runCase(name, { body, min = 1, expect }) {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-authz-dialect-'));
  try {
    mkdirSync(join(fixture, 'crates/demo/src'), { recursive: true });
    writeFileSync(join(fixture, 'crates/demo/src/lib.rs'), body);

    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: {
        ...process.env,
        FORGEKEEP_AUTHZ_DIALECT_ROOT: fixture,
        FORGEKEEP_AUTHZ_DIALECT_MIN: String(min),
      },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const red = result.status !== 0;

    if (red !== expect.red) {
      console.error(
        `❌ ${name}: expected the sweep to go ${expect.red ? 'red' : 'green'}, it went ${red ? 'red' : 'green'}:\n${output}`,
      );
      failed += 1;
      return;
    }
    for (const needle of expect.mentions ?? []) {
      if (!output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic never named ${needle}:\n${output}`);
        failed += 1;
        return;
      }
    }
    for (const needle of expect.silent ?? []) {
      if (output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic named ${needle}, which is not the defect:\n${output}`);
        failed += 1;
        return;
      }
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

/** The 1-based line `needle` starts on in `body`, so no number is hand-counted. */
function lineOf(body, needle) {
  const at = body.indexOf(needle);
  if (at < 0) throw new Error(`the fixture no longer contains ${needle}`);
  return body.slice(0, at).split('\n').length;
}

// A gate primitive whose only declaration is a `#[cfg(test)]` item is not a
// dialect anybody can adopt: it does not exist in the binary. The local
// stripper this check used to carry skipped `mod … {` blocks only, so a
// free-standing one entered the census — and, having no production caller,
// would have been reported as an orphaned dialect nobody can delete.
const FREE_STANDING_TEST_ITEM = `pub async fn require_repo_write(db: &Db) -> bool {
    true
}

pub async fn handler(db: &Db) {
    let _ = require_repo_write(db).await;
}

#[cfg(test)]
pub async fn check_fixture_access(db: &Db) -> bool {
    true
}
`;

runCase('a free-standing #[cfg(test)] declaration never enters the census', {
  body: FREE_STANDING_TEST_ITEM,
  expect: { red: false, silent: ['check_fixture_access'] },
});

// The other side of the same case, so the silence above is a decision rather
// than an accident: the identical declaration without the attribute is a
// production dialect, and it is reported.
runCase('the same declaration without the attribute is reported', {
  body: FREE_STANDING_TEST_ITEM.replace('#[cfg(test)]\n', ''),
  expect: { red: true, mentions: ['`check_fixture_access`'] },
});

// Byte alignment. The declaration sits *below* an inline test module whose body
// also carries a brace inside a literal, so a stripper that deleted the span —
// or that ended the module at the first `}` it saw — reports a line number that
// addresses nothing.
const AFTER_INLINE_TESTS = `#[cfg(test)]
mod tests {
    fn scaffold() {
        let brace_in_a_literal = "}";
        let _ = brace_in_a_literal;
    }
}

pub async fn check_orphan_access(db: &Db) -> bool {
    true
}
`;

runCase('a declaration below an inline test module is reported at its real line', {
  body: AFTER_INLINE_TESTS,
  expect: {
    red: true,
    mentions: [
      `demo/src/lib.rs:${lineOf(AFTER_INLINE_TESTS, 'pub async fn check_orphan_access')}`,
    ],
  },
});

// The rule the check exists for, stated from the caller side: a gate primitive
// reachable only from its own unit tests is precisely the shape it hunts, so a
// call inside a `#[cfg(test)]` item must not count as a caller.
runCase('a call site inside a test module is not a caller', {
  body: `pub async fn may_delete_release(db: &Db) -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn exercises_the_gate() {
        let _ = may_delete_release(db);
    }
}
`,
  expect: { red: true, mentions: ['`may_delete_release`'] },
});

// The same rule with the spelling that used to defeat it. The local stripper
// this check carried tracked `"` only, so the inner quote of `r#"… " …"#` ended
// its string state and the `}` sitting in that literal's DATA closed the test
// module. Everything below the raw string then read as production: the call
// answered for the gate, and the fixture declaration beside it entered the
// census. Both halves are asserted, because under that stripper the sweep still
// went red — at the wrong name (card_8c0d1652e26c).
runCase('a raw string carrying a quote and a brace does not end the test module', {
  body: `pub async fn may_rewrite_history(db: &Db) -> bool {
    true
}

#[cfg(test)]
mod tests {
    const DOC: &str = r#"the " } character"#;

    pub fn check_fixture_permission(db: &Db) -> bool {
        let _ = DOC;
        may_rewrite_history(db)
    }
}
`,
  expect: {
    red: true,
    mentions: ['`may_rewrite_history`'],
    silent: ['check_fixture_permission'],
  },
});

// A literal is not a caller. Both halves of this sweep read identifiers — a
// definition is `pub fn <name>`, a call is `<name>(` — so the census reads the
// code-only view, and one diagnostic string spelling the gate's name with a
// paren after it cannot answer for the caller the gate does not have.
runCase('a call spelled inside a string literal is not a caller', {
  body: `pub async fn can_force_push(db: &Db) -> bool {
    true
}

pub async fn handler(db: &Db) {
    tracing::warn!("can_force_push(db) refused the push");
}
`,
  expect: { red: true, mentions: ['`can_force_push`'] },
});

// The receiver. `\b<name>\s*\(` asked only how the call is SPELLED, so any
// same-named method of any other type answered for the gate — and the check
// only reddens at zero, so one phantom caller was enough to keep a dead dialect
// green. Here the gate is a free function, which a dot can never reach, and the
// only call in the tree is a private method of `Other` that happens to share
// its name.
runCase('a same-named method of another type is not a caller of a free gate', {
  body: `pub async fn check_repo_access(db: &Db) -> bool {
    true
}

struct Other;

impl Other {
    fn check_repo_access(&self) -> bool {
        true
    }
}

pub async fn handler(other: &Other) {
    let _ = other.check_repo_access();
}
`,
  expect: { red: true, mentions: ['`check_repo_access`'] },
});

// The same vector spelled with a qualifier instead of a dot. `Other::check_x(`
// names a type that declares no such method, which is the `Vec::new()` shape
// the sibling ratchet paid for: a qualifier is evidence about the type, not
// about the name.
runCase('a phantom type qualifier is not a caller', {
  body: `pub struct Gate;

impl Gate {
    pub fn check_branch_access(&self) -> bool {
        true
    }
}

struct Other;

pub fn handler() {
    let _ = Other::check_branch_access();
}
`,
  expect: { red: true, mentions: ['`Gate::check_branch_access`'] },
});

// The dead end this lock had to avoid, pinned so nobody re-walks it: rejecting
// every call written with a dot falsely accuses a `pub fn (&self, …)`, for
// which a dot is the ONLY legal spelling. `SsoUserInfo::check_identity_keys`
// and `CiJobClaims::has_repo_access` are exactly this shape on the live tree,
// and both must stay green.
runCase('a method gate called through self is a called gate', {
  body: `pub struct SsoUserInfo;

impl SsoUserInfo {
    pub fn check_identity_keys(&self) -> bool {
        true
    }

    pub fn resolve(&self) -> bool {
        self.check_identity_keys()
    }
}
`,
  expect: { red: false },
});

// And through a named receiver, which is how the second live one is called
// (`claims.has_repo_access(target_repo_id)`). One declaring type in the tree
// means there is nothing for the receiver to be confused with.
runCase('a method gate called through a named receiver is a called gate', {
  body: `pub struct CiJobClaims;

impl CiJobClaims {
    pub fn has_repo_access(&self, repo: i64) -> bool {
        let _ = repo;
        true
    }
}

pub fn handler(claims: &CiJobClaims) -> bool {
    claims.has_repo_access(7)
}
`,
  expect: { red: false },
});

// When two types DO declare the name, a bare receiver stops being evidence: it
// could be either. Only `self` inside an `impl` of the owning type is
// attributable, and here the call sits in an `impl Other`.
runCase('an ambiguous receiver does not answer for the colliding gate', {
  body: `pub struct Gate;

impl Gate {
    pub fn may_merge(&self) -> bool {
        true
    }
}

pub struct Other;

impl Other {
    fn may_merge(&self) -> bool {
        true
    }

    pub fn run(&self) -> bool {
        self.may_merge()
    }
}
`,
  expect: { red: true, mentions: ['`Gate::may_merge`'], silent: ['`Other::may_merge`'] },
});

// The other side of that case, so the rejection above is about the receiver and
// not about the collision: the same colliding pair with the call moved into an
// `impl Gate` is a caller, and neither gate is reported.
runCase('self inside the owning impl answers for the colliding gate', {
  body: `pub struct Gate;

impl Gate {
    pub fn may_merge(&self) -> bool {
        true
    }

    pub fn run(&self) -> bool {
        self.may_merge()
    }
}

pub struct Other;

impl Other {
    fn may_merge(&self) -> bool {
        true
    }
}
`,
  expect: { red: false },
});

// The anti-vacuous half: a census that stops recognising declarations has to
// say so rather than report a clean tree it can no longer read.
runCase('a census below the recognised-name floor is rejected', {
  body: FREE_STANDING_TEST_ITEM,
  min: 2,
  expect: { red: true, mentions: ['expected at least 2'] },
});

if (failed > 0) {
  console.error(`❌ authz-gate-dialect mutation stand: ${failed} case(s) failed`);
  process.exit(1);
}
console.log(
  '✅ authz-gate-dialect mutation stand: a `#[cfg(test)]` declaration stays out of the census, '
    + 'a production one does not, and a line number below an inline test module still addresses the file',
);
