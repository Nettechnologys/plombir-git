#!/usr/bin/env node

// Asserts that every `#[ignore]`d Rust test in the workspace is run by a step of
// `.github/workflows/regression.yml`, or is listed in NOT_RUN_BY_CI below with
// the reason no step runs it. It also asserts the converse: a step that asks
// for ignored tests, or narrows any test run by name, must select at least one
// test — a server-database step whose filter names a renamed module runs
// nothing and stays green.
//
// Why this exists: `#[ignore]` takes a test out of `cargo nextest run
// --workspace`, so it is executed only by a step that names it. The stock
// Docker and git-lfs client tests of rg-http were believed to run in CI and
// ran nowhere (card_7404df85da21); a dozen server-database tests were in the
// same state (card_0c68a0dc41e8). Nothing about an ignored test that no step
// runs looks wrong: it compiles, it is listed, and it never fails.
//
// The check reads both sides instead of trusting either:
//   - the tests, from the module tree of every crate target (`mod x;`,
//     `#[path]`, inline `mod x { … }`), so each test gets the path libtest and
//     nextest match their filters against;
//   - the steps, from the parsed workflow, by reading every `cargo test` and
//     `cargo nextest run` invocation the way cargo would: package, target,
//     name filters, `--skip`, `--exact` and nextest's `-E` filter sets.
// A filter it cannot read turns the check red rather than counting as "runs
// everything" or "runs nothing".

import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { testInclusiveRustCode, testInclusiveRustSource } from './lib/rust-source.mjs';
import { shellCodeOnly } from './lib/shell-source.mjs';
import { parseWorkflowFile, selectWorkflowParser } from './lib/workflow.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.PLOMBIR_GIT_IGNORED_TESTS_ROOT ?? resolve(scriptsDir, '..'));
const workflowPath = resolve(root, '.github/workflows/regression.yml');

// Ignored tests that no step of regression.yml runs, keyed `<package> <path>`.
// Every entry says why. An entry naming a test that no longer exists, that is
// no longer ignored, or that a step has started running fails the check: the
// list only ever shrinks by somebody looking at it.
export const NOT_RUN_BY_CI = new Map([
  // Child processes: an ordinary test re-executes the test binary with only
  // this test selected, under the environment it wants to prove harmless.
  ['rg-core pull_request::service::diff_configuration_ownership_tests::numstat_under_a_hostile_host_config_child',
    'child process of the_numstat_ignores_the_hosts_git_configuration'],
  ['rg-core pull_request::service::merge_configuration_ownership_tests::merge_under_a_hostile_host_config_child',
    'child process of merge_ignores_the_hosts_git_configuration'],
  ['rg-core pull_request::service::rebase_configuration_ownership_tests::rebase_under_a_hostile_host_config_child',
    'child process of rebase_ignores_the_hosts_git_configuration'],
  ['rg-git cli_gateway::tests::ls_remote_probe_run_from_an_enclosing_repository',
    'child process re-executed from a hostile working directory by its parent test'],
  ['rg-git transport_environment_child',
    'child process of the ambient-authority acceptance tests in tests/ambient_authority.rs'],
  ['rg-git host_configuration_child',
    'child process of the_host_git_configuration_never_reaches_a_gateway_child'],
  // Not assertions.
  ['rg-http session_gate_cost_tests::session_gate_lookup_cost',
    'a measurement that prints a cost and asserts nothing; run by hand when tuning the session gate'],
  // External services the CI runner does not provide.
  ['rg-http npm_live_client_tests::npm_publish_beta_and_install_by_tag_round_trip',
    'drives the stock npm client; the CI image has no npm on the Rust job and the registry contract is covered by the in-process npm tests'],
  ['rg-http npm_live_client_tests::npm_publish_with_public_provenance_bundle_round_trips',
    'needs registry.npmjs.org on the network in addition to npm'],
  ['rg-runner commands::shared_docker_daemon_tests::stopping_one_same_id_job_leaves_the_other_runner_container_alive',
    'needs a Docker daemon shared by two runners and the ubuntu:24.04 image pulled'],
  ['rg-runner commands::shared_docker_daemon_tests::an_orphaned_grandchild_does_not_stay_a_zombie',
    'needs a Docker daemon and the ubuntu:24.04 image pulled; the argv tests check --init on every run'],
]);

const problems = [];

// ── The tests ───────────────────────────────────────────────────────────────

function packageName(manifest) {
  const source = readFileSync(manifest, 'utf8');
  const section = source.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|(?![\s\S]))/m);
  return section?.[1].match(/^name\s*=\s*"([^"]+)"/m)?.[1] ?? null;
}

// Crate targets the way cargo discovers them for this workspace: no manifest
// sets `autotests = false` or a custom `[[test]]` path (the check refuses to
// guess if one does), and `[[bin]]` entries only rename src/main.rs.
function crateTargets(crateDir) {
  const manifest = readFileSync(join(crateDir, 'Cargo.toml'), 'utf8');
  if (/^\s*\[\[test\]\]/m.test(manifest) || /^\s*autotests\s*=/m.test(manifest)) {
    problems.push(
      `${relative(root, crateDir)}/Cargo.toml declares its own test targets; teach this check to read them.`,
    );
  }
  const targets = [];
  if (existsSync(join(crateDir, 'src/lib.rs'))) targets.push({ kind: 'lib', name: null, file: join(crateDir, 'src/lib.rs') });
  if (existsSync(join(crateDir, 'src/main.rs'))) targets.push({ kind: 'bin', name: null, file: join(crateDir, 'src/main.rs') });
  const testsDir = join(crateDir, 'tests');
  if (existsSync(testsDir)) {
    for (const entry of readdirSync(testsDir).sort()) {
      const path = join(testsDir, entry);
      if (entry.endsWith('.rs') && statSync(path).isFile()) {
        targets.push({ kind: 'test', name: entry.slice(0, -3), file: path });
      } else if (statSync(path).isDirectory() && existsSync(join(path, 'main.rs'))) {
        targets.push({ kind: 'test', name: entry, file: join(path, 'main.rs') });
      }
    }
  }
  return targets;
}

// Offsets of `{` … `}` pairs in a string- and comment-free view.
function braceMatches(code) {
  const close = new Map();
  const stack = [];
  for (let index = 0; index < code.length; index += 1) {
    if (code[index] === '{') stack.push(index);
    else if (code[index] === '}') close.set(stack.pop(), index);
  }
  return close;
}

const MOD_DECL = /(^|[\s;{}\]])(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*([;{])/g;
const PATH_ATTR = /#\[\s*path\s*=\s*"([^"]+)"\s*\]\s*$/;
const IGNORE_ATTR = /#\[\s*ignore\b/g;
// `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = …)]` — not `#[cfg(test)]`.
const TEST_ATTR = /#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*test\b\s*(?:\([^\]]*\))?\s*\]/g;
const FN_AFTER = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/g;

// Walk one module file: record its tests with their module path and whether
// they are ignored, and follow `mod x;` into the file cargo would compile.
const visited = new Set();

function walkModule({ file, modulePath, dirForChildren, pkg, target, seen, tests }) {
  if (seen.has(file)) return;
  seen.add(file);
  visited.add(file);
  const raw = readFileSync(file, 'utf8');
  const code = testInclusiveRustCode(raw);
  const text = testInclusiveRustSource(raw);
  const close = braceMatches(code);

  // Inline modules, as [open, close, name] spans, to resolve nesting.
  const inline = [];
  for (const match of code.matchAll(MOD_DECL)) {
    const name = match[2];
    const declEnd = match.index + match[0].length - 1;
    if (match[3] === '{') {
      inline.push({ open: declEnd, close: close.get(declEnd) ?? code.length, name });
      continue;
    }
    // `mod name;` — an attribute directly above may relocate the file.
    const nesting = inline
      .filter((span) => span.open < match.index && match.index < span.close)
      .map((span) => span.name);
    const before = text.slice(Math.max(0, match.index - 400), match.index + match[1].length);
    const pathAttr = before.replace(/\s+$/, '').match(PATH_ATTR)?.[1];
    const baseDir = join(dirForChildren, ...nesting);
    let child;
    let childDir;
    if (pathAttr) {
      // rustc gives a `#[path]` file directory ownership like a mod.rs: its
      // own `mod x;` resolve next to it, whatever the file is called.
      child = resolve(nesting.length ? baseDir : dirname(file), pathAttr);
      childDir = dirname(child);
    } else if (existsSync(join(baseDir, `${name}.rs`))) {
      child = join(baseDir, `${name}.rs`);
      childDir = join(baseDir, name);
    } else if (existsSync(join(baseDir, name, 'mod.rs'))) {
      child = join(baseDir, name, 'mod.rs');
      childDir = join(baseDir, name);
    } else {
      problems.push(`${relative(root, file)}: cannot find the file for \`mod ${name};\`.`);
      continue;
    }
    walkModule({
      file: child,
      modulePath: [...modulePath, ...nesting, name],
      dirForChildren: childDir,
      pkg,
      target,
      seen,
      tests,
    });
  }

  // Attributes belong to the next `fn`, in whichever order they are stacked.
  const byFn = new Map();
  const attach = (match, kind) => {
    FN_AFTER.lastIndex = match.index;
    const fn = FN_AFTER.exec(code);
    if (!fn) {
      problems.push(`${relative(root, file)}: \`${match[0]}\` with no function after it.`);
      return;
    }
    const entry = byFn.get(fn.index) ?? { name: fn[1], at: match.index, test: false, ignored: false };
    entry[kind] = true;
    entry.at = Math.min(entry.at, match.index);
    byFn.set(fn.index, entry);
  };
  for (const match of code.matchAll(TEST_ATTR)) attach(match, 'test');
  for (const match of code.matchAll(IGNORE_ATTR)) attach(match, 'ignored');

  for (const [fnAt, entry] of byFn) {
    const nesting = inline
      .filter((span) => span.open < fnAt && fnAt < span.close)
      .map((span) => span.name);
    const path = [...modulePath, ...nesting, entry.name].join('::');
    const line = raw.slice(0, entry.at).split('\n').length;
    tests.push({ pkg, target, path, ignored: entry.ignored, where: `${relative(root, file)}:${line}` });
  }
}

const tests = [];
const cratesDir = join(root, 'crates');
for (const entry of readdirSync(cratesDir).sort()) {
  const crateDir = join(cratesDir, entry);
  if (!existsSync(join(crateDir, 'Cargo.toml'))) continue;
  const pkg = packageName(join(crateDir, 'Cargo.toml'));
  if (!pkg) {
    problems.push(`crates/${entry}/Cargo.toml: no [package] name.`);
    continue;
  }
  for (const target of crateTargets(crateDir)) {
    walkModule({
      file: target.file,
      modulePath: [],
      dirForChildren: dirname(target.file),
      pkg,
      target,
      seen: new Set(),
      tests,
    });
  }
}

// A file holding an `#[ignore]` that no target's module tree reaches is either
// dead code or a module this reader failed to follow. Both deserve a look; a
// silent skip would drop its tests from the accounting.
function rustFiles(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== 'target' && entry.name !== 'node_modules') rustFiles(path, out);
    } else if (entry.name.endsWith('.rs')) {
      out.push(path);
    }
  }
  return out;
}
for (const file of rustFiles(cratesDir)) {
  if (visited.has(file)) continue;
  if (/#\[\s*ignore\b/.test(testInclusiveRustCode(readFileSync(file, 'utf8')))) {
    problems.push(`${relative(root, file)} holds an #[ignore] test, but no crate target's module tree reaches it.`);
  }
}

const ignoredTests = tests.filter((test) => test.ignored);
if (ignoredTests.length === 0 || ignoredTests.length === tests.length) {
  console.error(
    `Read ${tests.length} test(s), ${ignoredTests.length} of them ignored, under crates/ — the reader is broken, not the workspace.`,
  );
  process.exit(1);
}

// ── The steps ───────────────────────────────────────────────────────────────

function shellWords(command) {
  const words = [];
  let word = '';
  let quote = null;
  let inWord = false;
  for (let index = 0; index < command.length; index += 1) {
    const char = command[index];
    if (quote) {
      if (char === quote) quote = null;
      else if (char === '\\' && quote === '"' && index + 1 < command.length) word += command[++index];
      else word += char;
      continue;
    }
    if (char === "'" || char === '"') {
      quote = char;
      inWord = true;
    } else if (char === '\\' && index + 1 < command.length) {
      if (command[index + 1] !== '\n') word += command[index + 1];
      index += 1;
      inWord = true;
    } else if (/\s/.test(char)) {
      if (inWord) words.push(word);
      word = '';
      inWord = false;
    } else {
      word += char;
      inWord = true;
    }
  }
  if (inWord) words.push(word);
  return words;
}

// Shell commands of one `run:` body, split at the operators that start a new
// command. Quoted text never splits because the split runs over words.
function shellCommands(body) {
  const commands = [];
  for (const line of shellCodeOnly(body).split('\n')) {
    let current = [];
    for (const word of shellWords(line)) {
      if (['&&', '||', ';', '|'].includes(word)) {
        if (current.length) commands.push(current);
        current = [];
      } else {
        current.push(word);
      }
    }
    if (current.length) commands.push(current);
  }
  return commands;
}

const CARGO_VALUE_FLAGS = new Set([
  '-p', '--package', '--test', '--bin', '--example', '--bench', '-j', '--jobs', '-F', '--features',
  '--target', '--profile', '--manifest-path', '--exclude', '--target-dir', '--color', '--config',
  '-E', '--filterset', '--filter-expr', '--run-ignored', '--partition', '--test-threads',
  '--retries', '--failure-output', '--success-output', '--status-level', '--final-status-level',
  '--cargo-profile', '--archive-file', '--message-format',
]);
const LIBTEST_VALUE_FLAGS = new Set(['--skip', '--test-threads', '--format', '--logfile', '--color', '-Z', '--report-time']);

// The argument of one `test(…)` term, or null unless the term is exactly that:
// a regex matcher may hold parentheses of its own, so the term must close at
// its last byte without the depth ever dropping below zero before it.
function testTermArgument(term) {
  const trimmed = term.trim();
  if (!trimmed.startsWith('test(') || !trimmed.endsWith(')')) return null;
  const arg = trimmed.slice('test('.length, -1);
  let depth = 0;
  for (const char of arg) {
    if (char === '(') depth += 1;
    else if (char === ')') depth -= 1;
    if (depth < 0) return null;
  }
  return depth === 0 ? arg.trim() : null;
}

// One nextest filter-set term: `test(…)` with a regex, exact, contains or
// default (contains) matcher.
function nextestTerm(term) {
  const arg = testTermArgument(term);
  if (arg === null) return null;
  if (arg.startsWith('/') && arg.endsWith('/') && arg.length >= 2) {
    let re;
    try {
      re = new RegExp(arg.slice(1, -1));
    } catch {
      return null;
    }
    return (name) => re.test(name);
  }
  if (arg.startsWith('=')) return (name) => name === arg.slice(1);
  if (arg.startsWith('~')) return (name) => name.includes(arg.slice(1));
  if (/^[A-Za-z0-9_:]+$/.test(arg)) return (name) => name.includes(arg);
  return null;
}

// Split a filter set at its top-level `|`, never inside a term. Other
// operators (`or`, `&`, `-`, `not`) stay inside a term and make it unreadable.
function topLevelUnion(expr) {
  const terms = [];
  let depth = 0;
  let current = '';
  for (let index = 0; index < expr.length; index += 1) {
    const char = expr[index];
    if (char === '(') depth += 1;
    else if (char === ')') depth -= 1;
    if (depth === 0 && char === '|') {
      terms.push(current);
      current = '';
      continue;
    }
    current += char;
  }
  terms.push(current);
  return terms;
}

function nextestFilterSet(expr, where) {
  const matchers = topLevelUnion(expr).map(nextestTerm);
  if (matchers.some((matcher) => matcher === null)) {
    problems.push(
      `${where}: filter set ${JSON.stringify(expr)} uses more than \`test(…)\` terms joined by \`|\`; `
        + 'teach this check to read it rather than guessing what it runs.',
    );
    return null;
  }
  return (name) => matchers.some((matcher) => matcher(name));
}

// What one cargo invocation selects among ignored tests, or null when it runs
// none of them.
function readInvocation(words, where) {
  const start = words.indexOf('cargo');
  if (start < 0) return null;
  const sub = words[start + 1];
  const nextest = sub === 'nextest' && words[start + 2] === 'run';
  if (sub !== 'test' && !nextest) return null;

  const args = words.slice(start + (nextest ? 3 : 2));
  const dash = args.indexOf('--');
  const cargoArgs = dash < 0 ? args : args.slice(0, dash);
  const harnessArgs = dash < 0 ? [] : args.slice(dash + 1);

  const packages = [];
  const testTargets = [];
  let lib = false;
  let bins = false;
  let workspace = false;
  let runIgnored = null;
  let filterSet = null;
  const filters = [];
  for (let index = 0; index < cargoArgs.length; index += 1) {
    const arg = cargoArgs[index];
    const [flag, inlineValue] = arg.startsWith('--') && arg.includes('=') ? arg.split(/=(.*)/s) : [arg, undefined];
    if (CARGO_VALUE_FLAGS.has(flag)) {
      const value = inlineValue ?? cargoArgs[++index];
      if (flag === '-p' || flag === '--package') packages.push(value);
      else if (flag === '--test') testTargets.push(value);
      else if (flag === '--run-ignored') runIgnored = value;
      else if (flag === '-E' || flag === '--filterset' || flag === '--filter-expr') {
        filterSet = nextestFilterSet(value, where);
        if (!filterSet) return null;
      }
      continue;
    }
    if (flag === '--lib') lib = true;
    else if (flag === '--bins') bins = true;
    else if (flag === '--workspace' || flag === '--all') workspace = true;
    else if (flag === '--tests' || flag === '--all-targets') testTargets.push('*');
    else if (flag === '--doc') return null;
    else if (!flag.startsWith('-')) filters.push(flag);
  }

  const skips = [];
  let exact = false;
  let ignored = false;
  for (let index = 0; index < harnessArgs.length; index += 1) {
    const arg = harnessArgs[index];
    if (arg === '--ignored') ignored = true;
    else if (arg === '--exact') exact = true;
    else if (LIBTEST_VALUE_FLAGS.has(arg)) {
      if (arg === '--skip') skips.push(harnessArgs[index + 1]);
      index += 1;
    } else if (!arg.startsWith('-')) filters.push(arg);
  }
  // Which tests the harness runs at all: ordinary ones, ignored ones, or both.
  let runs = 'normal';
  if (nextest && runIgnored) {
    runs = { only: 'ignored', 'ignored-only': 'ignored', all: 'both', default: 'normal' }[runIgnored] ?? null;
  } else if (harnessArgs.includes('--include-ignored')) {
    runs = 'both';
  } else if (ignored) {
    runs = 'ignored';
  }
  if (runs === null) {
    problems.push(`${where}: \`--run-ignored ${runIgnored}\` is not a value this check knows.`);
    return null;
  }
  if (!workspace && packages.length === 0) {
    problems.push(`${where}: runs tests without \`-p\` or \`--workspace\`; name the package.`);
    return null;
  }

  return {
    where,
    runs,
    narrowed: filters.length > 0 || skips.length > 0 || filterSet !== null,
    selects(test) {
      if (runs === 'ignored' && !test.ignored) return false;
      if (runs === 'normal' && test.ignored) return false;
      if (packages.length && !packages.includes(test.pkg)) return false;
      const anyTargetFlag = lib || bins || testTargets.length > 0;
      if (anyTargetFlag) {
        const kind = test.target.kind;
        const targetSelected = (lib && kind === 'lib')
          || (bins && kind === 'bin')
          || (kind === 'test' && (testTargets.includes('*') || testTargets.includes(test.target.name)));
        if (!targetSelected) return false;
      }
      const name = test.path;
      const matches = (filter) => (exact ? name === filter : name.includes(filter));
      if (filters.length && !filters.some(matches)) return false;
      if (skips.some((skip) => (exact ? name === skip : name.includes(skip)))) return false;
      if (filterSet && !filterSet(name)) return false;
      return true;
    },
  };
}

const { parser, missing } = selectWorkflowParser();
if (!parser) {
  console.error(`No YAML parser available (tried ${missing.join(', ')}); cannot read regression.yml.`);
  process.exit(1);
}
const parsed = parseWorkflowFile(parser, workflowPath);
if (!parsed.ok || !parsed.jobs) {
  console.error(`regression.yml could not be read: ${parsed.message ?? parsed.diagnostic ?? parsed.kind}`);
  process.exit(1);
}

const invocations = [];
for (const [job, definition] of Object.entries(parsed.jobs)) {
  if (definition?.if === false || String(definition?.if ?? '').trim() === 'false') continue;
  for (const [index, step] of (definition?.steps ?? []).entries()) {
    if (!step || typeof step.run !== 'string') continue;
    if (step.if === false || String(step.if ?? '').trim() === 'false') continue;
    const where = `regression.yml job \`${job}\`, step \`${step.name ?? `#${index + 1}`}\``;
    for (const words of shellCommands(step.run)) {
      const invocation = readInvocation(words, where);
      if (invocation) invocations.push(invocation);
    }
  }
}

const ignoredRunners = invocations.filter((invocation) => invocation.runs !== 'normal');
if (ignoredRunners.length === 0) {
  console.error('No step of regression.yml runs ignored tests — the reader is broken, or every ignored test runs nowhere.');
  process.exit(1);
}

// ── Accounting ──────────────────────────────────────────────────────────────

const keyOf = (test) => `${test.pkg} ${test.path}`;
const byKey = new Map(ignoredTests.map((test) => [keyOf(test), test]));

let runByCi = 0;
for (const test of ignoredTests) {
  const runners = invocations.filter((invocation) => invocation.selects(test));
  const listed = NOT_RUN_BY_CI.has(keyOf(test));
  if (runners.length > 0 && listed) {
    problems.push(
      `${test.where} \`${keyOf(test)}\` is run by ${runners[0].where}, yet NOT_RUN_BY_CI still lists it — remove the entry.`,
    );
  } else if (runners.length === 0 && !listed) {
    problems.push(
      `${test.where} \`${keyOf(test)}\` is #[ignore]d and no step of regression.yml runs it. `
        + 'Add it to a step (the server-database jobs run `-- --ignored` filters), or to NOT_RUN_BY_CI with the reason.',
    );
  }
  if (runners.length > 0) runByCi += 1;
}

for (const [key, reason] of NOT_RUN_BY_CI) {
  if (!byKey.has(key)) {
    problems.push(`NOT_RUN_BY_CI lists \`${key}\`, which is not an #[ignore]d test any more — remove the entry.`);
  }
  if (typeof reason !== 'string' || reason.trim().length < 10) {
    problems.push(`NOT_RUN_BY_CI entry \`${key}\` has no reason.`);
  }
}

// A step that narrows by name and selects nothing runs nothing: a renamed test
// or module leaves it green and empty. The same holds for any step asking for
// ignored tests, narrowed or not.
for (const invocation of invocations) {
  if (!invocation.narrowed && invocation.runs === 'normal') continue;
  if (!tests.some((test) => invocation.selects(test))) {
    const asked = invocation.runs === 'ignored' ? 'ignored tests' : 'tests by name';
    problems.push(`${invocation.where} asks for ${asked} and selects none of them.`);
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

const narrowed = invocations.filter((invocation) => invocation.narrowed).length;
console.log(
  `ignored tests: ${ignoredTests.length} #[ignore] test(s) — ${runByCi} run by regression.yml, `
    + `${NOT_RUN_BY_CI.size} not run by CI with a reason; ${ignoredRunners.length} step command(s) run ignored tests, `
    + `${narrowed} narrow by name and each selects a test`,
);
