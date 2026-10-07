#!/usr/bin/env node

// Census every production primitive that can move a Git ref without going
// through another Plombir Git function.  A new mover must declare which policy
// owns it; the three user-facing server-side commit producers additionally
// have to carry `ServerSideCommitPolicy` to the created commit.
//
// "Can move a ref" is wider than `push` and `update-ref` (card_4f51d61a783f).
// The census used to see only those two and gix `reference` /
// `edit_reference`, and stayed green while missing the path nearly every merge
// takes — gix `commit_as`, which writes the commit *and* moves the branch —
// as well as the `fetch` that publishes a pull mirror's branches (pruning the
// rest), the fork fetch into `refs/forks/*`, `clone` (a fork and an import
// create a repository with every branch and tag at once) and `symbolic-ref
// HEAD`. A mover outside the census is a mover outside every policy: the
// read-only mirror and the server-side LFS lock checks both rest on knowing
// each one.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode, productionRustSource } from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = process.env.PLOMBIR_GIT_SERVER_SIDE_REF_POLICY_ROOT
  ? path.resolve(process.env.PLOMBIR_GIT_SERVER_SIDE_REF_POLICY_ROOT)
  : path.resolve(scriptsDir, '..');
const cratesDir = path.join(root, 'crates');

// git subcommands that write refs in the repository they run in, or — `clone`
// — create one with refs. Worktree-only commands (`commit`, `checkout`,
// `rebase`, `merge`) are left out: they cannot run in a bare served repository,
// and the scratch clones they run in are published only through `push`.
const GIT_REF_COMMANDS = [
  'push',
  'update-ref',
  'fetch',
  'clone',
  'symbolic-ref',
  'remote',
  'branch',
  'tag',
  'replace',
  'reset',
  'notes',
  'pull',
];

// gix methods that move a ref. `commit` / `commit_as` / `tag` write an object
// and then update the named reference; the arity floor keeps sea-orm's
// zero-argument `transaction.commit()` out.
const GIX_REF_METHODS = new Map([
  ['reference', 1],
  ['edit_reference', 1],
  ['edit_references', 1],
  ['tag_reference', 1],
  ['commit', 4],
  ['commit_as', 6],
  ['tag', 5],
]);

const CLASSIFIED_MOVERS = new Map(
  [
    ['crates/rg-git/src/protocol/receive_pack.rs', 'update_ref', 'gix:reference', 'canonical_receive_pack'],
    ['crates/rg-core/src/repo/service.rs', 'auto_init_repo', 'git:push', 'repository_initialization'],
    ['crates/rg-core/src/repo/service.rs', 'push_branch_with_lease', 'git:push', 'leased_server_side_publish'],
    ['crates/rg-core/src/repo/service.rs', 'set_bare_repo_head_to_branch', 'gix:edit_reference', 'head_metadata'],
    ['crates/rg-core/src/repo/service.rs', 'fork_repo', 'git:clone', 'repository_creation'],
    ['crates/rg-core/src/repo/service.rs', 'create_or_update_file', 'git:clone', 'scratch_clone'],
    ['crates/rg-core/src/repo/service.rs', 'update_files_in_commit', 'git:clone', 'scratch_clone'],
    ['crates/rg-core/src/repo/service.rs', 'delete_file', 'git:clone', 'scratch_clone'],
    ['crates/rg-core/src/pull_request/service.rs', 'git_rebase_merge', 'git:push', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_merge_no_ff', 'gix:commit_as', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_squash_merge', 'gix:commit_as', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'gix_delete_ref', 'gix:edit_reference', 'pull_request_merge_policy'],
    ['crates/rg-core/src/pull_request/service.rs', 'run_fork_fetch', 'git:fetch', 'server_scratch_ref'],
    ['crates/rg-core/src/pull_request/service.rs', 'rebase_group_tree', 'git:fetch', 'object_transfer_only'],
    ['crates/rg-core/src/pull_request/service.rs', 'replay_rebase', 'git:clone', 'scratch_clone'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'ensure_merge_group_ci', 'git:fetch', 'object_transfer_only'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'cleanup_merge_group_ref', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'retire_losing_merge_group_pipeline', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'publish', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/pull_request/merge_queue.rs', 'drop', 'git:update-ref', 'internal_merge_queue_ref'],
    ['crates/rg-core/src/mirror/service.rs', 'publish_mirrored_refs', 'git:fetch', 'pull_mirror_publication'],
    ['crates/rg-core/src/mirror/service.rs', 'adoptable_head', 'git:symbolic-ref', 'head_metadata'],
    ['crates/rg-core/src/mirror/service.rs', 'run_git_clone_mirror', 'git:clone', 'mirror_private_clone'],
    ['crates/rg-core/src/mirror/service.rs', 'run_git_remote_update', 'git:remote', 'mirror_private_clone'],
    ['crates/rg-core/src/import/service.rs', 'clone_repo', 'git:clone', 'repository_creation'],
    ['crates/rg-core/src/import/service.rs', 'import_wiki_pages_from_destination', 'git:clone', 'scratch_clone'],
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

  const record = (index, primitive, call) => {
    const owner = ownerAt(ranges, index);
    movers.push({
      file: relative(file),
      symbol: owner?.symbol ?? '<outside-function>',
      primitive,
      line: code.slice(0, index).split('\n').length,
      body: owner ? source.slice(owner.start, owner.end) : '',
      call,
    });
  };

  // The argument list is the first `&[...]` / `&vec![...]` argument: a plain
  // gateway call takes it first, an `Invocation` takes the gateway first.
  const runCall = /\.(?:run\w*|spawn_async)\s*\(/g;
  for (const match of code.matchAll(runCall)) {
    const open = code.indexOf('(', match.index);
    const args = callArguments(source, code, open);
    const list = args?.find((arg) => /^&\s*(?:vec!\s*)?\[/s.test(arg));
    const command = list?.match(/^&\s*(?:vec!\s*)?\[\s*"([a-z-]+)"/s)?.[1];
    if (command && GIT_REF_COMMANDS.includes(command)) {
      record(match.index, `git:${command}`, list);
    }
  }

  const gixCall = /\.([a-z_]+)\s*\(/g;
  for (const match of code.matchAll(gixCall)) {
    const minimumArity = GIX_REF_METHODS.get(match[1]);
    if (minimumArity === undefined) continue;
    const args = callArguments(source, code, code.indexOf('(', match.index));
    const arity = args === null || (args.length === 1 && args[0] === '') ? 0 : args.length;
    if (arity >= minimumArity) record(match.index, `gix:${match[1]}`, args.join(', '));
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

  const where = `${mover.file}:${mover.symbol}`;
  const literals = [...mover.call.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((literal) => literal[1]);
  const refspecs = literals.filter((literal) => literal.includes(':'));

  if (classification.policy === 'repository_creation') {
    // `--bare` copies branches and tags; `--mirror` copies every ref the source
    // has, `refs/replace/*` and `refs/pull/*` included, into a repository the
    // server then reads and serves (card_03ed757463d4).
    if (!literals.includes('--bare') || literals.includes('--mirror')) {
      failures.push(`${where} must create the repository with \`clone --bare\`, never \`--mirror\`.`);
    }
  }

  if (classification.policy === 'pull_mirror_publication') {
    const outside = refspecs.filter(
      (refspec) => !/^\+?refs\/(heads|tags)\/\*:refs\/\1\/\*$/.test(refspec),
    );
    if (refspecs.length === 0 || outside.length > 0) {
      failures.push(
        `${where} publishes ${JSON.stringify(outside)} — a pull mirror may write only refs/heads/* and refs/tags/* of the served repository.`,
      );
    }
  }

  if (classification.policy === 'server_scratch_ref' && !literals.includes('--no-tags')) {
    // A fetch with a destination follows tags into refs/tags/* — past tag
    // protection and the post-push hooks.
    failures.push(`${where} fetches into a scratch ref without \`--no-tags\`.`);
  }

  if (classification.policy === 'object_transfer_only') {
    if (refspecs.length > 0 || literals.includes('--tags')) {
      failures.push(`${where} is classified object-transfer-only but stores refs (${JSON.stringify([...refspecs, ...literals.filter((literal) => literal === '--tags')])}).`);
    }
  }

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
