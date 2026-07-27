#!/usr/bin/env node

// Runs every `scripts/*-contract-check.mjs` and fails on the first unexpected result.
//
// Why this exists: the contract checks are the repo's cheapest gate against
// frontend/backend drift, but until this runner none of them was wired into CI.
// A gate nobody executes is a comment the compiler does not verify — a refactor
// moves a file or renames a symbol, the check silently rots, and the rot is only
// discovered months later when somebody runs it by hand. That has now happened
// four times (a barrel split rotted 24 checks, a router move rotted 8).
//
// The check list is a GLOB, never a hand-maintained array: a hand-written list
// is the same trap one level up — a new check would land outside CI and rot the
// same way.

import { readdirSync, appendFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');

// Checks that are red on a clean main for a reason already tracked on a card.
// They still RUN — they are just not allowed to fail the job yet, so the gate
// can be switched on today instead of waiting for the backlog to drain.
//
// This list is a ratchet, not a dumping ground:
//   - a quarantined check that starts PASSING fails the job (remove it here);
//   - an entry naming a file that no longer exists fails the job;
//   - nothing gets added here without a card id explaining the red.
const QUARANTINE = new Map([
  [
    'release-assets-contract-check.mjs',
    'card_0c926648b67d — the check greps authorization symbols that were renamed',
  ],
  [
    'webhooks-contract-check.mjs',
    'card_71260b04bb85 — the check greps a helper that was removed and an old binding name',
  ],
]);

const checks = readdirSync(scriptsDir)
  .filter((name) => name.endsWith('-contract-check.mjs'))
  .sort();

if (checks.length === 0) {
  console.error(`No *-contract-check.mjs found in ${scriptsDir} — the glob is broken, not the repo.`);
  process.exit(1);
}

const passed = [];
const knownFailures = [];
const failures = [];
const unexpectedPasses = [];

for (const check of checks) {
  const result = spawnSync(process.execPath, [join(scriptsDir, check)], {
    cwd: root,
    encoding: 'utf8',
  });

  // A spawn that never produced an exit code (ENOENT, killed by a signal) is a
  // failure of the runner itself; treat it as red rather than silently green.
  const ok = result.status === 0;
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`.trimEnd();
  const quarantined = QUARANTINE.has(check);

  if (ok && !quarantined) {
    passed.push(check);
    console.log(`✅ ${check}`);
  } else if (ok && quarantined) {
    unexpectedPasses.push(check);
    console.log(`🎉 ${check} — quarantined but PASSING`);
  } else if (quarantined) {
    knownFailures.push(check);
    console.log(`🟡 ${check} — known red, ${QUARANTINE.get(check)}`);
  } else {
    failures.push({ check, output, status: result.status, signal: result.signal });
    console.log(`❌ ${check}`);
  }
}

// A quarantine entry for a check that no longer exists means the exemption is
// stale — most likely the check was renamed, which would silently move it back
// into the blocking set under a new name while this entry keeps pretending to
// cover it.
const staleQuarantine = [...QUARANTINE.keys()].filter((check) => !checks.includes(check));

for (const { check, output, status, signal } of failures) {
  const reason = signal ? `killed by ${signal}` : `exit ${status}`;
  console.error(`\n----- ${check} (${reason}) -----\n${output}`);
}

for (const check of staleQuarantine) {
  console.error(`\n❌ QUARANTINE names ${check}, which does not exist — remove or fix the entry.`);
}

for (const check of unexpectedPasses) {
  console.error(`\n❌ ${check} is quarantined but passes — remove it from QUARANTINE so it starts gating.`);
}

const fatal = failures.length + staleQuarantine.length + unexpectedPasses.length;
const summary =
  `${passed.length}/${checks.length} contract checks green` +
  (knownFailures.length > 0 ? `, ${knownFailures.length} known red (quarantined)` : '') +
  (failures.length > 0 ? `, ${failures.length} FAILED` : '') +
  (unexpectedPasses.length > 0 ? `, ${unexpectedPasses.length} quarantined-but-passing` : '') +
  (staleQuarantine.length > 0 ? `, ${staleQuarantine.length} stale quarantine entr${staleQuarantine.length === 1 ? 'y' : 'ies'}` : '');

console.log(`\n${fatal === 0 ? '✅' : '❌'} ${summary}`);

// Surface the same one-liner on the PR's checks tab, so a red gate is readable
// without opening the job log.
if (process.env.GITHUB_STEP_SUMMARY) {
  const detail = [...failures.map(({ check }) => `- ❌ \`${check}\``),
    ...staleQuarantine.map((check) => `- ❌ stale quarantine entry: \`${check}\``),
    ...unexpectedPasses.map((check) => `- ❌ quarantined but passing: \`${check}\``),
    ...knownFailures.map((check) => `- 🟡 \`${check}\` — ${QUARANTINE.get(check)}`)];
  appendFileSync(
    process.env.GITHUB_STEP_SUMMARY,
    `### Contract checks\n\n${summary}\n\n${detail.join('\n')}\n`,
  );
}

process.exit(fatal === 0 ? 0 : 1);
