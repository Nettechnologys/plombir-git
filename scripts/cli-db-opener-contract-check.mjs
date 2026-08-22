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

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(process.env.FORGEKEEP_CLI_DB_OPENER_ROOT ?? path.join(scriptsDir, '..'));
const cliSrc = path.join(root, 'crates/rg-cli/src');
const GATEWAY = 'crates/rg-cli/src/dbconn.rs';
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
      reason: `\`rg_db::${match[1]}\` opens the database outside \`dbconn\` — use \`dbconn::connect\` / \`dbconn::connect_offline_migration\` / \`dbconn::connect_offline_maintenance\` so the command gets the process lease and the database-presence check`,
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
    console.error(
      `❌ ${relative(violation.file)}:${lineAt(violation.source, violation.index)}: ${violation.reason}`,
    );
  }
  process.exit(1);
}

console.log(
  `✅ CLI DB openers: ${files.length} production file(s) under crates/rg-cli/src open the database ` +
    `only through dbconn (${gatewayOpeners} opener(s) there).`,
);
