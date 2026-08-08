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

import { stripRustComments } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
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
 * Remove `#[cfg(test)] mod … { … }` blocks.
 *
 * A gate primitive reachable only from its own unit tests is precisely the
 * shape this check is looking for — `check_push_allowed` had exactly that, and
 * counting its test call sites as callers would have hidden it. Files under
 * `crates/*&#47;tests/` are excluded wholesale for the same reason.
 */
function stripCfgTestModules(source) {
  let out = '';
  let i = 0;
  while (i < source.length) {
    const marker = source.indexOf('#[cfg(test)]', i);
    if (marker === -1) {
      out += source.slice(i);
      break;
    }
    out += source.slice(i, marker);
    // Only a `mod … {` block is skipped wholesale; `#[cfg(test)]` on a single
    // item is left in place, where the brace scan below would misread it.
    const rest = source.slice(marker);
    const head = /^#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{/.exec(rest);
    if (!head) {
      out += '#[cfg(test)]';
      i = marker + '#[cfg(test)]'.length;
      continue;
    }
    let depth = 1;
    let j = marker + head[0].length;
    while (j < source.length && depth > 0) {
      const ch = source[j];
      if (ch === '"') {
        j += 1;
        while (j < source.length) {
          if (source[j] === '\\') {
            j += 2;
            continue;
          }
          if (source[j] === '"') break;
          j += 1;
        }
      } else if (ch === '{') depth += 1;
      else if (ch === '}') depth -= 1;
      j += 1;
    }
    i = j;
  }
  return out;
}

/** `pub` fn definitions in `source` whose name promises a decision. */
function authzDefinitions(source, relative) {
  const rows = [];
  const re = /^[ \t]*pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+(\w+)\s*[(<]/gm;
  let match;
  while ((match = re.exec(source)) !== null) {
    if (!AUTHZ_NAME.test(match[1])) continue;
    rows.push({ name: match[1], file: relative, line: source.slice(0, match.index).split('\n').length });
  }
  return rows;
}

/**
 * Call sites of `name` in `source`, excluding its own definitions.
 *
 * A call is `name(`; a definition is the same shape preceded by `fn`. Matching
 * on the paren is what keeps `#[serde(default = "default_permission")]` — a
 * string, not a call — from reading as a caller, which is a real spelling in
 * this tree.
 */
function callSites(source, name) {
  const re = new RegExp(`\\b${name}\\s*\\(`, 'g');
  let count = 0;
  let match;
  while ((match = re.exec(source)) !== null) {
    if (/\bfn\s*$/.test(source.slice(0, match.index))) continue;
    count += 1;
  }
  return count;
}

const production = [];
for (const file of rustFiles(cratesDir)) {
  // `crates/<crate>/tests/**` is test code by layout; `#[cfg(test)] mod` is test
  // code by attribute. Neither counts as a caller.
  if (/^[^/]+\/tests\//.test(file.relative)) continue;
  production.push({ ...file, source: stripCfgTestModules(stripRustComments(readFileSync(file.path, 'utf8'))) });
}

const definitions = [];
for (const file of production) {
  // A definition inside `crates/<crate>/src/` only; benches and examples declare
  // nothing this check is about.
  if (!/^[^/]+\/src\//.test(file.relative)) continue;
  definitions.push(...authzDefinitions(file.source, file.relative));
}

const names = new Set(definitions.map((definition) => definition.name));
if (names.size < MIN_AUTHZ_NAMES) {
  console.error(
    `❌ Only ${names.size} decision-named pub fn(s) found across ${production.length} source file(s) ` +
      `(expected at least ${MIN_AUTHZ_NAMES}). The sweep is broken, not the tree — fix ` +
      'scripts/authz-gate-dialect-contract-check.mjs rather than lowering the floor.',
  );
  process.exit(1);
}

const orphaned = [];
for (const name of [...names].sort()) {
  let calls = 0;
  for (const file of production) calls += callSites(file.source, name);
  if (calls === 0) {
    orphaned.push(definitions.filter((definition) => definition.name === name));
  }
}

if (orphaned.length > 0) {
  for (const group of orphaned) {
    const where = group.map((definition) => `${definition.file}:${definition.line}`).join(', ');
    console.error(
      `❌ \`${group[0].name}\` (${where}) promises an access decision and no production code asks it. ` +
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
