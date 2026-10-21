#!/usr/bin/env node

// Refuses a push to `main` while the last finished Regression run on `main` is
// red (card_5b91d8c6f3c2).
//
// Why this exists: the push gate (`scripts/verify-push-gates.sh`) cannot run
// what needs a live database or the network — `postgres-smoke`, `mysql-smoke`,
// `security-audit` — and says so on every run. Pushes go straight to `main`,
// without a pull request whose status would show the red. So between 20.10 and
// 21.10 ten commits landed on top of a failing MySQL smoke, each push green
// locally, and nobody looked at the run before theirs. The next failure then
// hides behind the one already there.
//
// So `.githooks/pre-push` asks GitHub, for a push to `main`, how the last
// finished run went:
//
// * red (`failure`, `timed_out`, `startup_failure`) — the run and its failed
//   jobs are printed and the push is refused, unless `PUSH_OVER_RED_MAIN=1`
//   says the pusher saw it (typically: this push is the fix);
// * green, or no finished run — the push goes on;
// * `cancelled` runs are skipped: regression.yml cancels a run a newer push
//   superseded, and that says nothing about the commit;
// * no `gh`, no login, no network — a warning, never a refusal. The gate must
//   not depend on GitHub being reachable; it exists to make a red main visible.

import { spawnSync } from 'node:child_process';

const WORKFLOW = 'regression.yml';
const BRANCH = 'main';
const RED = new Set(['failure', 'timed_out', 'startup_failure']);
const OVERRIDE = 'PUSH_OVER_RED_MAIN';
const TIMEOUT_MS = 20_000;

function gh(args) {
  const result = spawnSync('gh', args, { encoding: 'utf8', timeout: TIMEOUT_MS });
  if (result.error?.code === 'ENOENT') return { missing: true };
  if (result.error) return { failure: result.error.message };
  if (result.status !== 0) {
    return { failure: (result.stderr || result.stdout || `exit ${result.status}`).trim() };
  }
  try {
    return { value: JSON.parse(result.stdout) };
  } catch (error) {
    return { failure: `unreadable answer from gh: ${error.message}` };
  }
}

function blind(reason) {
  console.warn(
    `⚠️  pre-push: cannot see whether the last ${WORKFLOW} run on ${BRANCH} is red — ${reason}. `
      + 'Pushing anyway; check the Actions tab before you stack another commit on it.',
  );
  process.exit(0);
}

const runs = gh([
  'run', 'list',
  '--workflow', WORKFLOW,
  '--branch', BRANCH,
  '--status', 'completed',
  '--limit', '20',
  '--json', 'databaseId,conclusion,headSha,url,createdAt',
]);
if (runs.missing) blind('`gh` is not installed');
if (runs.failure !== undefined) blind(`gh run list failed: ${runs.failure}`);
if (!Array.isArray(runs.value)) blind('gh run list did not answer with a list');

const last = runs.value.find((run) => run.conclusion !== 'cancelled' && run.conclusion !== 'skipped');
if (last === undefined || !RED.has(last.conclusion)) process.exit(0);

const jobs = gh(['run', 'view', String(last.databaseId), '--json', 'jobs']);
const failed = (jobs.value?.jobs ?? [])
  .filter((job) => RED.has(job.conclusion))
  .map((job) => job.name);

const overridden = process.env[OVERRIDE] === '1';
const report = [
  `${overridden ? '⚠️ ' : '❌'} pre-push: the last ${WORKFLOW} run on ${BRANCH} is ${last.conclusion} — `
    + `${String(last.headSha ?? '').slice(0, 8)}, ${last.createdAt}`,
  `   ${last.url}`,
  failed.length > 0
    ? `   failed: ${failed.join(', ')}`
    : '   (could not list its failed jobs)',
];
if (overridden) {
  console.warn([...report, `   ${OVERRIDE}=1 — pushing over it.`].join('\n'));
  process.exit(0);
}
console.error(
  [
    ...report,
    '',
    '   A push on top of a red main hides the next failure behind this one.',
    '   Fix it in this push, then say you saw it:',
    `     ${OVERRIDE}=1 git push`,
  ].join('\n'),
);
process.exit(1);
