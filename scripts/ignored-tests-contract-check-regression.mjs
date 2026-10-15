#!/usr/bin/env node

// Mutation stand for ignored-tests-contract-check.mjs. It runs the real check
// against a copy of the workflow and the crates, mutated one way at a time, and
// asserts the check goes red naming what changed. A check that stopped reading
// either side would pass every one of these vacuously.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'ignored-tests-contract-check.mjs');

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-ignored-tests-'));
  mkdirSync(join(fixture, '.github', 'workflows'), { recursive: true });
  cpSync(
    join(root, '.github', 'workflows', 'regression.yml'),
    join(fixture, '.github', 'workflows', 'regression.yml'),
  );
  cpSync(join(root, 'crates'), join(fixture, 'crates'), {
    recursive: true,
    filter: (source) => !source.split(/[\\/]/).includes('target'),
  });
  return fixture;
}

function replaceRequired(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runFixture(name, mutate, expectedStatus, expectedOutput) {
  const fixture = fixtureRoot();
  try {
    if (mutate) {
      mutate({
        workflow: join(fixture, '.github', 'workflows', 'regression.yml'),
        crate: (path) => join(fixture, 'crates', path),
      });
    }
    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: { ...process.env, PLOMBIR_GIT_IGNORED_TESTS_ROOT: fixture },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (result.status !== expectedStatus || !output.includes(expectedOutput)) {
      throw new Error(
        `${name}: expected exit ${expectedStatus} and ${JSON.stringify(expectedOutput)}, `
          + `got exit ${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

runFixture(
  'the clean tree accounts for every ignored test',
  null,
  0,
  'ignored tests: 63 #[ignore] test(s) — 53 run by regression.yml, 10 not run by CI with a reason',
);

// card_0c68a0dc41e8 acceptance: the stock-client step is the only thing that
// runs these tests; without it they run nowhere.
runFixture(
  'dropping the stock Docker and git-lfs step leaves its tests running nowhere',
  ({ workflow }) => replaceRequired(
    workflow,
    "test(/^oci_live_client_tests::/) | test(/^lfs_locks_stock_client_tests::/)\n          | ",
    '',
  ),
  1,
  '`rg-http oci_live_client_tests::docker_pushes_and_pulls_a_private_repository` is #[ignore]d and no step of regression.yml runs it',
);

// The defect the check found on its first run: a test renamed under a step
// that kept the old name, so the step selected nothing and stayed green.
runFixture(
  'a step whose filter names a renamed test selects nothing',
  ({ crate }) => replaceRequired(
    crate('rg-core/src/search/code_indexer.rs'),
    'fn server_code_index_rebuild_publishes_atomically_without_holding_writers',
    'fn server_code_index_rebuild_was_renamed',
  ),
  1,
  'step `Prove PostgreSQL code index refresh boundary` asks for ignored tests and selects none of them',
);

// The same hazard without --ignored: the server-database jobs prove backend
// claims by running ordinary tests narrowed to one module.
runFixture(
  'an ordinary step narrowed to a renamed module selects nothing',
  ({ crate }) => {
    const main = crate('rg-db/tests/integration/main.rs');
    replaceRequired(main, 'mod password_reset_token_single_use;', 'mod password_reset_token_used_once;');
    cpSync(
      crate('rg-db/tests/integration/password_reset_token_single_use.rs'),
      crate('rg-db/tests/integration/password_reset_token_used_once.rs'),
    );
    rmSync(crate('rg-db/tests/integration/password_reset_token_single_use.rs'));
  },
  1,
  'step `Prove a parallel rg-db test file migrates PostgreSQL safely` asks for tests by name and selects none of them',
);

runFixture(
  'a test listed as not run by CI that a step starts running must leave the list',
  ({ workflow }) => replaceRequired(
    workflow,
    '          webhook_secret_at_rest_tests::\n',
    '          webhook_secret_at_rest_tests::\n          session_gate_cost_tests::\n',
  ),
  1,
  '`rg-http session_gate_cost_tests::session_gate_lookup_cost` is run by',
);

runFixture(
  'a listed test that is no longer ignored must leave the list',
  ({ crate }) => replaceRequired(
    crate('rg-http/tests/integration/session_gate_cost_tests.rs'),
    '#[ignore = "measurement, not an assertion — run explicitly"]',
    '',
  ),
  1,
  'NOT_RUN_BY_CI lists `rg-http session_gate_cost_tests::session_gate_lookup_cost`, which is not an #[ignore]d test any more',
);

// A commented-out attribute ignores nothing; the test runs in the workspace
// step, so the list entry is stale exactly as if the line were deleted.
runFixture(
  'a commented-out #[ignore] does not count as one',
  ({ crate }) => replaceRequired(
    crate('rg-http/tests/integration/session_gate_cost_tests.rs'),
    '#[ignore = "measurement, not an assertion — run explicitly"]',
    '// #[ignore = "measurement, not an assertion — run explicitly"]',
  ),
  1,
  'NOT_RUN_BY_CI lists `rg-http session_gate_cost_tests::session_gate_lookup_cost`, which is not an #[ignore]d test any more',
);

// A test reached only through `#[path]` keeps the path rustc gives it; a reader
// that lost the attribute would invent `…_tests::the_reference…` and miss the
// step's filter.
runFixture(
  'a test behind #[path] keeps its compiled module path',
  ({ workflow }) => replaceRequired(
    workflow,
    'cargo test -p rg-db-migration -j 6 --lib the_reference_installs_over_orphans_on_a_server_database -- --ignored --nocapture',
    'cargo test -p rg-db-migration -j 6 --lib -- --ignored --nocapture --exact migrations::m20261007_000003_issue_comments_reference_their_issue::tests::the_reference_installs_over_orphans_on_a_server_database',
  ),
  0,
  'ignored tests: 63 #[ignore] test(s) — 53 run by regression.yml',
);

runFixture(
  'a test confined to another target by --test is not run by it',
  ({ workflow }) => replaceRequired(
    workflow,
    'cargo test -p rg-core -j 6 --lib adoption_and_settings_serialize_on_server_database -- --ignored --nocapture',
    'cargo test -p rg-core -j 6 --test integration adoption_and_settings_serialize_on_server_database -- --ignored --nocapture',
  ),
  1,
  'step `Prove default-branch and import adoption serialize on PostgreSQL` asks for ignored tests and selects none of them',
);

runFixture(
  'a nextest filter set this check cannot read is red, not a guess',
  ({ workflow }) => replaceRequired(
    workflow,
    "test(/^oci_live_client_tests::/) | test(/^lfs_locks_stock_client_tests::/)",
    "test(/^oci_live_client_tests::/) & !test(/^lfs_locks_stock_client_tests::/)",
  ),
  1,
  'teach this check to read it rather than guessing what it runs',
);

runFixture(
  'an ignored test in a file no target reaches is reported',
  ({ crate }) => writeFileSync(
    crate('rg-http/tests/integration/orphan_module_tests.rs'),
    '#[test]\n#[ignore = "never compiled"]\nfn stranded() {}\n',
  ),
  1,
  'crates/rg-http/tests/integration/orphan_module_tests.rs holds an #[ignore] test, but no crate target\'s module tree reaches it',
);
