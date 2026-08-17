#!/usr/bin/env node

// Asserts that every `.github/workflows/*.yml` is YAML a parser will actually
// load, and that the steps this repository depends on survive that load intact.
//
// Why this exists: `regression.yml` did not parse for weeks. Four `run:` lines
// ended a cargo filter as `server_migration_serialization:: -- --ignored`, and a
// plain YAML scalar may not contain `: ` — the second colon opens a mapping, so
// the document died at line 252 and GitHub could load NONE of the twelve jobs
// declared below it. The gates were not failing; they did not exist. A file that
// cannot be parsed and a file whose jobs all pass produce the same thing on a PR
// that never runs: nothing.
//
// Nothing in the repository noticed, and that is the part worth fixing. The
// only earlier check that read the workflow — `local-gate-coverage-contract-
// check.mjs` — recovered jobs with line regexes, so it cheerfully reported "12
// job(s) in regression.yml" about a document no parser accepts. Both readers
// now share the parsed workflow boundary below: a regex cannot distinguish
// valid YAML from text that merely looks like it.
//
// The file list is a GLOB, for the reason the contract-check runner gives about
// its own: a workflow added tomorrow must be covered by this the same day.

import { readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  parseWorkflowFile,
  selectWorkflowParser,
  workflowJobRuns,
} from './lib/workflow.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const workflowsDir = resolve(root, '.github/workflows');

// Commands that must still be in the parsed job graph, byte for byte.
//
// The parse alone does not cover this. Quoting is not the only way to make the
// document load again: deleting the `::` suffix also makes it parse, and turns
// `--test integration server_migration_serialization::` — one module of one test
// binary — into a substring filter over everything. That mutation is green
// everywhere except in what the job actually proves, so the arguments are pinned
// here rather than trusted. The Grafana checker is pinned for the same reason:
// parsing its own script proves nothing if the observability job stops invoking
// it, so that wiring is part of the parsed job-graph contract too.
//
// This is a ratchet, not a list to grow at will: a pin whose job or command is
// gone fails the check, so these steps cannot be renamed into silence. Change
// one of these commands on purpose and you update its pin in the same commit.
const PINNED_STEPS = [
  {
    workflow: 'regression.yml',
    job: 'postgres-smoke',
    run: 'cargo test -p rg-db -j 6 --test integration server_migration_serialization:: -- --ignored --nocapture',
  },
  {
    workflow: 'regression.yml',
    job: 'postgres-smoke',
    run: 'cargo test -p rg-db -j 6 --test integration password_reset_token_single_use:: -- --nocapture',
  },
  {
    workflow: 'regression.yml',
    job: 'mysql-smoke',
    run: 'cargo test -p rg-db -j 6 --test integration server_migration_serialization:: -- --ignored --nocapture',
  },
  {
    workflow: 'regression.yml',
    job: 'mysql-smoke',
    run: 'cargo test -p rg-db -j 6 --test integration password_reset_token_single_use:: -- --nocapture',
  },
  {
    workflow: 'regression.yml',
    job: 'observability-config',
    run: 'node scripts/grafana-provisioning-contract-check.mjs',
  },
];

// The parser selection is shared with Grafana provisioning so both gates keep
// the same fail-loud Python/Ruby fallback and syntax diagnostics.
const { parser, missing } = selectWorkflowParser();

// A gate that cannot run is not a gate that passed: refuse loudly rather than
// exit green on a machine where nothing could have been checked.
if (!parser) {
  console.error(
    `No YAML parser available (tried ${missing.join(', ')}) — install either, `
      + 'or this check cannot tell a valid workflow from an unparseable one.',
  );
  process.exit(1);
}

let workflows;
try {
  workflows = readdirSync(workflowsDir)
    .filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
    .sort();
} catch (error) {
  console.error(`Cannot read ${workflowsDir} — ${error.message}`);
  process.exit(1);
}

// An empty glob is the check breaking, not the repository passing.
if (workflows.length === 0) {
  console.error(`No *.yml/*.yaml found in ${workflowsDir} — the glob is broken, or the workflows are gone.`);
  process.exit(1);
}

const problems = [];
const parsed = new Map();

for (const workflow of workflows) {
  const result = parseWorkflowFile(parser, join(workflowsDir, workflow));

  if (!result.ok && result.kind === 'spawn') {
    console.error(`${parser.name} could not be run on ${workflow} — ${result.message}`);
    process.exit(1);
  }
  if (!result.ok && result.kind === 'syntax') {
    problems.push(
      `.github/workflows/${workflow} is not valid YAML — ${parser.name} rejects it:\n     `
        + result.diagnostic.split('\n').join('\n     '),
    );
    continue;
  }
  if (!result.ok && result.kind === 'parser') {
    console.error(`${parser.name} failed on ${workflow} (exit ${result.status})\n${result.diagnostic}`);
    process.exit(1);
  }
  if (!result.ok) {
    console.error(`${parser.name} produced output this check cannot read for ${workflow} — ${result.message}`);
    process.exit(1);
  }
  parsed.set(workflow, result);
}

// A document that parses into nothing usable is a workflow GitHub loads and then
// runs no job from, which is the same outcome as a syntax error one step later.
for (const [workflow, parsedWorkflow] of parsed) {
  const jobs = parsedWorkflow.jobs;
  if (!jobs || typeof jobs !== 'object' || Array.isArray(jobs) || Object.keys(jobs).length === 0) {
    problems.push(`.github/workflows/${workflow} parses, but declares no jobs — nothing in it can run.`);
    continue;
  }

  for (const [job, definition] of Object.entries(jobs)) {
    const inspected = workflowJobRuns(definition);
    if (inspected.invalidSteps) {
      problems.push(`${workflow}: job \`${job}\` has a \`steps\` that is not a list — it declares no runnable step.`);
      continue;
    }
    for (const invalid of inspected.invalidRuns) {
      // A `run` that parsed into something other than a string is a scalar the
      // author meant as a command and YAML read as data — the quiet half of the
      // same defect, since the document still loads.
      const where = invalid.name ? `step \`${invalid.name}\`` : `step #${invalid.index + 1}`;
      problems.push(
        `${workflow}: job \`${job}\`, ${where} has a \`run\` that parsed as `
          + `${invalid.value === null ? 'empty' : typeof invalid.value}, not a shell command.`,
      );
    }
  }
}

for (const pin of PINNED_STEPS) {
  if (!workflows.includes(pin.workflow)) {
    problems.push(`A pinned step names ${pin.workflow}, which is not a workflow file — remove or fix the pin.`);
    continue;
  }
  // The file exists but did not parse: that failure is already reported above,
  // and every pin in it would otherwise pile on a second, misleading finding
  // that sends the reader to edit the pin instead of the YAML.
  const parsedWorkflow = parsed.get(pin.workflow);
  if (!parsedWorkflow) continue;
  const job = parsedWorkflow.jobs?.[pin.job];
  if (!job) {
    problems.push(`A pinned step names job \`${pin.job}\` of ${pin.workflow}, which no longer exists — remove or fix the pin.`);
    continue;
  }
  const commands = workflowJobRuns(job).runs.map((command) => command.trim());
  if (!commands.includes(pin.run)) {
    problems.push(
      `${pin.workflow}: job \`${pin.job}\` no longer runs the pinned command\n     `
        + `${pin.run}\n     `
        + 'Restore it, or update the pin in this file if the change was intended.',
    );
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

console.log(
  `workflow yaml: ${workflows.length} workflow file(s) parse under ${parser.name}, `
    + `${PINNED_STEPS.length} pinned step(s) intact`,
);
