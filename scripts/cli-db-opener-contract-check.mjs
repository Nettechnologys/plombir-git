#!/usr/bin/env node

// Every production database opener in `crates/rg-cli/src` must go through
// `crate::dbconn`.
//
// `dbconn` is where a subcommand's connection acquires the two things the raw
// `rg_db` openers cannot give it: the process lease that keeps an offline-only
// command away from a live server, and — since card_8baddb74fa82 — the check
// that refuses a file-backed SQLite database which is not there instead of
// creating an empty one. The default URL is relative, so a command run from
// the wrong directory otherwise addresses a brand new empty database and every
// downstream step succeeds against it. `backup-db` did exactly that on a live
// instance: it called `rg_db::connect` directly, VACUUMed an empty database
// into the backup file, and printed `Backup written`.
//
// The module doc-comment on `dbconn.rs` has said "the one place any subcommand
// opens the database" since it was written, and `backup-db` was outside it for
// as long. A sentence is not a gate; this is.
//
// SECOND QUESTION, same subject one level in: which commands open an *ordinary*
// pool — no lease — against an instance that may be live, and does each of them
// still have a reason.
//
// The card that asked for this (card_8ef8170e1f5b) left the design open: a
// source guard reading a comment next to the call site, or a reason carried by
// the code itself. The comment version is the one that already failed —
// `card_e069d60b4142` recognised the shape and fixed `migrate`, nobody swept
// the neighbours, and `rebuild-fts` and `rotate-encryption-key` stayed on an
// ordinary pool for months. Prose beside a call site says whatever it said when
// it was written, and a copied call brings the neighbour's prose with it.
//
// So the reason is typed: `dbconn::connect_online` takes an `OnlineAccess`
// variant, and a command physically cannot reach an unleased pool without
// naming which answer applies. rustc enforces THAT, which is why this check
// does not re-grep it. What rustc cannot ask is whether the *set* of such
// commands is still the set somebody thought about — copying a neighbouring
// call brings a plausible variant with it. That is the inventory below: a new
// online opener is red until it is written down here with its command name.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(process.env.FORGEKEEP_CLI_DB_OPENER_ROOT ?? path.join(scriptsDir, '..'));
const cliSrc = path.join(root, 'crates/rg-cli/src');
const GATEWAY = 'crates/rg-cli/src/dbconn.rs';

/**
 * Every production site that opens an ordinary pool — no process lease — and
 * the command it belongs to.
 *
 * Keyed by file with a count, the same shape (and for the same reason) as the
 * sibling inventories in this directory: a file already listed cannot quietly
 * grow a second online opener for a different command.
 *
 * The `why` here is a summary; the load-bearing justification is the
 * `OnlineAccess` variant at the call site and its doc-comment in `dbconn.rs`.
 */
const ONLINE_POOL_COMMANDS = [
  {
    file: 'crates/rg-cli/src/commands.rs',
    sites: 2,
    commands: ['forgekeep rotate-instance-key', 'forgekeep index-repo'],
    why: 'one writes a single row through one statement; the other runs exactly what `POST /repos/{owner}/{repo}/ai/index` runs on request',
  },
  {
    file: 'crates/rg-cli/src/admin.rs',
    sites: 1,
    commands: ['forgekeep backup-db'],
    why: '`VACUUM INTO` reads the source and writes a different file, so it never asks for the source write lock',
  },
];
// The gateway's own openers. Fewer than this and the sweep is reading the
// wrong file (renamed, moved, restructured) — which is the way an absence-based
// gate goes quiet without going red.
const MIN_GATEWAY_OPENERS = 3;

if (!existsSync(cliSrc)) {
  console.error(`❌ CLI DB opener sweep found no ${GATEWAY.replace('/dbconn.rs', '')} to read.`);
  process.exit(1);
}

function rustFiles(dir) {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (entry.name.startsWith('.') || entry.name === 'target') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) files.push(full);
  }
  return files;
}

function relative(file) {
  return path.relative(root, file).split(path.sep).join('/');
}

function lineAt(source, index) {
  return source.slice(0, index).split('\n').length;
}

// `production` blanks `#[cfg(test)]` items: test code builds throwaway pools by
// the dozen and is governed by its own gate
// (`test-db-connect-timeout-contract-check.mjs`), not by this one.
const opener = /\brg_db\s*::\s*(connect(?:_with_timeouts|_with_pool)?)\s*\(/g;
// The same call spelled so the qualifier is gone by the time it is written.
const hiddenOpenerImports = [
  /\buse\s+rg_db\s*::\s*\*\s*;/g,
  /\buse\s+rg_db\s+as\s+[A-Za-z_][A-Za-z0-9_]*\s*;/g,
  /\bextern\s+crate\s+rg_db\s+as\s+[A-Za-z_][A-Za-z0-9_]*\s*;/g,
  /\buse\s+rg_db\s*::\s*connect(?:_with_timeouts|_with_pool)?\b[^;]*;/g,
  /\buse\s+rg_db\s*::\s*\{[^;}]*\bconnect(?:_with_timeouts|_with_pool)?\b[^;}]*\}\s*;/g,
];

const files = rustFiles(cliSrc);
const violations = [];
let gatewayOpeners = 0;

for (const file of files) {
  const production = productionRustCode(readFileSync(file, 'utf8'));
  const name = relative(file);
  const isGateway = name === GATEWAY;

  for (const match of production.matchAll(opener)) {
    if (isGateway) {
      gatewayOpeners += 1;
      continue;
    }
    violations.push({
      file,
      source: production,
      index: match.index,
      reason: `\`rg_db::${match[1]}\` opens the database outside \`dbconn\` — use \`dbconn::connect_online\` / \`dbconn::connect_offline_migration\` / \`dbconn::connect_offline_maintenance\` so the command gets the process lease (or names why it needs none) and the database-presence check`,
    });
  }

  if (isGateway) continue;
  for (const pattern of hiddenOpenerImports) {
    for (const match of production.matchAll(pattern)) {
      violations.push({
        file,
        source: production,
        index: match.index,
        reason: 'an rg_db opener is imported or aliased into scope, which hides it from this sweep',
      });
    }
  }
}

// ── Second question: the unleased openers ────────────────────────────
const onlineOpener = /\bdbconn\s*::\s*connect_online\s*\(/g;
const declaredOnline = new Map(ONLINE_POOL_COMMANDS.map((entry) => [entry.file, entry]));
const foundOnline = new Map();

for (const file of files) {
  const production = productionRustCode(readFileSync(file, 'utf8'));
  const name = relative(file);
  if (name === GATEWAY) continue;
  const sites = [...production.matchAll(onlineOpener)].length;
  if (sites > 0) foundOnline.set(name, sites);
}

for (const [name, sites] of [...foundOnline].sort()) {
  const entry = declaredOnline.get(name);
  if (entry === undefined) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} opens an ordinary pool (${sites} \`dbconn::connect_online\` site(s)) and is not in ONLINE_POOL_COMMANDS. A command that runs against a live instance holds SQLite's single write lock for as long as its work takes, and fewer than a dozen write paths in this tree retry contention — so "probably fine" is the answer that already cost months once. Write down the command and why it is safe, or give it one of the offline leases`,
    });
    continue;
  }
  if (entry.sites !== sites) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} holds ${sites} \`dbconn::connect_online\` site(s), ONLINE_POOL_COMMANDS says ${entry.sites} (${entry.commands.join(', ')}). A new command joined the unleased set, or one left it — either way the inventory has to say so`,
    });
  }
}

for (const entry of ONLINE_POOL_COMMANDS) {
  if (!foundOnline.has(entry.file)) {
    violations.push({
      file: path.join(root, entry.file),
      source: '',
      index: 0,
      reason: `ONLINE_POOL_COMMANDS names ${entry.file}, which opens no ordinary pool any more (was: ${entry.commands.join(', ')}). Drop the entry — one pointing at nothing is how this inventory stops being one`,
    });
  }
}

if (foundOnline.size === 0) {
  violations.push({
    file: path.join(root, GATEWAY),
    source: '',
    index: 0,
    reason: 'no `dbconn::connect_online` site was found anywhere in crates/rg-cli/src. Either the opener was renamed — in which case this half of the check is reading nothing — or the last unleased command went away and ONLINE_POOL_COMMANDS should be emptied deliberately',
  });
}

if (gatewayOpeners < MIN_GATEWAY_OPENERS) {
  console.error(
    `❌ CLI DB opener sweep saw ${gatewayOpeners} opener(s) in ${GATEWAY}, expected at least ` +
      `${MIN_GATEWAY_OPENERS}. The gateway is not where this check thinks it is, so a green ` +
      'result here would mean nothing.',
  );
  process.exit(1);
}

if (violations.length > 0) {
  for (const violation of violations) {
    // A violation with no source view is about a file rather than a line: the
    // inventory questions are answered per file, not per call.
    const where = violation.source === ''
      ? relative(violation.file)
      : `${relative(violation.file)}:${lineAt(violation.source, violation.index)}`;
    console.error(`❌ ${where}: ${violation.reason}`);
  }
  process.exit(1);
}

const onlineSites = [...foundOnline.values()].reduce((total, sites) => total + sites, 0);
console.log(
  `✅ CLI DB openers: ${files.length} production file(s) under crates/rg-cli/src open the database ` +
    `only through dbconn (${gatewayOpeners} opener(s) there); ${onlineSites} unleased opener(s) ` +
    'across the inventory, each named with its command.',
);
