#!/usr/bin/env node

// The three advisory scanners in `security-audit` must accept the same
// advisories, and osv-scanner must be able to fail the job.
//
// cargo-audit reads `.cargo/audit.toml`, cargo-deny reads `deny.toml`,
// osv-scanner reads `osv-scanner.toml`. An id waived in one list and not the
// others is either a decision nobody wrote down or a stale pre-approval, and a
// list that stops matching is how a scanner ends up silently stricter or looser
// than the comment above it claims.
//
// osv-scanner is the only one of the three that sees GHSA-only advisories. It
// ran with `continue-on-error` for weeks while it printed a pre-auth panic in
// the public SSH server's russh (card_95a9eb65f6d4); the workflow half of this
// check keeps it a gate.
//
// ADVISORY_IGNORE_ROOT points the check at a fixture tree (the regression stand).

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { parseWorkflowFile, selectWorkflowParser } from './lib/workflow.mjs';

const root = resolve(process.env.ADVISORY_IGNORE_ROOT || '.');
const AUDIT = '.cargo/audit.toml';
const DENY = 'deny.toml';
const OSV = 'osv-scanner.toml';
const WORKFLOW = '.github/workflows/regression.yml';
const JOB = 'security-audit';

const problems = [];

// tomllib is in the Python standard library from 3.11; every CI image and the
// workstation have it. No TOML-shaped regex: a list split over lines or a
// trailing comment would read as a different set.
const TOML_PROGRAM = `
import json, sys, tomllib
with open(sys.argv[1], "rb") as handle:
    json.dump(tomllib.load(handle), sys.stdout)
`;

function readToml(relative) {
  const file = join(root, relative);
  if (!existsSync(file)) {
    problems.push(`${relative}: missing.`);
    return null;
  }
  const result = spawnSync('python3', ['-c', TOML_PROGRAM, file], { encoding: 'utf8' });
  if (result.error || result.status !== 0) {
    problems.push(`${relative}: could not be parsed as TOML: ${(result.stderr || result.error?.message || '').trim()}`);
    return null;
  }
  return JSON.parse(result.stdout);
}

function idList(relative, value) {
  if (!Array.isArray(value) || value.some((id) => typeof id !== 'string' || id.trim() === '')) {
    problems.push(`${relative}: the ignore list is not a list of advisory ids.`);
    return null;
  }
  const seen = new Set();
  for (const id of value) {
    if (seen.has(id)) problems.push(`${relative}: ${id} is listed twice.`);
    seen.add(id);
  }
  return seen;
}

const audit = readToml(AUDIT);
const deny = readToml(DENY);
const osv = readToml(OSV);

const lists = [];
if (audit) {
  const ids = idList(AUDIT, audit.advisories?.ignore);
  if (ids) lists.push({ file: AUDIT, ids });
}
if (deny) {
  // cargo-deny takes either a bare id or `{ id, reason }`.
  const raw = deny.advisories?.ignore;
  const ids = idList(DENY, Array.isArray(raw) ? raw.map((entry) => (typeof entry === 'object' && entry ? entry.id : entry)) : raw);
  if (ids) lists.push({ file: DENY, ids });
}
if (osv) {
  const entries = osv.IgnoredVulns ?? [];
  if (!Array.isArray(entries)) {
    problems.push(`${OSV}: IgnoredVulns is not an array of tables.`);
  } else {
    for (const [index, entry] of entries.entries()) {
      if (typeof entry?.reason !== 'string' || entry.reason.trim() === '') {
        problems.push(`${OSV}: IgnoredVulns[${index}] (${entry?.id ?? 'no id'}) carries no reason — say why it is accepted.`);
      }
      if (entry?.ignoreUntil !== undefined) {
        problems.push(`${OSV}: IgnoredVulns[${index}] (${entry?.id}) sets ignoreUntil, which the other two lists cannot express.`);
      }
    }
    const ids = idList(OSV, entries.map((entry) => entry?.id));
    if (ids) lists.push({ file: OSV, ids });
  }
}

if (lists.length === 3) {
  const union = new Set(lists.flatMap(({ ids }) => [...ids]));
  for (const id of [...union].sort()) {
    const missing = lists.filter(({ ids }) => !ids.has(id)).map(({ file }) => file);
    if (missing.length) {
      const present = lists.filter(({ ids }) => ids.has(id)).map(({ file }) => file);
      problems.push(`${id} is accepted in ${present.join(', ')} but not in ${missing.join(', ')}.`);
    }
  }
}

// The workflow half: osv-scanner runs in security-audit, reads this config,
// and nothing lets its failure pass.
const { parser, missing } = selectWorkflowParser();
if (!parser) {
  problems.push(`no YAML parser available (tried ${missing.join(', ')}).`);
} else {
  const parsed = parseWorkflowFile(parser, join(root, WORKFLOW));
  const steps = parsed.ok ? parsed.jobs?.[JOB]?.steps : null;
  if (!parsed.ok) {
    problems.push(`${WORKFLOW}: could not be parsed (${parsed.kind}): ${parsed.message ?? parsed.diagnostic}`);
  } else if (!Array.isArray(steps)) {
    problems.push(`${WORKFLOW}: job \`${JOB}\` has no steps.`);
  } else {
    if (parsed.jobs[JOB]['continue-on-error'] !== undefined) {
      problems.push(`${WORKFLOW}: job \`${JOB}\` sets continue-on-error — its scanners would not stop anything.`);
    }
    const osvSteps = steps.filter((step) => /osv-scanner/.test(`${step?.run ?? ''} ${step?.with?.tool ?? ''}`));
    const scans = osvSteps.filter((step) => typeof step.run === 'string');
    if (!osvSteps.some((step) => step.with?.tool !== undefined)) {
      problems.push(`${WORKFLOW}: job \`${JOB}\` does not install osv-scanner.`);
    }
    if (scans.length === 0) {
      problems.push(`${WORKFLOW}: job \`${JOB}\` never runs osv-scanner.`);
    }
    for (const step of osvSteps) {
      if (step['continue-on-error'] !== undefined) {
        problems.push(`${WORKFLOW}: step "${step.name}" sets continue-on-error — an OSV finding would not fail the job.`);
      }
    }
    for (const step of scans) {
      if (!/--config[= ]osv-scanner\.toml(\s|$)/.test(step.run)) {
        problems.push(`${WORKFLOW}: step "${step.name}" does not pass --config=osv-scanner.toml.`);
      }
      if (!/--lockfile[= ]Cargo\.lock(\s|$)/.test(step.run)) {
        problems.push(`${WORKFLOW}: step "${step.name}" does not scan --lockfile=Cargo.lock.`);
      }
      if (/\|\||;\s*true|\bexit 0\b/.test(step.run)) {
        problems.push(`${WORKFLOW}: step "${step.name}" can swallow the scanner's exit status.`);
      }
    }
  }
}

if (problems.length) {
  console.error('❌ advisory ignore lists / osv-scanner gate:');
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}
console.log(`advisory ignore lists agree (${lists[0].ids.size} accepted) and osv-scanner gates security-audit`);
