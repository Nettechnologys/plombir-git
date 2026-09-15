#!/usr/bin/env node

// Census every production primitive that can move a Git ref without going
// through another ForgeKeep function.  A new mover must declare which policy
// owns it; the three user-facing server-side commit producers additionally
// have to carry `ServerSideCommitPolicy` to the created commit.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode, productionRustSource } from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = process.env.FORGEKEEP_SERVER_SIDE_REF_POLICY_ROOT
  ? path.resolve(process.env.FORGEKEEP_SERVER_SIDE_REF_POLICY_ROOT)
  : path.resolve(scriptsDir, '..');
const cratesDir = path.join(root, 'crates');

const CLASSIFIED_MOVERS = new Map(
  [
    ['crates/rg-git/src/protocol/receive_pack.rs', 'update_ref', 'gix:reference', 'canonical_receive_pack'],
    ['crates/rg-core/src/repo/service.rs', 'auto_init_repo', 'git:push', 'repository_initialization'],
    ['crates/rg-core/src/repo/service.rs', 'push_branch_with_lease', 'git:push', 'leased_server_side_publish'],
    ['crates/rg-core/src/repo/service.rs', 'set_bare_repo_head_to_branch', 'gix:edit_reference', 'head_metadata'],
    ['crates/rg-core/src/pull_request/service.rs', 'git_rebase_merge', 'git:push', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_set_head_to_branch_with_repo', 'gix:edit_reference', 'head_metadata'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_fast_forward_with_repo', 'gix:reference', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_delete_ref', 'gix:edit_reference', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'cleanup_merge_group_ref', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'retire_losing_merge_group_pipeline', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'publish', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'drop', 'git:update-ref', 'internal_merge_queue_ref'],
  ].map(([file, symbol, primitive, policy]) => [
    `${file}:${symbol}:${primitive}`,
    { file, symbol, primitive, policy },
  ]),
);

const SERVER_SIDE_POLICY_PRODUCERS = [
  ['crates/rg-core/src/repo/service.rs', 'create_or_update_file'],
  ['crates/rg-core/src/repo/service.rs', 'update_files_in_commit'],
  ['crates/rg-core/src/repo/service.rs', 'delete_file'],
].map(([file, symbol]) => ({ file, symbol }));

function rustFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (entry.name.startsWith('.') || entry.name === 'target' || entry.name === 'tests') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) out.push(full);
  }
  return out;
}

function relative(file) {
  return path.relative(root, file).split(path.sep).join('/');
}

function closingDelimiter(code, open, opening, closing) {
  let depth = 0;
  for (let index = open; index < code.length; index += 1) {
    if (code[index] === opening) depth += 1;
    else if (code[index] === closing) {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return null;
}

function functionRanges(code) {
  const ranges = [];
  const declaration = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>{;]*>)?\s*\(/g;
  let match;
  while ((match = declaration.exec(code)) !== null) {
    const openParen = code.indexOf('(', match.index);
    const closeParen = closingDelimiter(code, openParen, '(', ')');
    if (closeParen === null) continue;
    const openBrace = code.indexOf('{', closeParen + 1);
    const semicolon = code.indexOf(';', closeParen + 1);
    if (openBrace < 0 || (semicolon >= 0 && semicolon < openBrace)) continue;
    const closeBrace = closingDelimiter(code, openBrace, '{', '}');
    if (closeBrace === null) continue;
    ranges.push({ symbol: match[1], start: match.index, end: closeBrace + 1 });
  }
  return ranges;
}

function ownerAt(ranges, index) {
  return ranges
    .filter((range) => index >= range.start && index < range.end)
    .sort((left, right) => right.start - left.start)[0] ?? null;
}

function callArguments(source, code, open) {
  const close = closingDelimiter(code, open, '(', ')');
  if (close === null) return null;
  const args = [];
  let start = open + 1;
  let depth = 0;
  for (let index = open + 1; index < close; index += 1) {
    const character = code[index];
    if ('([{'.includes(character)) depth += 1;
    else if (')]}'.includes(character)) depth -= 1;
    else if (character === ',' && depth === 0) {
      args.push(source.slice(start, index).trim());
      start = index + 1;
    }
  }
  if (depth !== 0) return null;
  args.push(source.slice(start, close).trim());
  return args;
}

function discoverMovers(file) {
  const bytes = readFileSync(file, 'utf8');
  const code = productionRustCode(bytes);
  const source = productionRustSource(bytes);
  const ranges = functionRanges(code);
  const movers = [];

  const record = (index, primitive) => {
    const owner = ownerAt(ranges, index);
    movers.push({
      file: relative(file),
      symbol: owner?.symbol ?? '<outside-function>',
      primitive,
      line: code.slice(0, index).split('\n').length,
      body: owner ? source.slice(owner.start, owner.end) : '',
    });
  };

  const runCall = /\.(?:run|run_with_env|run_with_env_removed|run_or_bail|spawn_async)\s*\(/g;
  for (const match of code.matchAll(runCall)) {
    const open = code.indexOf('(', match.index);
    const args = callArguments(source, code, open);
    const command = args?.[0]?.match(/^&\s*\[\s*"(push|update-ref)"/s)?.[1];
    if (command) record(match.index, `git:${command}`);
  }

  const gixCall = /\.(edit_reference|reference)\s*\(/g;
  for (const match of code.matchAll(gixCall)) {
    record(match.index, `gix:${match[1]}`);
  }

  return movers;
}

function functionBody(file, symbol) {
  const bytes = readFileSync(path.join(root, file), 'utf8');
  const code = productionRustCode(bytes);
  const source = productionRustSource(bytes);
  const matches = functionRanges(code).filter((range) => range.symbol === symbol);
  if (matches.length !== 1) return null;
  return source.slice(matches[0].start, matches[0].end);
}

const files = rustFiles(cratesDir).filter((file) => /^crates\/rg-[^/]+\//.test(relative(file)));
const movers = files.flatMap(discoverMovers);
const failures = [];
const seen = new Set();

for (const mover of movers) {
  const key = `${mover.file}:${mover.symbol}:${mover.primitive}`;
  const classification = CLASSIFIED_MOVERS.get(key);
  if (!classification) {
    failures.push(
      `Unclassified production ref mover ${mover.file}:${mover.line} (${mover.symbol}, ${mover.primitive}).`,
    );
    continue;
  }
  seen.add(key);

  if (classification.policy === 'leased_server_side_publish') {
    if (!mover.body.includes('--force-with-lease=')) {
      failures.push(`${mover.file}:${mover.symbol} does not enforce an explicit ref lease.`);
    }
    if (!mover.body.includes('validate_edit_branch')) {
      failures.push(`${mover.file}:${mover.symbol} publishes a branch without validating its name.`);
    }
  }
}

for (const producer of SERVER_SIDE_POLICY_PRODUCERS) {
  const body = functionBody(producer.file, producer.symbol);
  if (body === null) {
    failures.push(
      `Server-side policy producer disappeared or is ambiguous: ${producer.file}:${producer.symbol}.`,
    );
    continue;
  }
  if (!body.includes('ServerSideCommitPolicy')) {
    failures.push(`${producer.file}:${producer.symbol} no longer carries ServerSideCommitPolicy.`);
  }
  if (!body.includes('verify_created_commit')) {
    failures.push(
      `${producer.file}:${producer.symbol} no longer verifies the created commit before publishing.`,
    );
  }
  if (!body.includes('push_branch_with_lease')) {
    failures.push(
      `${producer.file}:${producer.symbol} no longer publishes through push_branch_with_lease.`,
    );
  }
}

for (const [key, classification] of CLASSIFIED_MOVERS) {
  if (!seen.has(key)) {
    failures.push(
      `Classified ref mover disappeared or changed shape: ${classification.file}:${classification.symbol} (${classification.primitive}, ${classification.policy}).`,
    );
  }
}

if (movers.length < CLASSIFIED_MOVERS.size) {
  failures.push(
    `Ref-mover census found only ${movers.length}; expected at least ${CLASSIFIED_MOVERS.size}.`,
  );
}

if (failures.length > 0) {
  for (const failure of failures) console.log(`FAIL ${failure}`);
  process.exit(1);
}

const byPolicy = new Map();
for (const classification of CLASSIFIED_MOVERS.values()) {
  byPolicy.set(classification.policy, (byPolicy.get(classification.policy) ?? 0) + 1);
}
console.log(
  `Server-side ref policy census ok: ${movers.length} mover(s), ` +
    [...byPolicy]
      .map(([policy, count]) => `${policy}=${count}`)
      .join(', '),
);
