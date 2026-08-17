#!/usr/bin/env node

// Mutation stand for the Grafana provisioning gate. Each case runs the real
// checker against a copied deploy tree, proving that syntax errors, vacuous
// documents and mount drift cannot look like a green repository.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runObservability } from './run-local-gates.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'grafana-provisioning-contract-check.mjs');

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-grafana-provisioning-'));
  cpSync(join(root, 'deploy'), join(fixture, 'deploy'), { recursive: true });
  return fixture;
}

function replaceRequired(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runCheck(name, fixture, expectedStatus, expectedOutput) {
  const result = spawnSync(process.execPath, [check, fixture], {
    cwd: fixture,
    encoding: 'utf8',
  });
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
  if (result.status !== expectedStatus || (expectedOutput && !output.includes(expectedOutput))) {
    throw new Error(
      `${name}: expected exit ${expectedStatus}${expectedOutput ? ` and ${JSON.stringify(expectedOutput)}` : ''}, `
        + `got exit ${result.status}\n${output}`,
    );
  }
  console.log(`✅ ${name}`);
}

function runFixture(name, mutate, expectedStatus, expectedOutput = '') {
  const fixture = fixtureRoot();
  try {
    if (mutate) mutate(fixture);
    runCheck(name, fixture, expectedStatus, expectedOutput);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

function runLocalGateFixture(name, mutate, expectedOutput) {
  const fixture = fixtureRoot();
  try {
    mutate(fixture);
    const result = runObservability({ cwd: fixture });
    if (result.ok || !result.output.includes(expectedOutput)) {
      throw new Error(
        `${name}: expected the local observability gate to fail with ${JSON.stringify(expectedOutput)}; `
          + `got ok=${result.ok}\n${result.output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

runFixture(
  'the shipped Grafana provisioning tree is valid',
  null,
  0,
  'grafana provisioning: 2 YAML file(s)',
);

runFixture(
  'malformed dashboard provisioning YAML fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'dashboards', 'dashboards.yml'),
    'apiVersion: 1',
    'apiVersion: [',
  ),
  1,
  'dashboards/dashboards.yml is not valid YAML',
);

runFixture(
  'malformed datasource provisioning YAML fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'datasources', 'prometheus.yml'),
    'apiVersion: 1',
    'apiVersion: [',
  ),
  1,
  'datasources/prometheus.yml is not valid YAML',
);

runFixture(
  'a dashboard provider path outside the compose mount fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'dashboards', 'dashboards.yml'),
    'path: /var/lib/grafana/dashboards',
    'path: /var/lib/grafana/not-mounted',
  ),
  1,
  'options.path (/var/lib/grafana/not-mounted) is not the target',
);

runFixture(
  'renaming the provisioning directory cannot make the glob vacuously green',
  (fixture) => renameSync(
    join(fixture, 'deploy', 'grafana', 'provisioning'),
    join(fixture, 'deploy', 'grafana', 'provisioning-renamed'),
  ),
  1,
  'deploy/grafana/provisioning/ is missing',
);

runFixture(
  'a wrong Grafana provisioning apiVersion fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'datasources', 'prometheus.yml'),
    'apiVersion: 1',
    'apiVersion: 2',
  ),
  1,
  'must declare numeric apiVersion: 1',
);

runFixture(
  'an empty dashboard provider list fails closed',
  (fixture) => writeFileSync(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'dashboards', 'dashboards.yml'),
    'apiVersion: 1\nproviders: []\n',
  ),
  1,
  'must declare at least one dashboard provider',
);

runFixture(
  'an empty datasource list fails closed',
  (fixture) => writeFileSync(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'datasources', 'prometheus.yml'),
    'apiVersion: 1\ndatasources: []\n',
  ),
  1,
  'must declare at least one datasource',
);

runLocalGateFixture(
  'the local observability gate propagates a provisioning parse failure',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'datasources', 'prometheus.yml'),
    'apiVersion: 1',
    'apiVersion: [',
  ),
  'datasources/prometheus.yml is not valid YAML',
);

runLocalGateFixture(
  'the local observability gate propagates dashboard mount drift',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'grafana', 'provisioning', 'dashboards', 'dashboards.yml'),
    'path: /var/lib/grafana/dashboards',
    'path: /var/lib/grafana/not-mounted',
  ),
  'options.path (/var/lib/grafana/not-mounted) is not the target',
);
