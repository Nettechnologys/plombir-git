#!/usr/bin/env node

// Mutation stand for pipefail-early-reader-contract-check.mjs: the real check
// runs against a fixture repository whose every early-exit pipe sits in a
// different place the check has to see — a script that sets pipefail, a library
// that only inherits it through `source`, a workflow step under `shell: bash`,
// one that sets pipefail itself — next to the shapes it must leave alone.

import assert from 'node:assert/strict';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'pipefail-early-reader-contract-check.mjs');

const CLEAN = {
  // Searched from a here-string, from a file, or through a reader that reads
  // everything: none of these can hand the writer a SIGPIPE.
  'scripts/clean.sh': `#!/usr/bin/env bash
set -euo pipefail
out="$(git rev-list --objects HEAD)"
grep -q '^?' <<<"\${out}"
grep -q pattern ./some-file
git log | grep -oE '[0-9a-f]{40}' | sort
# a comment about \`docker logs | grep -q\` is not a pipe
echo "a string that says | grep -q is data"
`,
  // Without pipefail the pipeline's status is grep's own.
  'scripts/no-pipefail.sh': `#!/bin/sh
set -eu
docker logs x | grep -q ready
`,
  '.github/workflows/clean.yml': `name: clean
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Default shell, no pipefail
        run: curl -fsS http://localhost/ | grep -q html
`,
};

const MUTANTS = {
  'scripts/sets-pipefail.sh': `#!/usr/bin/env bash
set -euo pipefail
if ! git rev-list --objects HEAD \\
  | grep -q '^?'; then
  exit 1
fi
`,
  'scripts/lib/inherited.sh': `first_url() {
  url="$(sed 's/x//' log | grep -oE 'http://[^ ]+' | head -1)"
}
`,
  'scripts/sources-lib.sh': `#!/usr/bin/env bash
set -o pipefail
source "\${ROOT_DIR}/scripts/lib/inherited.sh"
`,
  '.github/workflows/mutant.yml': `name: mutant
on: push
jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - name: Under shell bash
        shell: bash
        run: |
          printf '%s\\n' "\${changed}" | grep -qE '^crates/'
      - name: Sets pipefail itself
        run: |
          set -euo pipefail
          docker logs x | grep --max-count=1 ready
`,
};

const EXPECTED = [
  'scripts/lib/inherited.sh:2',
  'scripts/sets-pipefail.sh:3',
  '.github/workflows/mutant.yml job `smoke`, step "Under shell bash"',
  '.github/workflows/mutant.yml job `smoke`, step "Sets pipefail itself"',
];

function git(args, cwd) {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
  assert.equal(result.status, 0, `git ${args.join(' ')}: ${result.stderr}`);
}

function run(files) {
  const dir = scratchDir(join(tmpdir(), 'plombir-git-pipefail-reader-'));
  try {
    for (const [path, body] of Object.entries(files)) {
      mkdirSync(dirname(join(dir, path)), { recursive: true });
      writeFileSync(join(dir, path), body);
    }
    git(['init', '-q', '-b', 'main'], dir);
    git(['add', '.'], dir);
    const result = spawnSync(process.execPath, [check, dir], { encoding: 'utf8' });
    return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const clean = run(CLEAN);
assert.equal(clean.status, 0, `the clean fixture must pass:\n${clean.output}`);

const mutant = run({ ...CLEAN, ...MUTANTS });
assert.equal(mutant.status, 1, `the mutant fixture must fail:\n${mutant.output}`);
const reported = mutant.output.split('\n').filter((line) => line.startsWith('❌ '));
for (const site of EXPECTED) {
  assert.ok(
    reported.some((line) => line.startsWith(`❌ ${site}`)),
    `the check did not name ${site}:\n${mutant.output}`,
  );
}
assert.equal(reported.length, EXPECTED.length, `the check named a site it should leave alone:\n${mutant.output}`);

console.log(`✅ the pipefail early-reader check names all ${EXPECTED.length} mutant sites and passes the clean shapes`);
