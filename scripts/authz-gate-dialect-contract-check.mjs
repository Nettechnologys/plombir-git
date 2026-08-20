#!/usr/bin/env node

// Asserts that no `pub` function whose NAME PROMISES AN ACCESS DECISION sits in
// the tree without a production caller.
//
// Why this exists: four such functions have now been found and deleted — the
// OCI `check_repo_access`, `branch_protection::check_push_allowed`,
// `collaborator::get_effective_permission`, `review::check_approval_status`
// (card_1e448747cd6b, card_ab36709fa0c7). Each was a *second dialect* of a gate
// whose live implementation lived elsewhere and had drifted away from it. None
// of them was a bug on its own — nothing called them. The damage is the
// invitation: the next person adding a push path, or an "am I allowed" branch,
// greps for a plausible name, finds one, wires it up, and now the forge enforces
// two different policies depending on which door you knock at.
//
// A one-off sweep cannot hold that shut, because the fifth dialect appears
// exactly as quietly as the first four did. This is the ratchet.
//
// Scope, deliberately narrow on both axes:
//
//   - `pub` only. A private fn with no caller is already a `dead_code` warning,
//     and the workspace denies warnings; the compiler covers that half. What it
//     cannot see is a `pub` item in a library crate, which is exactly where all
//     four lived.
//   - The name has to promise a DECISION (`check_`/`may_`/`can_`/`require_`
//     prefix, or a `_access`/`_permission` suffix). Not "touches authz" — the
//     point is the invitation a name extends to the next caller. `_permissions`
//     plural is out on purpose: it reads as file modes (`get_permissions`), not
//     as an access verdict.
//
// A hit is not automatically "delete it". It is "this promises a decision and
// nothing asks it" — answered either by deleting the function or by wiring the
// live path through it. What must not happen is the third option, silence.

import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));

// The mutation stand points this at a fixture tree. Everything below is
// relative to it, so the stand exercises the real sweep rather than a copy of
// it — which is the only way a fixture can say anything about this file.
const override = process.env.FORGEKEEP_AUTHZ_DIALECT_ROOT;
const root = override ? resolve(override) : resolve(scriptsDir, '..');
const cratesDir = resolve(root, 'crates');

/** A name that promises an access decision to whoever reads it next. */
const AUTHZ_NAME = /^(?:check|may|can|require)_\w+$|_(?:access|permission)$/;

/**
 * Lower bound on a healthy sweep. The workspace carries ~30 such names
 * (`require_write`, `can_read_repo`, `check_merge_allowed`, …). A parser that
 * silently understands nothing would report "no orphaned dialects" over a tree
 * it never read — which is worse than red, because nobody investigates green.
 */
const MIN_AUTHZ_NAMES = 20;

// The stand drives a fixture holding a handful of names, so it sets its own
// floor — and only it: without the root override the workspace number is not
// negotiable, or the floor becomes an environment variable away from useless.
const minAuthzNames = override
  ? Number(process.env.FORGEKEEP_AUTHZ_DIALECT_MIN ?? 0)
  : MIN_AUTHZ_NAMES;

/** Every `.rs` file under `crates/`, as `{ path, relative }`. */
function rustFiles(dir, relative = '') {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith('.') || entry.name === 'target') continue;
    const next = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...rustFiles(join(dir, entry.name), next));
    else if (entry.isFile() && entry.name.endsWith('.rs')) out.push({ path: join(dir, entry.name), relative: next });
  }
  return out;
}

/**
 * `pub` fn definitions in `source` whose name promises a decision, each with
 * the type that declares it — `null` for a free function.
 *
 * The owner is part of the subject, not decoration: a free function and a
 * method that share a name are two different gates, reached by two different
 * spellings, and each needs a caller of its own.
 */
function authzDefinitions(source, relative) {
  const rows = [];
  const blocks = implBlocks(source);
  const re = /^[ \t]*pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+(\w+)\s*[(<]/gm;
  let match;
  while ((match = re.exec(source)) !== null) {
    if (!AUTHZ_NAME.test(match[1])) continue;
    const block = blocks
      .filter((candidate) => match.index > candidate.open && match.index < candidate.end)
      .sort((a, b) => b.open - a.open)[0];
    rows.push({
      name: match[1],
      owner: block === undefined ? null : block.type,
      file: relative,
      line: source.slice(0, match.index).split('\n').length,
    });
  }
  return rows;
}

const IDENT = '[A-Za-z_][A-Za-z0-9_]*';

/** `text` with every balanced `<…>` group removed. */
function stripGenerics(text) {
  let out = '';
  let depth = 0;
  for (const ch of text) {
    if (ch === '<') depth += 1;
    else if (ch === '>') depth = Math.max(0, depth - 1);
    else if (depth === 0) out += ch;
  }
  return out;
}

/** The index just past the `}` closing the block opened at `open`. */
function blockEnd(code, open) {
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === '{') depth += 1;
    else if (code[i] === '}') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return code.length;
}

/**
 * The `impl` blocks `source` declares, with the type each one is about.
 *
 * The self-type is what follows `for` when the block implements a trait and
 * what follows `impl` otherwise; generic parameters and a `where` clause name
 * no type of their own, so both are dropped before the last path segment is
 * read. An `impl` in RETURN position (`-> impl Iterator<…>`) opens no block and
 * is told apart by what precedes it on its line.
 *
 * Lifted from `scripts/rust-source-view-contract-check.mjs`, where the same
 * parse settled the same question for the same reason (card_53a95b2b6217): a
 * method is not identified by its name but by the pair `Type::method`.
 */
function implBlocks(source) {
  const out = [];
  const keyword = /\bimpl\b/g;
  for (let m = keyword.exec(source); m !== null; m = keyword.exec(source)) {
    const lineStart = source.lastIndexOf('\n', m.index) + 1;
    if (!/^\s*(?:unsafe\s+)?$/.test(source.slice(lineStart, m.index))) continue;
    const open = source.indexOf('{', m.index);
    if (open < 0) continue;
    const header = stripGenerics(source.slice(m.index + 'impl'.length, open)).replace(/\bwhere\b[\s\S]*$/, '');
    let target = header;
    const trait = /\bfor\b(?![A-Za-z0-9_])/g;
    for (let f = trait.exec(header); f !== null; f = trait.exec(header)) {
      target = header.slice(f.index + 'for'.length);
    }
    const segments = target.match(new RegExp(IDENT, 'g'));
    if (segments === null) continue;
    const type = segments[segments.length - 1];
    // A lowercase tail is a module path or a primitive, not a type spelled the
    // way a `Type::method` qualifier is.
    if (!/^[A-Z]/.test(type)) continue;
    out.push({ type, open, end: blockEnd(source, open) });
  }
  return out;
}

/**
 * Every locally implemented type that declares each `fn` name, over the whole
 * production tree.
 *
 * This is what tells an ambiguous `receiver.name(…)` apart from an unambiguous
 * one, so visibility is not asked: a PRIVATE method of some other type is
 * exactly the spelling that used to answer for a gate nobody calls, and it has
 * to be visible to this map. Declarations outside any `impl` block are free
 * functions and belong to no type, so they are simply not recorded here.
 */
function declarationIndex(files) {
  const methods = new Map();
  const declaration = new RegExp(
    `(?:^|[\\s;{}])(?:pub(?:\\([^)]*\\))?\\s+)?(?:const\\s+)?(?:async\\s+)?(?:unsafe\\s+)?fn\\s+(\\w+)\\s*[(<]`,
    'gm',
  );
  for (const file of files) {
    const blocks = implBlocks(file.source);
    for (let m = declaration.exec(file.source); m !== null; m = declaration.exec(file.source)) {
      const block = blocks
        .filter((candidate) => m.index > candidate.open && m.index < candidate.end)
        .sort((a, b) => b.open - a.open)[0];
      if (block === undefined) continue;
      if (!methods.has(m[1])) methods.set(m[1], new Set());
      methods.get(m[1]).add(block.type);
    }
  }
  return { methods };
}

/**
 * Call sites in `source` that could be reaching the declaration of `name`
 * owned by `owner` (`null` for a free function), excluding its own definitions.
 *
 * A call is `name(`; a definition is the same shape preceded by `fn`. Matching
 * on the paren is what keeps `#[serde(default = "default_permission")]` — a
 * string, not a call — from reading as a caller, which is a real spelling in
 * this tree.
 *
 * The receiver decides the rest, because `\b${name}\s*\(` alone did not ask
 * it: any `whatever.check_repo_access(…)` on any type in the tree answered for
 * the gate nobody calls, and the check only reddens at zero. That is the same
 * collision the sibling ratchet paid for twice — `Vec::new()` making a
 * normalizer of every builder (`sol_4d15ddc4996e`), then a bare-name key
 * laundering `ActionTemplate::literal` (`sol_7c92dc0eacb9`) — and it is settled
 * here the way it was settled there: on the pair `Type::method`, not the name.
 *
 * Spelling by spelling:
 *
 *   - bare `name(` and `module::name(` (lowercase qualifier — Rust spells a
 *     module in snake_case) reach a FREE function and nothing else: an inherent
 *     method cannot be called that way.
 *   - `Type::name(` reaches the method of that exact type, `Self::name(` the
 *     method of the enclosing `impl`. A qualifier naming a type that declares
 *     no such method is a phantom caller.
 *   - `receiver.name(` reaches a method, and WHICH one is a question a regex
 *     cannot answer. When exactly one type in the tree declares the name there
 *     is nothing to confuse it with, so the call counts. When two or more do,
 *     only `self.name(` inside an `impl` of the owning type is attributable —
 *     any other receiver is precisely the ambiguity this lock exists to refuse.
 *
 * Deliberately NOT locked on the spelling alone: `(?<![.\w])` — reject every
 * call written with a dot — was tried and falsely accused two live gates,
 * `SsoUserInfo::check_identity_keys` and `CiJobClaims::has_repo_access`. Both
 * are `pub fn (&self, …)`, for which a dot is the only legal spelling. The lock
 * has to know the receiver's type, not the shape of the call.
 */
function callSites(source, name, owner, index) {
  const types = index.methods.get(name) ?? new Set();
  const blocks = implBlocks(source);
  const implTypeAt = (at) => {
    const block = blocks
      .filter((candidate) => at > candidate.open && at < candidate.end)
      .sort((a, b) => b.open - a.open)[0];
    return block === undefined ? null : block.type;
  };

  const re = new RegExp(`(?<![A-Za-z0-9_])${name}\\s*\\(`, 'g');
  let count = 0;
  for (let m = re.exec(source); m !== null; m = re.exec(source)) {
    const before = source.slice(0, m.index);
    if (/\bfn\s*$/.test(before)) continue;

    if (/::\s*$/.test(before)) {
      const qualifier = /(\w+)\s*::\s*$/.exec(before);
      // `<T as Trait>::name(` names no segment this reader can weigh; counting
      // it keeps the check from reddening over a spelling it does not parse.
      if (qualifier === null) count += 1;
      else if (!/^[A-Z]/.test(qualifier[1])) count += owner === null ? 1 : 0;
      else if (qualifier[1] === 'Self') count += implTypeAt(m.index) === owner ? 1 : 0;
      else count += qualifier[1] === owner ? 1 : 0;
      continue;
    }

    if (/\.\s*$/.test(before)) {
      if (owner === null) continue;
      if (types.size === 1) count += 1;
      else if (/(?<![A-Za-z0-9_])self\s*\.\s*$/.test(before)) {
        count += implTypeAt(m.index) === owner ? 1 : 0;
      }
      continue;
    }

    count += owner === null ? 1 : 0;
  }
  return count;
}

const production = [];
for (const file of rustFiles(cratesDir)) {
  // `crates/<crate>/tests/**` is test code by layout; a `#[cfg(test)]` item is
  // test code by attribute. Neither counts as a caller — a gate primitive
  // reachable only from its own unit tests is precisely the shape this check
  // hunts (`check_push_allowed` had exactly that), so counting its test call
  // sites as callers would hide it.
  //
  // This used to run a local `stripCfgTestModules`, which by its own comment
  // skipped only `mod … { … }` blocks and left `#[cfg(test)]` on a single item
  // standing. The shared view blanks the whole item whatever its shape, and it
  // blanks it with spaces, so the reported line numbers still address the
  // original file — the local one deleted the span and shifted every line under
  // the first inline test module.
  //
  // The code-only twin rather than the string-bearing one, because neither half
  // of this sweep reads a value: a definition is `pub fn <name>` and a call is
  // `<name>(`, both identifiers. What a literal CAN do is manufacture a caller —
  // one diagnostic string spelling `check_repo_access(` would answer for the
  // gate nobody calls, which is the exact false green this check exists to
  // refuse.
  if (/^[^/]+\/tests\//.test(file.relative)) continue;
  production.push({ ...file, source: productionRustCode(readFileSync(file.path, 'utf8')) });
}

const definitions = [];
for (const file of production) {
  // A definition inside `crates/<crate>/src/` only; benches and examples declare
  // nothing this check is about.
  if (!/^[^/]+\/src\//.test(file.relative)) continue;
  definitions.push(...authzDefinitions(file.source, file.relative));
}

const names = new Set(definitions.map((definition) => definition.name));
if (names.size < minAuthzNames) {
  console.error(
    `❌ Only ${names.size} decision-named pub fn(s) found across ${production.length} source file(s) ` +
      `(expected at least ${minAuthzNames}). The sweep is broken, not the tree — fix ` +
      'scripts/authz-gate-dialect-contract-check.mjs rather than lowering the floor.',
  );
  process.exit(1);
}

// Which type owns which `fn`, read once over the whole production tree: a
// method is identified by the pair `Type::method`, and the answer is not a
// property of the file the call site happens to sit in.
const declarations = declarationIndex(production);

// Grouped by the PAIR, so a free function and a method sharing a name are two
// subjects and each has to be asked for separately. Two declarations of the
// same kind stay one subject: which module path a bare call resolves to is a
// question about imports, not about receivers, and this check does not answer
// it.
const subjects = new Map();
for (const definition of definitions) {
  const key = `${definition.name}\u0000${definition.owner ?? ''}`;
  if (!subjects.has(key)) subjects.set(key, []);
  subjects.get(key).push(definition);
}

const orphaned = [];
for (const key of [...subjects.keys()].sort()) {
  const group = subjects.get(key);
  let calls = 0;
  for (const file of production) calls += callSites(file.source, group[0].name, group[0].owner, declarations);
  if (calls === 0) orphaned.push(group);
}

if (orphaned.length > 0) {
  for (const group of orphaned) {
    const where = group.map((definition) => `${definition.file}:${definition.line}`).join(', ');
    const spelled = group[0].owner === null ? group[0].name : `${group[0].owner}::${group[0].name}`;
    console.error(
      `❌ \`${spelled}\` (${where}) promises an access decision and no production code asks it. ` +
        'Delete it, or wire the live path through it — a second dialect of a gate is adopted by the ' +
        'next caller who greps for a plausible name.',
    );
  }
  process.exit(1);
}

console.log(
  `authz gate dialects: ${names.size} decision-named pub fn(s) across ${production.length} production ` +
    'source file(s), every one of them called',
);
