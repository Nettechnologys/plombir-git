#!/usr/bin/env node

// Copied-tree mutation stand for the cheap first-user journey wiring contract.
// Every textual claim must disappear when its only remaining spelling is in a
// shell or JavaScript comment. Valid hashes in shell data and `//` inside a
// JavaScript string must remain visible without confusing the source views.

import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const copied = [
  'scripts/first-user-journey-contract-check.mjs',
  'scripts/first-user-journey-e2e.sh',
  'scripts/first-user-journey-e2e.mjs',
  'scripts/ephemeral-stand.sh',
  'scripts/lib/js-source.mjs',
  'scripts/lib/shell-source.mjs',
  'scripts/lib/ts-source.mjs',
  'web/package.json',
];

function baseline(fixture) {
  for (const path of copied) {
    const target = join(fixture, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, path), target);
  }
}

function mutate(fixture, path, from, to) {
  const target = join(fixture, path);
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) {
    throw new Error(`fixture mutation cannot find ${JSON.stringify(from)} in ${path}`);
  }
  writeFileSync(target, source.replace(from, to));
}

function mutateEvery(fixture, path, from, to, expectedCount) {
  const target = join(fixture, path);
  const source = readFileSync(target, 'utf8');
  const count = source.split(from).length - 1;
  if (count !== expectedCount) {
    throw new Error(
      `fixture mutation expected ${expectedCount} occurrence(s) of ${JSON.stringify(from)} in ${path}, found ${count}`,
    );
  }
  writeFileSync(target, source.split(from).join(to));
}

function run(fixture) {
  const result = spawnSync(
    process.execPath,
    [join(fixture, 'scripts/first-user-journey-contract-check.mjs')],
    {
      cwd: fixture,
      env: { ...process.env, PLOMBIR_GIT_FIRST_USER_JOURNEY_ROOT: fixture },
      encoding: 'utf8',
    },
  );
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const INLINE_COMMENT_MUTATIONS = [
  {
    name: 'the two-stand default survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
      'JOURNEY_RUNS=1 # JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
    ),
    expect: 'defaults to two clean stands',
  },
  {
    name: 'the requested-stand loop survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'for run in $(seq 1 "${JOURNEY_RUNS}"); do',
      'for run in 1; do # seq 1 "${JOURNEY_RUNS}"',
    ),
    expect: 'no longer loops over every requested clean stand',
  },
  {
    name: 'fresh-frontend wiring survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      '  STAND_REBUILD_FRONTEND=1 "${ROOT_DIR}/scripts/ephemeral-stand.sh" \\\n',
      '  STAND_REBUILD_FRONTEND=0 true # STAND_REBUILD_FRONTEND=1 "${ROOT_DIR}/scripts/ephemeral-stand.sh" \\\n',
    ),
    expect: 'may serve a stale web/build',
  },
  {
    name: 'the empty-stand browser invocation starts only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      '  STAND_REBUILD_FRONTEND=1 "${ROOT_DIR}/scripts/ephemeral-stand.sh" \\\n',
      '  true # STAND_REBUILD_FRONTEND=1 "${ROOT_DIR}/scripts/ephemeral-stand.sh" \\\n',
    ),
    expect: 'without pre-registering its user',
  },
  {
    name: 'the founder-registration default survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/ephemeral-stand.sh',
      'REGISTER_FOUNDER=1',
      'REGISTER_FOUNDER=0 # REGISTER_FOUNDER=1',
    ),
    expect: 'lost the founder-registration default',
  },
  {
    name: 'the --no-founder state transition survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/ephemeral-stand.sh',
      '    --no-founder) REGISTER_FOUNDER=0 ;;',
      '    --no-founder) true ;; # --no-founder) REGISTER_FOUNDER=0',
    ),
    expect: 'no longer accepts --no-founder',
  },
  {
    name: 'the founder call guard survives only in a shell inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/ephemeral-stand.sh',
      'if [[ ${REGISTER_FOUNDER} -eq 1 ]]; then',
      'if false; then # if [[ ${REGISTER_FOUNDER} -eq 1 ]]; then',
    ),
    expect: 'no longer controls the founder registration call',
  },
  {
    name: 'the browser username control survives only in a JavaScript inline comment',
    apply: (fixture) => {
      mutateEvery(
        fixture,
        'scripts/first-user-journey-e2e.mjs',
        'input[autocomplete="username"]',
        'input[data-journey-username]',
        2,
      );
      mutate(
        fixture,
        'scripts/first-user-journey-e2e.mjs',
        '    allowLoggedOut401 = false;',
        '    allowLoggedOut401 = false; // input[autocomplete="username"]',
      );
    },
    expect: 'no longer drives the registration/login username control',
  },
  {
    name: 'the HttpOnly-cookie handoff survives only in a JavaScript inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "    const cookies = await tab.send('Network.getCookies', { urls: [FRONTEND_URL] });",
      "    const cookies = { cookies: [] }; // const cookies = await tab.send('Network.getCookies', { urls: [FRONTEND_URL] });",
    ),
    expect: 'no longer takes the real HttpOnly session into git',
  },
  {
    name: 'the real git push survives only in a JavaScript block comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "  git(['-C', seed, 'push', '-q', remote, 'main'], {",
      "  git(['-C', seed, 'fetch', '-q', remote, 'main'], { /* git(['-C', seed, 'push' */",
    ),
    expect: 'no longer performs a real git push',
  },
  {
    name: 'the blob-link selector survives only in a JavaScript inline comment',
    apply: (fixture) => {
      mutateEvery(
        fixture,
        'scripts/first-user-journey-e2e.mjs',
        'a.file-entry',
        'a.missing-file-entry',
        2,
      );
      mutate(
        fixture,
        'scripts/first-user-journey-e2e.mjs',
        "  await step('open the pushed file through the blob UI', async () => {",
        "  await step('open the pushed file through the blob UI', async () => { // a.file-entry",
      );
    },
    expect: 'no longer opens a repository file through the blob link',
  },
  {
    name: 'the issue-close action survives only in a JavaScript inline comment',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "    await click(tab, 'button.btn-close');",
      "    await click(tab, 'button.btn-primary'); // await click(tab, 'button.btn-close');",
    ),
    expect: 'no longer closes the created issue through the UI',
  },
];

const DIRECT_MUTATIONS = [
  {
    name: 'the runner defaults to one stand',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-1}',
    ),
    expect: 'defaults to two clean stands',
  },
  {
    name: 'the runner stops rebuilding the frontend',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'STAND_REBUILD_FRONTEND=1 ',
      '',
    ),
    expect: 'may serve a stale web/build',
  },
  {
    name: 'the runner pre-registers its browser user',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      '    --no-founder \\\n',
      '',
    ),
    expect: 'without pre-registering its user',
  },
  {
    name: 'the stand ignores --no-founder',
    apply: (fixture) => mutate(
      fixture,
      'scripts/ephemeral-stand.sh',
      '--no-founder) REGISTER_FOUNDER=0',
      '--no-founder) REGISTER_FOUNDER=1',
    ),
    expect: 'no longer accepts --no-founder',
  },
  {
    name: 'the browser journey fetches instead of pushing',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "git(['-C', seed, 'push'",
      "git(['-C', seed, 'fetch'",
    ),
    expect: 'no longer performs a real git push',
  },
  {
    name: 'the browser journey no longer closes its issue',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "await click(tab, 'button.btn-close');",
      "await click(tab, 'button.btn-primary');",
    ),
    expect: 'no longer closes the created issue',
  },
];

const VALID_VARIANTS = [
  {
    name: 'a quoted shell hash before the live two-stand default',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
      "PROBE='# two stands'; JOURNEY_RUNS=${JOURNEY_RUNS:-2}",
    ),
  },
  {
    name: 'an assignment hash before the live two-stand default',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
      'PROBE=two#stands; JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
    ),
  },
  {
    name: 'a parameter-expansion hash before the live two-stand default',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.sh',
      'JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
      ': "${PROBE:-# two stands}"; JOURNEY_RUNS=${JOURNEY_RUNS:-2}',
    ),
  },
  {
    name: 'a JavaScript URL string before the live cookie handoff',
    apply: (fixture) => mutate(
      fixture,
      'scripts/first-user-journey-e2e.mjs',
      "    const cookies = await tab.send('Network.getCookies', { urls: [FRONTEND_URL] });",
      "    const probe = 'https://plombir-git.invalid/session'; void probe; const cookies = await tab.send('Network.getCookies', { urls: [FRONTEND_URL] });",
    ),
  },
];

let fixture = scratchDir(join(tmpdir(), 'plombir-git-first-user-contract.'));
try {
  baseline(fixture);
  const clean = run(fixture);
  if (clean.status !== 0) {
    console.error(`❌ first-user journey baseline fixture is red, so mutations prove nothing:\n${clean.output}`);
    process.exit(1);
  }

  for (const mutation of [...INLINE_COMMENT_MUTATIONS, ...DIRECT_MUTATIONS]) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = scratchDir(join(tmpdir(), 'plombir-git-first-user-contract.'));
    baseline(fixture);
    mutation.apply(fixture);
    const result = run(fixture);
    if (result.status === 0 || !result.output.includes(mutation.expect)) {
      console.error(
        `❌ mutation did not go red by name: ${mutation.name}\n`
          + `expected ${JSON.stringify(mutation.expect)}, status=${result.status}\n${result.output}`,
      );
      process.exit(1);
    }
    console.log(`✅ mutation rejected: ${mutation.name}`);
  }

  for (const variant of VALID_VARIANTS) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = scratchDir(join(tmpdir(), 'plombir-git-first-user-contract.'));
    baseline(fixture);
    variant.apply(fixture);
    const result = run(fixture);
    if (result.status !== 0) {
      console.error(`❌ valid source variant went red: ${variant.name}\n${result.output}`);
      process.exit(1);
    }
    console.log(`✅ valid source variant accepted: ${variant.name}`);
  }
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
