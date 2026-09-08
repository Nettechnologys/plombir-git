#!/usr/bin/env node

// Mutation stand for local-gate-coverage-contract-check.mjs. It runs the real
// checker against a copied workflow, hook, and verifier so a line-oriented YAML
// reader cannot return unnoticed: invalid YAML must fail before any coverage
// count is reported, while the existing accounting diagnostics remain intact.

import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { shellCodeOnly, shellInvokes } from './lib/shell-source.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'local-gate-coverage-contract-check.mjs');

for (const source of [
  "echo '# cargo clippy'",
  'echo "# cargo clippy"',
  'value=v3#1',
  'value=${VERSION#v}',
]) {
  assert.equal(shellCodeOnly(source), source, `${source}: data hash must survive the shell view`);
}
const inlineComment = 'true # cargo clippy';
const inlineView = shellCodeOnly(inlineComment);
assert.equal(inlineView.length, inlineComment.length, 'the shell view must remain byte-aligned');
assert.equal(inlineView.trimEnd(), 'true', 'a real inline comment must be blanked');
assert.equal(shellInvokes(inlineComment, 'cargo clippy'), false);
assert.equal(shellInvokes("echo '# cargo clippy'", 'cargo clippy'), false);
assert.equal(shellInvokes('value=v3#1; cargo clippy', 'cargo clippy'), true);
assert.equal(shellInvokes('value=${VERSION#v}; cargo clippy', 'cargo clippy'), true);
console.log('✅ shell source view distinguishes comments, quoted data, and live commands');

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-local-gate-coverage-'));
  mkdirSync(join(fixture, '.github', 'workflows'), { recursive: true });
  mkdirSync(join(fixture, '.githooks'), { recursive: true });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(
    join(root, '.github', 'workflows', 'regression.yml'),
    join(fixture, '.github', 'workflows', 'regression.yml'),
  );
  cpSync(join(root, '.githooks', 'pre-push'), join(fixture, '.githooks', 'pre-push'));
  cpSync(
    join(root, 'scripts', 'verify-push-gates.sh'),
    join(fixture, 'scripts', 'verify-push-gates.sh'),
  );
  return fixture;
}

function replaceRequired(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runFixture(name, mutate, expectedStatus, expectedOutput, forbiddenOutput = '') {
  const fixture = fixtureRoot();
  try {
    if (mutate) {
      mutate({
        workflow: join(fixture, '.github', 'workflows', 'regression.yml'),
        hook: join(fixture, '.githooks', 'pre-push'),
        verifier: join(fixture, 'scripts', 'verify-push-gates.sh'),
      });
    }
    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: { ...process.env, FORGEKEEP_LOCAL_GATE_COVERAGE_ROOT: fixture },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (
      result.status !== expectedStatus
      || !output.includes(expectedOutput)
      || (forbiddenOutput && output.includes(forbiddenOutput))
    ) {
      throw new Error(
        `${name}: expected exit ${expectedStatus}, ${JSON.stringify(expectedOutput)}`
          + `${forbiddenOutput ? ` and no ${JSON.stringify(forbiddenOutput)}` : ''}, `
          + `got exit ${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

runFixture(
  'the parsed graph preserves the clean-tree classification',
  null,
  0,
  '13 job(s) in regression.yml — 4 mirrored by run-local-gates.mjs, 3 by the card verifier, '
    + '1 excluded by design, 5 running nowhere',
);

runFixture(
  'commenting out a verifier command is not coverage',
  ({ verifier }) => replaceRequired(
    verifier,
    'cargo clippy --workspace --all-targets -j 6 -- -D warnings',
    '# cargo clippy --workspace --all-targets -j 6 -- -D warnings',
  ),
  1,
  'CARGO_JOBS says `clippy` is mirrored by `cargo clippy` in scripts/verify-push-gates.sh, which no longer invokes it.',
);

runFixture(
  'an inline verifier comment is not coverage',
  ({ verifier }) => replaceRequired(
    verifier,
    'cargo clippy --workspace --all-targets -j 6 -- -D warnings',
    'true # cargo clippy --workspace --all-targets -j 6 -- -D warnings',
  ),
  1,
  'CARGO_JOBS says `clippy` is mirrored by `cargo clippy` in scripts/verify-push-gates.sh, which no longer invokes it.',
);

runFixture(
  'a quoted command name is data, not coverage',
  ({ verifier }) => replaceRequired(
    verifier,
    'cargo clippy --workspace --all-targets -j 6 -- -D warnings',
    "echo '# cargo clippy'",
  ),
  1,
  'CARGO_JOBS says `clippy` is mirrored by `cargo clippy` in scripts/verify-push-gates.sh, which no longer invokes it.',
);

for (const [name, prefix] of [
  ['a quoted hash does not hide the live command after it', "echo '# cargo is prose'; "],
  ['a double-quoted hash does not hide the live command after it', 'echo "# cargo is prose"; '],
  ['a hash inside an assignment word is not a comment', 'value=v3#1; '],
  ['a hash inside parameter expansion is not a comment', 'value=${VERSION#v}; '],
]) {
  runFixture(
    name,
    ({ verifier }) => replaceRequired(
      verifier,
      'cargo clippy --workspace --all-targets -j 6 -- -D warnings',
      `${prefix}cargo clippy --workspace --all-targets -j 6 -- -D warnings`,
    ),
    0,
    'local gate coverage:',
  );
}

runFixture(
  'removing the hook fallback is not covered by the card prompt',
  ({ hook }) => replaceRequired(
    hook,
    '    sh scripts/verify-push-gates.sh',
    '    # sh scripts/verify-push-gates.sh',
  ),
  1,
  '.githooks/pre-push no longer invokes scripts/verify-push-gates.sh when its receipt is absent.',
);

runFixture(
  'malformed regression.yml fails before coverage is reported',
  ({ workflow }) => writeFileSync(workflow, `${readFileSync(workflow, 'utf8')}\nbroken: [\n`),
  1,
  '.github/workflows/regression.yml is not valid YAML',
  'local gate coverage:',
);

runFixture(
  'renaming a job preserves the stale mirror diagnostic',
  ({ workflow }) => replaceRequired(workflow, '  contract-checks:\n', '  contract-checks-renamed:\n'),
  1,
  'run-local-gates.mjs mirrors `contract-checks`, which is not a job in regression.yml — renamed or removed.',
);

// The mutation this check was green over for as long as it existed: the job
// keeps its name, keeps its place in the mirror, keeps being counted as
// covered — and stops running every contract check in the repository. Accounting
// by job name is not coverage (card_fad8ad0ef007).
runFixture(
  'a mirrored job that stops running what it is mirrored for is not coverage',
  ({ workflow }) => replaceRequired(
    workflow,
    '        run: node scripts/run-contract-checks.mjs',
    '        # run: node scripts/run-contract-checks.mjs',
  ),
  1,
  'regression.yml job `contract-checks` no longer runs `node scripts/run-contract-checks.mjs`',
);

// The same rule must not be satisfiable by prose. A `run:` body that merely
// *names* the command executes nothing, and this is where the shell-comment
// stripping earns its place.
runFixture(
  'a run body that only mentions the command in a comment is not coverage',
  ({ workflow }) => replaceRequired(
    workflow,
    '        run: node scripts/run-contract-checks.mjs',
    '        run: |\n          # node scripts/run-contract-checks.mjs\n          true',
  ),
  1,
  'regression.yml job `contract-checks` no longer runs `node scripts/run-contract-checks.mjs`',
);

runFixture(
  'a run body that only mentions the command in an inline comment is not coverage',
  ({ workflow }) => replaceRequired(
    workflow,
    '        run: node scripts/run-contract-checks.mjs',
    '        run: |\n          true # node scripts/run-contract-checks.mjs',
  ),
  1,
  'regression.yml job `contract-checks` no longer runs `node scripts/run-contract-checks.mjs`',
);
