#!/usr/bin/env node

// Asserts that EVERY job of `.github/workflows/regression.yml` is accounted for
// in `scripts/run-local-gates.mjs` — mirrored by the local runner, mirrored by a
// command `scripts/verify-push-gates.sh` provably still invokes, excluded on
// purpose, declared as running in CI only, or declared as running nowhere with
// the reason. It also proves that
// `.githooks/pre-push` retains the verifier as its no-receipt fallback.
//
// Why this exists: the local mirror is only worth having if it cannot drift away
// from the workflow silently. Without this check, adding a cheap job to
// regression.yml and forgetting the mirror leaves that gate enforced by nothing
// — which is exactly the defect the mirror was built to answer, one level down.
// A hand-written list is never the ratchet; the thing that reads both sides is.
//
// It originally covered the cargo-free half only, which left the same hole in
// the other one: the hook named `cargo fmt` and `cargo clippy` in shell, five
// cargo jobs ran nowhere at all, and no reader of the repository could tell.
// Deleting a cargo line from the verifier must not go unnoticed; neither may
// removing the hook's fallback or adding another cargo job. The accounting now
// spans every job in the workflow, and the two halves differ only in what a
// bucket may claim — a cargo job may honestly claim "nowhere".
//
// Scope is deliberately coverage, not equivalence: a job's shell cannot be
// compared line by line with a JS port without producing a check that goes red
// on whitespace. What must not happen silently is a gate existing on one side
// only, and that is decidable.
//
// Cargo-free is decided from parsed `steps[].run` strings. YAML comments and
// prose outside a step never enter that graph, so they cannot move a job to the
// wrong side or make an invalid document look covered.

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  parseWorkflowFile,
  selectWorkflowParser,
  workflowJobRuns,
} from './lib/workflow.mjs';
import { shellCodeOnly, shellInvokes } from './lib/shell-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.PLOMBIR_GIT_LOCAL_GATE_COVERAGE_ROOT ?? resolve(scriptsDir, '..'));
const workflowPath = resolve(root, '.github/workflows/regression.yml');
const hookPath = resolve(root, '.githooks/pre-push');
const verifierPath = resolve(root, 'scripts/verify-push-gates.sh');

const { GATES, EXCLUDED, CARGO_JOBS } = await import('./run-local-gates.mjs');

const problems = [];
const { parser, missing } = selectWorkflowParser();

if (!parser) {
  console.error(
    `No YAML parser available (tried ${missing.join(', ')}) — install either, `
      + 'or local gate coverage cannot inspect regression.yml.',
  );
  process.exit(1);
}

const parsed = parseWorkflowFile(parser, workflowPath);
if (!parsed.ok && parsed.kind === 'spawn') {
  console.error(`${parser.name} could not be run on regression.yml — ${parsed.message}`);
  process.exit(1);
}
if (!parsed.ok && parsed.kind === 'syntax') {
  console.error(
    `.github/workflows/regression.yml is not valid YAML — ${parser.name} rejects it:\n     `
      + parsed.diagnostic.split('\n').join('\n     '),
  );
  process.exit(1);
}
if (!parsed.ok && parsed.kind === 'parser') {
  console.error(`${parser.name} failed on regression.yml (exit ${parsed.status})\n${parsed.diagnostic}`);
  process.exit(1);
}
if (!parsed.ok) {
  console.error(`${parser.name} produced output this check cannot read for regression.yml — ${parsed.message}`);
  process.exit(1);
}

const jobs = parsed.jobs;

// A parse that finds no jobs is the check breaking, not the workflow passing.
if (!jobs || Object.keys(jobs).length === 0) {
  console.error(`${workflowPath} parses, but declares no jobs — nothing in it can be covered locally.`);
  process.exit(1);
}

const runBodies = new Map();
for (const [job, definition] of Object.entries(jobs)) {
  const inspected = workflowJobRuns(definition);
  if (inspected.invalidSteps) {
    problems.push(`regression.yml: job \`${job}\` has a \`steps\` that is not a list — it cannot be classified.`);
    continue;
  }
  for (const invalid of inspected.invalidRuns) {
    const where = invalid.name ? `step \`${invalid.name}\`` : `step #${invalid.index + 1}`;
    problems.push(
      `regression.yml: job \`${job}\`, ${where} has a \`run\` that parsed as `
        + `${invalid.value === null ? 'empty' : typeof invalid.value}, not a shell command.`,
    );
  }
  runBodies.set(job, shellCodeOnly(inspected.runs.join('\n')));
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

const cargoFree = [...runBodies.entries()]
  .filter(([, body]) => !shellInvokes(body, 'cargo'))
  .map(([job]) => job);
const cargoBearing = Object.keys(jobs).filter((job) => !cargoFree.includes(job));

if (cargoFree.length === 0) {
  console.error('No cargo-free jobs found — every job appears to build Rust, which the workflow contradicts.');
  process.exit(1);
}
if (cargoBearing.length === 0) {
  console.error('No cargo-bearing jobs found — no job appears to build Rust, which the workflow contradicts.');
  process.exit(1);
}

const mirrored = new Set(GATES.map((gate) => gate.job));

// --- cargo-free half: mirrored by the local runner, or excluded on purpose ---

for (const job of cargoFree) {
  if (!mirrored.has(job) && !EXCLUDED.has(job)) {
    problems.push(
      `${job} is a cargo-free job of regression.yml that run-local-gates.mjs neither runs nor excludes. `
        + 'Add it to GATES, or to EXCLUDED with the reason it must not run before a push.',
    );
  }
}

// Both directions of staleness: a mirror or an exemption naming a job that no
// longer exists is an entry that has quietly stopped covering anything.
//
// The third direction is the one that was missing, and it is the one the whole
// coverage claim rests on: a job that still EXISTS but no longer DOES anything.
// Commenting out the single line `run: node scripts/run-contract-checks.mjs`
// left `contract-checks` in the job graph, mirrored here, counted as covered —
// while the workflow ran none of this repository's ~45 contract checks, and
// this check, whose one job is proving every check mechanism is executed by a
// job of regression.yml, reported it green (card_fad8ad0ef007). Accounting by
// job name is not coverage; the command has to still be in the job.
for (const gate of GATES) {
  if (!Object.hasOwn(jobs, gate.job)) {
    problems.push(`run-local-gates.mjs mirrors \`${gate.job}\`, which is not a job in regression.yml — renamed or removed.`);
    continue;
  }
  if (!cargoFree.includes(gate.job)) {
    problems.push(`run-local-gates.mjs mirrors \`${gate.job}\`, which now invokes cargo — it no longer belongs in a seconds-budget hook.`);
    continue;
  }
  if (typeof gate.invokes !== 'string' || gate.invokes === '') {
    problems.push(
      `run-local-gates.mjs mirrors \`${gate.job}\` without declaring what that job invokes. `
        + 'Give the GATES entry an `invokes` command, or the mirror covers a job name rather than a gate.',
    );
    continue;
  }
  if (!shellInvokes(runBodies.get(gate.job), gate.invokes)) {
    problems.push(
      `regression.yml job \`${gate.job}\` no longer runs \`${gate.invokes}\`, which run-local-gates.mjs mirrors it for. `
        + 'The job still exists and still counts as covered while executing nothing of the sort — '
        + 'restore the step, or move the entry to EXCLUDED with the reason.',
    );
  }
}

for (const job of EXCLUDED.keys()) {
  if (!Object.hasOwn(jobs, job)) {
    problems.push(`run-local-gates.mjs excludes \`${job}\`, which is not a job in regression.yml — remove or fix the entry.`);
  }
}

// --- cargo half: mirrored by the verifier, run by CI only, or nowhere ---

// "Runs in CI" is a claim about the workflow, so it is checked against the
// workflow: a trigger that fires on what a contributor does (push / pull
// request), and no `if:` on the job that turns it off. `workflow_dispatch`
// alone would make every job a gate somebody has to remember to press.
//
// YAML 1.1 parsers read the bare key `on` as the boolean `true`, so the
// trigger is looked up under both spellings.
const workflowDocument = parsed.document ?? {};
const triggers = workflowDocument.on ?? workflowDocument[true] ?? workflowDocument.true;
const triggerNames = typeof triggers === 'string'
  ? [triggers]
  : Array.isArray(triggers)
    ? triggers
    : Object.keys(triggers ?? {});
const runsOnContribution = triggerNames.some((name) => name === 'push' || name === 'pull_request');

function switchedOff(definition) {
  if (!Object.hasOwn(definition ?? {}, 'if')) return false;
  const condition = definition.if;
  if (condition === false) return true;
  const text = String(condition).replace(/^\s*\$\{\{\s*|\s*\}\}\s*$/g, '').trim();
  return text === 'false';
}

// Comments are stripped before grepping shell files for the same reason they are
// stripped from the workflow: prose naming `cargo clippy` executes nothing.
function activeShell(sourcePath) {
  return shellCodeOnly(readFileSync(sourcePath, 'utf8'));
}

const hookCommands = activeShell(hookPath);
// The verifier intentionally interposes a warning/status-preserving shell
// function. Coverage is about the wrapped command, so remove that trusted
// prefix before asking the generic shell parser what is invoked. The separate
// push-gate-warning contract check proves that every verifier command retains
// the wrapper.
const verifierCommands = activeShell(verifierPath).replace(/\brun_warning_free\s+(?:env\s+)?/g, '');

if (!shellInvokes(hookCommands, 'sh scripts/verify-push-gates.sh')) {
  problems.push(
    '.githooks/pre-push no longer invokes scripts/verify-push-gates.sh when its receipt is absent.',
  );
}
if (!shellInvokes(verifierCommands, 'node scripts/run-local-gates.mjs')) {
  problems.push(
    'scripts/verify-push-gates.sh no longer invokes scripts/run-local-gates.mjs.',
  );
}

for (const job of cargoBearing) {
  if (!CARGO_JOBS.has(job)) {
    problems.push(
      `${job} is a cargo job of regression.yml that CARGO_JOBS in run-local-gates.mjs does not account for. `
        + 'Give it a `verifier` command the card verifier runs, a `ciOnly` reason it cannot be mirrored before a push, '
        + 'or an `uncovered` reason saying it runs nowhere.',
    );
  }
}

for (const [job, where] of CARGO_JOBS) {
  if (!Object.hasOwn(jobs, job)) {
    problems.push(`CARGO_JOBS accounts for \`${job}\`, which is not a job in regression.yml — renamed or removed.`);
    continue;
  }
  if (!cargoBearing.includes(job)) {
    problems.push(`CARGO_JOBS accounts for \`${job}\`, which no longer invokes cargo — it belongs in GATES or EXCLUDED now.`);
    continue;
  }
  const claims = ['verifier', 'ciOnly', 'uncovered'].filter((key) => where[key]);
  if (claims.length !== 1) {
    problems.push(
      `CARGO_JOBS entry \`${job}\` claims ${claims.length === 0 ? 'none' : claims.map((claim) => `\`${claim}\``).join(' and ')} `
        + 'of `verifier`, `ciOnly`, `uncovered` — a gate is accounted for in exactly one way.',
    );
    continue;
  }
  if (where.ciOnly && !runsOnContribution) {
    problems.push(
      `CARGO_JOBS says \`${job}\` runs in CI, but regression.yml is triggered by neither \`push\` nor \`pull_request\` — `
        + 'nothing a contributor does starts it. Restore the trigger, or move the entry to `uncovered`.',
    );
  }
  if (where.ciOnly && switchedOff(jobs[job])) {
    problems.push(
      `CARGO_JOBS says \`${job}\` runs in CI, but its \`if:\` switches the job off. `
        + 'Remove the condition, or move the entry to `uncovered`.',
    );
  }
  if (where.verifier && !shellInvokes(verifierCommands, where.verifier)) {
    problems.push(
      `CARGO_JOBS says \`${job}\` is mirrored by \`${where.verifier}\` in scripts/verify-push-gates.sh, which no longer invokes it. `
        + 'Restore the command, or move the entry to `uncovered` with the reason.',
    );
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

const verifierMirrored = [...CARGO_JOBS.values()].filter((where) => where.verifier).length;
const ciOnly = [...CARGO_JOBS.values()].filter((where) => where.ciOnly).length;
const uncovered = [...CARGO_JOBS.values()].filter((where) => where.uncovered).length;

console.log(
  `local gate coverage: ${Object.keys(jobs).length} job(s) in regression.yml — `
    + `${mirrored.size} mirrored by run-local-gates.mjs, ${verifierMirrored} by the card verifier, `
    + `${EXCLUDED.size} excluded by design, ${ciOnly} in CI only, ${uncovered} running nowhere`,
);
