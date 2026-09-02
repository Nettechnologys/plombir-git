#!/usr/bin/env node

// Every production command in `crates/rg-cli/src` that resolves the repository
// storage root must also decide what a missing one means — and every directory
// the CLI creates rather than requires must be written down here with the
// reason it may.
//
// `[server].repo_root` defaults to the relative `./repos`, and the directory
// that is not there is created rather than reported: `create_dir_all` exists to
// do exactly that. So a command started from the wrong directory — `docker
// exec` without `-w`, a cron entry, another shell — builds a second, empty root
// beside itself and every step afterwards succeeds against it. `import` is
// where that stops being a stray directory: the repository's row lands in the
// real database (`--db-url` / `--config` were right, `--repo-root` was
// forgotten) while the clone lands where `forgekeep serve` never looks, and the
// instance then lists a repository whose git directory nobody can open
// (card_cc8259eba428).
//
// `crate::repo_root` is where that question is answered once, by asking the
// database the command has already opened: an instance that owns repositories
// keeps them somewhere, so a root that is not there is a mistake about *which*
// root, while an instance with none is a clean install where creating it is
// correct. This is the second half of the class `crate::dbconn` closed for
// `[database].url` (card_8baddb74fa82), and it is checked here for the same
// reason its sibling `cli-db-opener-contract-check.mjs` exists: the module
// doc-comment says "the one place", and a sentence is not a gate.
//
// SECOND QUESTION, one level out: which directories does the CLI create at all.
// The defect is not `create_dir_all` — it is a *setting-derived* path being
// created rather than required, which turns "you are pointed somewhere else"
// into "done". rustc cannot ask that; it is not a type question. So the answer
// is an inventory: a new directory-creating site is red until somebody writes
// down which directory it makes and why making it is the right answer there.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(process.env.FORGEKEEP_CLI_REPO_ROOT_ROOT ?? path.join(scriptsDir, '..'));
const cliSrc = path.join(root, 'crates/rg-cli/src');
const GATEWAY = 'crates/rg-cli/src/repo_root.rs';
const DECLARING_FILE = 'crates/rg-cli/src/config.rs';

/**
 * Every production site that creates a directory instead of requiring one, and
 * why creating it is the right answer there.
 *
 * Keyed by file with a count, the same shape (and for the same reason) as the
 * sibling inventories in this directory: a file already listed cannot quietly
 * grow a second creator for a different path.
 */
const DIRECTORY_CREATORS = [
  {
    file: 'crates/rg-cli/src/admin.rs',
    sites: 2,
    directories: ['the backup output directory', 'the restore input directory'],
    why: 'both are the path the operator typed on this command line as the subject of the command, not an instance-wide setting resolved from a relative default — there is no other directory they could have meant',
  },
  {
    file: 'crates/rg-cli/src/commands.rs',
    sites: 3,
    directories: [
      'the repository storage root (`forgekeep import`)',
      'the repository storage root (`forgekeep create-repo`)',
      'the bare repository directory (`forgekeep create-repo`)',
    ],
    why: '`import` asks `repo_root::check_repo_root_presence` first, so it can only create a root on an instance that owns no repositories; `create-repo` opens no database to ask, and answers the weaker way it can — by announcing the absolute root instead of the relative spelling. `create-repo` makes the root in a call of its own so it can be the owner-only one, while `<owner>/<name>.git` below it keeps the mode the server\'s own repository-creation path gives them — reachability is decided once, at the root',
  },
  {
    file: 'crates/rg-cli/src/serve.rs',
    sites: 3,
    directories: [
      'the directory of `--listen-address-file`',
      'the at-rest encryption key directory',
      'the repository storage root',
    ],
    why: '`serve` is the process that defines the instance rather than one addressing an existing one, so a first boot creating its own root is the correct answer and not a mistake about which root',
  },
];

/**
 * Every production file that resolves `[server].repo_root`, with the commands
 * behind it and how many of them answer the missing-root question which way.
 *
 * A new subcommand copied from a neighbour brings the neighbour's resolution
 * with it and none of its judgement; that is the case this half is for.
 */
const REPO_ROOT_DECIDERS = [
  {
    file: 'crates/rg-cli/src/commands.rs',
    resolves: 3,
    checks: 2,
    commands: {
      'forgekeep import': 'CreateOnACleanInstance — it writes the database row that the clone has to match',
      'forgekeep index-repo': 'Refuse — it only reads out of the root, so creating one would produce an empty directory and the same failure a step later',
      'forgekeep create-repo': 'no database of its own to ask; announces the absolute root instead',
    },
  },
];

if (!existsSync(cliSrc)) {
  console.error('❌ CLI repo-root sweep found no crates/rg-cli/src to read.');
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

// `productionRustCode` blanks `#[cfg(test)]` items: a test builds throwaway
// directories by the dozen and is not what this inventory is about.
// Both spellings, or the inventory reads a directory out of existence the
// moment a site is narrowed: `create_dir_all_owner_only` creates exactly the
// same directories as `create_dir_all` and differs only in the mode it leaves
// on the ones it made, so a sweep that knew only the first name would have gone
// green on `serve` while it still created three of them.
const creator =
  /\b(?:std::fs::create_dir_all|rg_core::platform::fs::create_dir_all_owner_only(?:_async)?)\s*\(/g;
const resolver = /\bconfig::resolve_repo_root\s*\(/g;
const decider = /\brepo_root::check_repo_root_presence\s*\(/g;
const gatewayReference = /\brepo_root::/g;

// The spellings that would put either name in scope bare, hiding it from the
// regexes above.
const hiddenNames = [
  { pattern: /\buse\s+crate::repo_root::(?!\s*\{?\s*self\b)[^;]*;/g, what: '`crate::repo_root`' },
  { pattern: /\buse\s+(?:crate::)?config::resolve_repo_root\b[^;]*;/g, what: '`resolve_repo_root`' },
  {
    pattern: /\buse\s+(?:crate::)?config::\{[^;}]*\bresolve_repo_root\b[^;}]*\}\s*;/g,
    what: '`resolve_repo_root`',
  },
  { pattern: /\buse\s+std::fs::create_dir_all\b[^;]*;/g, what: '`create_dir_all`' },
  {
    pattern: /\buse\s+(?:rg_core::)?platform::fs::create_dir_all_owner_only\w*\b[^;]*;/g,
    what: '`create_dir_all_owner_only`',
  },
];

const files = rustFiles(cliSrc);
const violations = [];
const foundCreators = new Map();
const foundResolvers = new Map();
const foundCheckers = new Map();
const gatewayCalls = new Map();
let gatewayEntryPoints = 0;

for (const file of files) {
  const production = productionRustCode(readFileSync(file, 'utf8'));
  const name = relative(file);

  if (name === GATEWAY) {
    for (const entry of ['check_repo_root_presence', 'announce_a_new_repo_root']) {
      if (new RegExp(`\\bpub\\(crate\\)\\s+(?:async\\s+)?fn\\s+${entry}\\b`).test(production)) {
        gatewayEntryPoints += 1;
      }
    }
    continue;
  }

  const creators = [...production.matchAll(creator)].length;
  if (creators > 0) foundCreators.set(name, creators);

  const resolvers = [...production.matchAll(resolver)].length;
  if (resolvers > 0 && name !== DECLARING_FILE) foundResolvers.set(name, resolvers);

  const checkers = [...production.matchAll(decider)].length;
  if (checkers > 0) foundCheckers.set(name, checkers);

  if ([...production.matchAll(gatewayReference)].length > 0) gatewayCalls.set(name, true);

  for (const { pattern, what } of hiddenNames) {
    for (const match of production.matchAll(pattern)) {
      violations.push({
        file,
        source: production,
        index: match.index,
        reason: `${what} is imported into scope bare, which hides its call sites from this sweep — spell it qualified at the call site instead`,
      });
    }
  }
}

// ── First question: does every resolver decide ───────────────────────
const declaredDeciders = new Map(REPO_ROOT_DECIDERS.map((entry) => [entry.file, entry]));

for (const [name, resolves] of [...foundResolvers].sort()) {
  const entry = declaredDeciders.get(name);
  if (entry === undefined) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} resolves [server].repo_root (${resolves} site(s)) and is not in REPO_ROOT_DECIDERS. The default is relative, so a command that resolves it and does not decide what a missing root means will create one beside whatever directory it was started from and report success. Say which command this is and what it does when the root is not there`,
    });
    continue;
  }
  if (entry.resolves !== resolves) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} resolves [server].repo_root at ${resolves} site(s), REPO_ROOT_DECIDERS says ${entry.resolves} (${Object.keys(entry.commands).join(', ')}). A command joined the set or left it — either way the inventory has to say so`,
    });
  }
  if (!gatewayCalls.has(name)) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} resolves [server].repo_root without reaching \`crate::repo_root\` at all. Resolution and the missing-root decision have to travel together, or the second one is whatever the last person remembered`,
    });
  }
  const checks = foundCheckers.get(name) ?? 0;
  if (checks !== entry.checks) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} calls \`repo_root::check_repo_root_presence\` at ${checks} site(s), REPO_ROOT_DECIDERS says ${entry.checks}. Which command stopped asking, and what answers the question for it now?`,
    });
  }
}

for (const entry of REPO_ROOT_DECIDERS) {
  if (!foundResolvers.has(entry.file)) {
    violations.push({
      file: path.join(root, entry.file),
      source: '',
      index: 0,
      reason: `REPO_ROOT_DECIDERS names ${entry.file}, which resolves [server].repo_root nowhere any more (was: ${Object.keys(entry.commands).join(', ')}). Drop the entry — one pointing at nothing is how this inventory stops being one`,
    });
  }
}

// ── Second question: which directories the CLI creates ───────────────
const declaredCreators = new Map(DIRECTORY_CREATORS.map((entry) => [entry.file, entry]));

for (const [name, sites] of [...foundCreators].sort()) {
  const entry = declaredCreators.get(name);
  if (entry === undefined) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} creates a directory (${sites} \`std::fs::create_dir_all\` site(s)) and is not in DIRECTORY_CREATORS. A path resolved from a relative default and then created rather than required turns "you are pointed somewhere else" into "done" — write down which directory this makes and why making it is the right answer here`,
    });
    continue;
  }
  if (entry.sites !== sites) {
    violations.push({
      file: path.join(root, name),
      source: '',
      index: 0,
      reason: `${name} creates directories at ${sites} site(s), DIRECTORY_CREATORS says ${entry.sites} (${entry.directories.join('; ')}). A new one appeared or an old one went away, and either way it has to be read by somebody`,
    });
  }
}

for (const entry of DIRECTORY_CREATORS) {
  if (!foundCreators.has(entry.file)) {
    violations.push({
      file: path.join(root, entry.file),
      source: '',
      index: 0,
      reason: `DIRECTORY_CREATORS names ${entry.file}, which creates no directory any more (was: ${entry.directories.join('; ')}). Drop the entry`,
    });
  }
}

// ── Floors: an absence-based check must prove it is reading something ─
if (gatewayEntryPoints < 2) {
  console.error(
    `❌ CLI repo-root sweep found ${gatewayEntryPoints} of the 2 entry points it expects in ` +
      `${GATEWAY}. The gateway is not where this check thinks it is, so a green result here ` +
      'would mean nothing.',
  );
  process.exit(1);
}

if (foundResolvers.size === 0) {
  console.error(
    '❌ CLI repo-root sweep found no `config::resolve_repo_root` call anywhere in ' +
      'crates/rg-cli/src. Either the resolver was renamed — in which case this check is reading ' +
      'nothing — or the one-shot commands stopped resolving the root and REPO_ROOT_DECIDERS ' +
      'should be emptied deliberately.',
  );
  process.exit(1);
}

if (violations.length > 0) {
  for (const violation of violations) {
    // A violation with no source view is about a file rather than a line: the
    // inventory questions are answered per file, not per call.
    const where =
      violation.source === ''
        ? relative(violation.file)
        : `${relative(violation.file)}:${lineAt(violation.source, violation.index)}`;
    console.error(`❌ ${where}: ${violation.reason}`);
  }
  process.exit(1);
}

const creatorSites = [...foundCreators.values()].reduce((total, sites) => total + sites, 0);
const resolverSites = [...foundResolvers.values()].reduce((total, sites) => total + sites, 0);
console.log(
  `✅ CLI repository root: ${resolverSites} resolution site(s) across ` +
    `${foundResolvers.size} file(s), each reaching crate::repo_root; ${creatorSites} ` +
    'directory-creating site(s) across the inventory, each named with the reason it may create.',
);
