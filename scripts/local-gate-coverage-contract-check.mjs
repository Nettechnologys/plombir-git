#!/usr/bin/env node

// Asserts that EVERY job of `.github/workflows/regression.yml` is accounted for
// in `scripts/run-local-gates.mjs` — mirrored by the local runner, mirrored by a
// command `.githooks/pre-push` provably still invokes, excluded on purpose, or
// declared as running nowhere with the reason.
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
// Deleting a cargo line from the hook went unnoticed, and so would an eighth
// cargo job. The accounting now spans all twelve, and the two halves differ only
// in what a bucket may claim — a cargo job may honestly claim "nowhere".
//
// Scope is deliberately coverage, not equivalence: a job's shell cannot be
// compared line by line with a JS port without producing a check that goes red
// on whitespace. What must not happen silently is a gate existing on one side
// only, and that is decidable.
//
// Cargo-free is decided from the `run:` bodies, with comment lines stripped
// first: the workflow's prose discusses `cargo build --release` at length above
// jobs that never invoke cargo, and classifying on raw text puts them on the
// wrong side.

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const workflowPath = resolve(root, '.github/workflows/regression.yml');
const hookPath = resolve(root, '.githooks/pre-push');

const { GATES, EXCLUDED, CARGO_JOBS } = await import('./run-local-gates.mjs');

function parseJobs(source) {
  const lines = source.split('\n').filter((line) => !/^\s*#/.test(line));
  const jobs = new Map();
  let inJobs = false;
  let current = null;

  for (const line of lines) {
    if (/^jobs:\s*$/.test(line)) {
      inJobs = true;
      continue;
    }
    if (!inJobs) continue;
    // Top-level keys reset the section; job ids sit exactly two spaces in.
    if (/^\S/.test(line) && line.trim() !== '') {
      inJobs = false;
      current = null;
      continue;
    }
    const header = line.match(/^ {2}([A-Za-z0-9][A-Za-z0-9_-]*):\s*$/);
    if (header) {
      current = header[1];
      jobs.set(current, []);
      continue;
    }
    if (current) jobs.get(current).push(line);
  }
  return jobs;
}

// Collects the shell of a job: inline `run: cmd` and block `run: |` bodies.
function runBodies(bodyLines) {
  const collected = [];
  for (let i = 0; i < bodyLines.length; i += 1) {
    const line = bodyLines[i];
    const inline = line.match(/^\s*run:\s*(?!\||>)(\S.*)$/);
    if (inline) {
      collected.push(inline[1]);
      continue;
    }
    const block = line.match(/^(\s*)run:\s*[|>][-+]?\s*$/);
    if (!block) continue;
    const indent = block[1].length;
    for (let j = i + 1; j < bodyLines.length; j += 1) {
      const next = bodyLines[j];
      if (next.trim() === '') {
        collected.push('');
        continue;
      }
      const nextIndent = next.match(/^\s*/)[0].length;
      if (nextIndent <= indent) break;
      collected.push(next);
      i = j;
    }
  }
  return collected.join('\n');
}

const source = readFileSync(workflowPath, 'utf8');
const jobs = parseJobs(source);
const problems = [];

// A parse that finds no jobs is the check breaking, not the workflow passing.
if (jobs.size === 0) {
  console.error(`No jobs parsed out of ${workflowPath} — the parser is broken, not the workflow.`);
  process.exit(1);
}

const cargoFree = [...jobs.entries()]
  .filter(([, body]) => !/\bcargo\b/.test(runBodies(body)))
  .map(([job]) => job);
const cargoBearing = [...jobs.keys()].filter((job) => !cargoFree.includes(job));

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
for (const gate of GATES) {
  if (!jobs.has(gate.job)) {
    problems.push(`run-local-gates.mjs mirrors \`${gate.job}\`, which is not a job in regression.yml — renamed or removed.`);
  } else if (!cargoFree.includes(gate.job)) {
    problems.push(`run-local-gates.mjs mirrors \`${gate.job}\`, which now invokes cargo — it no longer belongs in a seconds-budget hook.`);
  }
}

for (const job of EXCLUDED.keys()) {
  if (!jobs.has(job)) {
    problems.push(`run-local-gates.mjs excludes \`${job}\`, which is not a job in regression.yml — remove or fix the entry.`);
  }
}

// --- cargo half: mirrored by a command the hook still invokes, or nowhere ---

// Comments are stripped before grepping the hook for the same reason they are
// stripped from the workflow: this file's own prose names `cargo clippy`, and a
// check satisfied by a sentence about a command is satisfied by nothing.
const hookCommands = readFileSync(hookPath, 'utf8')
  .split('\n')
  .filter((line) => !/^\s*#/.test(line))
  .join('\n');

for (const job of cargoBearing) {
  if (!CARGO_JOBS.has(job)) {
    problems.push(
      `${job} is a cargo job of regression.yml that CARGO_JOBS in run-local-gates.mjs does not account for. `
        + 'Give it a `hook` command the pre-push hook runs, or an `uncovered` reason saying it runs nowhere.',
    );
  }
}

for (const [job, where] of CARGO_JOBS) {
  if (!jobs.has(job)) {
    problems.push(`CARGO_JOBS accounts for \`${job}\`, which is not a job in regression.yml — renamed or removed.`);
    continue;
  }
  if (!cargoBearing.includes(job)) {
    problems.push(`CARGO_JOBS accounts for \`${job}\`, which no longer invokes cargo — it belongs in GATES or EXCLUDED now.`);
    continue;
  }
  const claims = ['hook', 'uncovered'].filter((key) => where[key]);
  if (claims.length !== 1) {
    problems.push(
      `CARGO_JOBS entry \`${job}\` claims ${claims.length === 0 ? 'neither `hook` nor' : 'both `hook` and'} \`uncovered\` — `
        + 'a gate runs in exactly one place, or in none of them.',
    );
    continue;
  }
  if (where.hook && !hookCommands.includes(where.hook)) {
    problems.push(
      `CARGO_JOBS says \`${job}\` is mirrored by \`${where.hook}\` in .githooks/pre-push, which no longer invokes it. `
        + 'Restore the command, or move the entry to `uncovered` with the reason.',
    );
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

const hookMirrored = [...CARGO_JOBS.values()].filter((where) => where.hook).length;
const uncovered = [...CARGO_JOBS.values()].filter((where) => where.uncovered).length;

console.log(
  `local gate coverage: ${jobs.size} job(s) in regression.yml — `
    + `${mirrored.size} mirrored by run-local-gates.mjs, ${hookMirrored} by pre-push, `
    + `${EXCLUDED.size} excluded by design, ${uncovered} running nowhere`,
);
