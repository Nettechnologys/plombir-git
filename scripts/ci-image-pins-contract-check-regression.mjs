#!/usr/bin/env node

// Does `scripts/ci-image-pins-contract-check.mjs` still bite?
//
// The fixtures are written in the vocabulary of the defect the check stands
// for: an image pinned where no updater looks. A Dependabot bump that landed in
// the pin file and not in the workflow copy, a service added without a pin, the
// Dependabot entry dropped, the reverse-proxy script growing its own digest
// back, the frontend directive drifting from its pin. Each must be red; a
// commented-out stale pin, which a byte read would take for a live one, must
// not change anything.
//
// Every fixture is a copied repository shape, never the checkout: an
// interrupted run must not leave a production file mutated.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/ci-image-pins-contract-check.mjs';
const COPIED = [
  check,
  'scripts/check-reverse-proxy-examples.sh',
  'scripts/gitea-import-live.sh',
  'Dockerfile',
  '.github/dependabot.yml',
  'ci/images/Dockerfile',
  '.github/workflows',
  'scripts/lib',
];

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-ci-image-pins-'));
  for (const path of COPIED) {
    mkdirSync(dirname(join(fixture, path)), { recursive: true });
    cpSync(join(root, path), join(fixture, path), { recursive: true });
  }
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [join(fixture, check), fixture], {
    cwd: fixture,
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

let failures = 0;

// `mutate(text)` returns the text `file` gets in the fixture.
function fixture(name, file, mutate, expectRed) {
  const dir = fixtureRoot();
  try {
    const path = join(dir, file);
    const source = readFileSync(path, 'utf8');
    const mutated = mutate(source);
    if (mutated === source) throw new Error('the mutation changed nothing — the anchor it edits is gone');
    writeFileSync(path, mutated);
    const result = run(dir);
    let reason = null;
    if (expectRed === null) {
      if (result.status !== 0) reason = 'the check failed';
    } else if (result.status === 0) {
      reason = 'the check passed';
    } else if (!result.output.includes(expectRed)) {
      reason = `no diagnostic mentioning ${JSON.stringify(expectRed)}`;
    }
    if (reason) {
      failures += 1;
      console.error(`❌ ${name}: ${reason}\n----- exit ${result.status} -----\n${result.output}`);
    } else {
      console.log(`✅ ${name}`);
    }
  } catch (error) {
    failures += 1;
    console.error(`❌ ${name}: ${error.message}`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

{
  const dir = fixtureRoot();
  const result = run(dir);
  rmSync(dir, { recursive: true, force: true });
  if (result.status !== 0) {
    failures += 1;
    console.error(`❌ the unmutated fixture: the check failed\n${result.output}`);
  } else {
    console.log('✅ the unmutated fixture');
  }
}

const bumpDigest = (source, stage) => source.replace(
  new RegExp(`(@sha256:)([0-9a-f]{64})( AS ${stage})$`, 'm'),
  (_, head, hex, tail) => `${head}${hex.startsWith('0') ? `1${hex.slice(1)}` : `0${hex.slice(1)}`}${tail}`,
);

// What a Dependabot pull request looks like: the pin file moves, the workflow
// copy does not. This is the drift the check exists to name.
fixture(
  'a Dependabot bump of the postgres pin that the workflow did not follow is red',
  'ci/images/Dockerfile',
  (source) => bumpDigest(source, 'postgres'),
  'jobs.postgres-smoke.services.postgres.image',
);

fixture(
  'a Dependabot bump of the frontend pin that the directive did not follow is red',
  'ci/images/Dockerfile',
  (source) => bumpDigest(source, 'dockerfile-frontend'),
  'Dockerfile:1 selects',
);

// A byte read takes the first `FROM … AS postgres`, commented or not.
fixture(
  'a commented-out stale pin above the live one changes nothing',
  'ci/images/Dockerfile',
  (source) => source.replace(
    /^FROM postgres:/m,
    '# FROM postgres:16@sha256:0000000000000000000000000000000000000000000000000000000000000000 AS postgres\nFROM postgres:',
  ),
  null,
);

fixture(
  'a stage pinned twice is red',
  'ci/images/Dockerfile',
  (source) => `${source}FROM mysql:8.0@sha256:${'a'.repeat(64)} AS mysql\n`,
  'pinned twice',
);

fixture(
  'a tag-only pin is red',
  'ci/images/Dockerfile',
  (source) => source.replace(/^(FROM mysql:8\.4)@sha256:[0-9a-f]{64}/m, '$1'),
  'must be `<name>:<tag>@sha256:<64 hex>`',
);

fixture(
  'dropping the Dependabot entry is red',
  '.github/dependabot.yml',
  (source) => source.replace('directory: /ci/images', 'directory: /ci/images-gone'),
  'no `docker` entry',
);

// The pin file lived in `.github/ci-images/` for three weekly runs, each of
// which found the bumps and then could not open a single pull request.
fixture(
  'a docker entry under .github/ is red',
  '.github/dependabot.yml',
  (source) => source.replace(
    '  - package-ecosystem: docker-compose\n',
    '  - package-ecosystem: docker\n    directory: /.github/ci-images\n    schedule:\n      interval: weekly\n  - package-ecosystem: docker-compose\n',
  ),
  'under .github/',
);

fixture(
  'a workflow service added without a pin is red',
  '.github/workflows/regression.yml',
  (source) => source.replace(
    /^( {6}postgres:\n)/m,
    '      redis:\n        image: redis:7\n$1',
  ),
  'jobs.postgres-smoke.services.redis.image is `redis:7`',
);

fixture(
  'the workflow copy commented out above a stale live line is red',
  '.github/workflows/regression.yml',
  (source) => source.replace(
    /^( {8})image: (mysql:8\.4@sha256:[0-9a-f]{64})$/m,
    '$1# image: $2\n$1image: mysql:8.4',
  ),
  'jobs.mysql-smoke.services.mysql.image is `mysql:8.4`',
);

fixture(
  'the reverse-proxy script pinning a digest of its own again is red',
  'scripts/check-reverse-proxy-examples.sh',
  (source) => source.replace(
    'NGINX_IMAGE="$(ci_image nginx)"',
    `NGINX_IMAGE=nginx:1.27-alpine@sha256:${'b'.repeat(64)}`,
  ),
  'pins a digest of its own',
);

// card_7f2bdd581d17: the live import script is held to the same rule, and
// to reading both servers it starts.
fixture(
  'the live import script pinning a digest of its own is red',
  'scripts/gitea-import-live.sh',
  (source) => source.replace(
    'gitea) PINNED="$(ci_image gitea)" ;;',
    `gitea) PINNED=gitea/gitea:1.27.3@sha256:${'c'.repeat(64)} ;;`,
  ),
  'gitea-import-live.sh pins a digest of its own',
);

fixture(
  'the live import script no longer starting the pinned Forgejo is red',
  'scripts/gitea-import-live.sh',
  (source) => source.replace('forgejo) PINNED="$(ci_image forgejo)" ;;', 'forgejo) PINNED="$(ci_image gitea)" ;;'),
  'no longer reads the `forgejo` pin',
);

if (failures > 0) {
  console.error(`\n${failures} fixture(s) did not behave — the CI image pins check has stopped asserting what it claims.`);
  process.exit(1);
}
console.log('\nci-image-pins-contract-check still bites.');
