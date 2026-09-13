#!/usr/bin/env node

// The receipt-producing verifier and CI must agree on warning-free Rust gates.
// A warning that leaves either command at exit zero is not a green gate.

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseWorkflowFile, selectWorkflowParser, workflowJobRuns } from './lib/workflow.mjs';
import { shellCodeOnly } from './lib/shell-source.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const verifier = shellCodeOnly(readFileSync(resolve(root, 'scripts/verify-push-gates.sh'), 'utf8'));
const workflowPath = resolve(root, '.github/workflows/regression.yml');
const { parser, missing } = selectWorkflowParser();

if (!parser) {
  console.error(`No YAML parser available (tried ${missing.join(', ')}) — warning policy cannot inspect regression.yml.`);
  process.exit(1);
}

const parsed = parseWorkflowFile(parser, workflowPath);
if (!parsed.ok) {
  console.error(`Cannot inspect warning policy in regression.yml — ${parsed.message ?? parsed.diagnostic}`);
  process.exit(1);
}

const jobCommands = (name) => {
  const job = parsed.jobs?.[name];
  if (!job) return '';
  return shellCodeOnly(workflowJobRuns(job).runs.join('\n'));
};

const requiredVerifierCommands = [
  'run_warning_free cargo fmt --all -- --check',
  'run_warning_free cargo clippy --workspace --all-targets -j 6 -- -D warnings',
  'run_warning_free env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps -j 6',
  'run_warning_free node scripts/run-local-gates.mjs',
];

const problems = [];
for (const command of requiredVerifierCommands) {
  if (!verifier.includes(command)) {
    problems.push(`scripts/verify-push-gates.sh does not run warning-free command: ${command}`);
  }
}

const clippy = jobCommands('clippy');
if (!clippy.includes('cargo clippy --workspace --all-targets -j 6 -- -D warnings')) {
  problems.push('regression.yml `clippy` no longer denies every warning with the workspace command.');
}

const docs = jobCommands('docs');
if (!docs.includes('RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps -j 6')) {
  problems.push('regression.yml `docs` no longer denies every rustdoc warning with the workspace command.');
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

console.log('✅ push receipt and CI Rust gates reject warning diagnostics');
