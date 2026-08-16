#!/usr/bin/env node

// Asserts that every script under `scripts/` is reachable from something that
// actually runs it — or carries a written reason why it is not.
//
// Why this exists: `scripts/observability-contract-check-regression.mjs` was
// executed by nothing for as long as it existed. It is the stand that proves the
// observability gate still asserts something, and it missed
// `run-contract-checks.mjs`'s glob by one hyphen: the glob matched
// `*-contract-check.mjs`, the stand ends in `-contract-check-regression.mjs`.
// Nothing was wrong with either file. The wiring was a *filename convention*,
// and a convention fails silently by construction — a file that does not match
// it is not reported as unmatched, it is simply never mentioned again.
//
// So the glob got widened, and this check exists so that the next widening is
// not needed to notice the next orphan. It does not care about names: it walks
// outward from the things an outside agent actually starts — CI workflows, git
// hooks, npm scripts — and asks which scripts that walk never reaches. A script
// the walk misses is either wired up, or listed in UNEXECUTED with a reason.
//
// Truth boundary, stated because it decides how to read a green run: a
// reference is a *mention* of the script's basename on a non-comment line of a
// reachable file, not a proven invocation. So this check is lenient — it can
// call a script alive on the strength of a mention that never executes. It
// leans that way on purpose: over-reporting would mean blocking a correct tree
// on a parse this check cannot do honestly, while under-reporting still catches
// the thing it was written for, a script nothing anywhere names.
//
// The subject is `scripts/` only. `deploy/*.sh` is operator tooling a human
// starts by hand — "nothing references it" is its normal state, not a defect,
// so folding it in here would make every run report a finding that is never
// actionable.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const SELF = basename(fileURLToPath(import.meta.url));
const RUNNER = 'run-contract-checks.mjs';

const failures = [];

// Scripts that nothing executes, and the reason each one does not have to.
//
// This is a ratchet, not a dumping ground — the same rules the contract-check
// runner's QUARANTINE lives by:
//   - an entry naming a file that no longer exists fails the check;
//   - an entry for a script that IS reachable fails the check (remove it);
//   - nothing goes in here without a reason a reader can act on.
//
// An exemption covers exactly the script it names and does not propagate to
// what that script calls. A manual tool's callees are equally manual, so they
// each owe their own line here — one entry quietly covering a subtree is how an
// exemption list stops describing the tree it exempts.
const UNEXECUTED = new Map([
  [
    'full-interface-regression.mjs',
    'a manual replay against a throwaway instance, and regression.yml says so where it runs the narrow ' +
      'routing-only smoke instead. It registers users, writes repositories and exercises delete verbs — ' +
      'nothing a shared runner should point at a real backend.',
  ],
  [
    'install-git-hooks.sh',
    'a one-time developer setup step (CONTRIBUTING.md tells you to run it). It installs the hook that ' +
      'runs the gates; a gate cannot install itself.',
  ],
]);

// The things an outside agent starts on its own. Everything else has to be
// reachable from one of these, or it is not run by anybody.
const ROOT_DIRS = ['.github/workflows', '.githooks'];
const ROOT_FILES = ['package.json', 'web/package.json'];

const roots = [];
for (const dir of ROOT_DIRS) {
  const full = join(root, dir);
  if (!existsSync(full)) {
    // A missing root is not "nothing to check" — it is this check losing the
    // half of the graph that dir represents, which would turn its verdicts into
    // noise. Say so instead of scoring the tree against what is left.
    failures.push(`${dir}/ does not exist, so every verdict below is computed from an incomplete root set`);
    continue;
  }
  for (const entry of readdirSync(full, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.isFile()) roots.push(join(full, entry.name));
  }
}
for (const file of ROOT_FILES) {
  const full = join(root, file);
  if (existsSync(full)) roots.push(full);
}

if (roots.length === 0) {
  console.error('❌ script execution coverage: no CI workflow, git hook or package.json found — nothing to walk from.');
  process.exit(1);
}

// The subjects: executables directly under `scripts/`, plus the modules under
// `scripts/lib/` they import (a library nobody imports is the same orphan one
// directory down).
function collect(dir, prefix) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isFile()) continue;
    if (!entry.name.endsWith('.mjs') && !entry.name.endsWith('.sh')) continue;
    out.push(`${prefix}${entry.name}`);
  }
  return out;
}

const libDir = join(scriptsDir, 'lib');
const scripts = [...collect(scriptsDir, ''), ...(existsSync(libDir) ? collect(libDir, 'lib/') : [])];

// References are matched by basename, so two scripts sharing one would make
// every reference to either ambiguous. Refuse rather than attribute it to the
// first one found.
const byBasename = new Map();
for (const script of scripts) {
  const name = basename(script);
  if (byBasename.has(name)) {
    failures.push(
      `${byBasename.get(name)} and ${script} share a basename, so this check cannot tell which one a ` +
        'reference means — rename one of them',
    );
  }
  byBasename.set(name, script);
}

// The runner's globs are read out of the runner, not restated here: a copy of
// the naming rule in this file would drift from the real one, and drift in the
// wiring rule is the exact defect this check exists to catch.
const runnerPath = join(scriptsDir, RUNNER);
const globSuffixes = existsSync(runnerPath)
  ? [...readFileSync(runnerPath, 'utf8').matchAll(/^const [A-Z_]*SUFFIX = '([^']+)';/gm)].map((match) => match[1])
  : [];
if (globSuffixes.length === 0) {
  failures.push(
    `could not read any \`const …SUFFIX = '…'\` glob out of scripts/${RUNNER} — without them this check ` +
      'would report every contract check as an orphan, so it refuses to score the tree instead',
  );
}

/** Non-comment text of a file: a script named only inside a comment is documented, not run. */
function executableText(path) {
  return readFileSync(path, 'utf8')
    .split('\n')
    .filter((line) => {
      const trimmed = line.trimStart();
      return !(
        trimmed.startsWith('//') ||
        trimmed.startsWith('#') ||
        trimmed.startsWith('*') ||
        trimmed.startsWith('/*')
      );
    })
    .join('\n');
}

function referencedIn(text) {
  return [...byBasename.entries()].filter(([name]) => text.includes(name)).map(([, script]) => script);
}

const reachable = new Set();
const queue = [];
function enqueue(script) {
  if (reachable.has(script)) return;
  reachable.add(script);
  queue.push(script);
}

for (const rootFile of roots) {
  for (const script of referencedIn(executableText(rootFile))) enqueue(script);
}

while (queue.length > 0) {
  const script = queue.shift();

  // This file names the scripts in UNEXECUTED, which would otherwise launder
  // every one of them into "reachable" — an exemption list that marks its own
  // entries alive would report itself as the thing keeping them alive.
  if (script === SELF) continue;

  for (const next of referencedIn(executableText(join(scriptsDir, script)))) enqueue(next);

  // The runner does not name its checks; it globs them.
  if (script === RUNNER) {
    for (const candidate of scripts) {
      if (candidate.includes('/')) continue;
      if (globSuffixes.some((suffix) => candidate.endsWith(suffix))) enqueue(candidate);
    }
  }
}

const orphans = scripts.filter((script) => !reachable.has(script) && !UNEXECUTED.has(script));
for (const script of orphans) {
  failures.push(
    `scripts/${script} is executed by nothing: no workflow, hook, npm script or reachable script names it, ` +
      'and no runner glob covers it. Wire it up, or record why it does not run in UNEXECUTED ' +
      `in scripts/${SELF}`,
  );
}

for (const script of UNEXECUTED.keys()) {
  if (!scripts.includes(script)) {
    failures.push(
      `UNEXECUTED names scripts/${script}, which does not exist — most likely it was renamed, which would ` +
        'move the real file back into the checked set under a new name while this entry keeps covering it',
    );
  } else if (reachable.has(script)) {
    failures.push(
      `scripts/${script} is listed in UNEXECUTED but something does run it now — remove the entry so the ` +
        'exemption list keeps meaning what it says',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ Script execution coverage failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `✅ script execution coverage: ${reachable.size}/${scripts.length} scripts reachable from ${roots.length} ` +
    `entry point(s), ${UNEXECUTED.size} exempt with a recorded reason`,
);
