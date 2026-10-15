#!/usr/bin/env node

// Mutation stand for the Grafana provisioning gate. Each case runs the real
// checker against a copied deploy tree, proving that syntax errors, vacuous
// documents and mount drift cannot look like a green repository.

import { spawnSync } from 'node:child_process';
import { cpSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runObservability } from './run-local-gates.mjs';
import { dashboardProblems } from './lib/grafana-dashboards.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'grafana-provisioning-contract-check.mjs');

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-grafana-provisioning-'));
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

// ── The dashboard judge behind scripts/check-grafana-dashboards.mjs ──
//
// The live check boots the pinned image; this half drives its judge with what
// that image answered, so a mutation can be shown red without Docker. The lists
// below are what grafana/grafana:13.2.3 returned from /api/plugins on
// 2026-10-15. They are a fixture, not the gate's source of truth: the gate asks
// the image every run, and the last case shows the answer is what decides.
const GRAFANA_13_PANELS = new Set(
  ('alertlist annolist barchart bargauge candlestick canvas dashlist flamegraph gauge geomap gettingstarted '
    + 'heatmap histogram logs logstable news nodeGraph piechart stat state-timeline status-history table text '
    + 'timeseries traces trend welcome xychart').split(' '),
);
const GRAFANA_13_DATASOURCES = new Set(
  ('alertmanager cloudwatch grafana-azure-monitor-datasource grafana-postgresql-datasource '
    + 'grafana-pyroscope-datasource grafana-testdata-datasource graphite influxdb jaeger loki mssql mysql '
    + 'opentsdb prometheus stackdriver tempo zipkin').split(' '),
);
const PROVISIONED = [{ name: 'Prometheus', uid: 'PBFA97CFB590B2093', type: 'prometheus' }];
const shippedDashboard = join(root, 'deploy', 'grafana', 'dashboards', 'plombir-git-main.json');

function judge(name, mutate, expected, { panelPlugins = GRAFANA_13_PANELS, copy = false } = {}) {
  const dashboard = JSON.parse(readFileSync(shippedDashboard, 'utf8'));
  if (mutate) mutate(dashboard);
  const dashboards = [{ file: 'plombir-git-main.json', dashboard }];
  if (copy) {
    dashboards.push({ file: 'plombir-git-copy.json', dashboard: { ...dashboard, title: 'Copy' } });
  }
  const problems = dashboardProblems({
    dashboards,
    panelPlugins,
    datasourcePlugins: GRAFANA_13_DATASOURCES,
    datasources: PROVISIONED,
    provisionedUids: new Set(['plombir-git-main']),
  });
  const ok = expected === null
    ? problems.length === 0
    : problems.length > 0 && problems.every((problem) => problem.includes(expected));
  if (!ok) {
    throw new Error(
      `${name}: expected ${expected === null ? 'no problems' : `problems mentioning ${JSON.stringify(expected)}`}, `
        + `got ${JSON.stringify(problems, null, 2)}`,
    );
  }
  console.log(`✅ ${name}`);
}

judge('the shipped dashboard resolves on Grafana 13', null, null);

// The defect the live check was written for: Grafana 12 removed the Angular
// `graph` panel, and every one of the overview's graphs was one.
judge(
  'a panel turned back into an Angular `graph` is red',
  (dashboard) => { dashboard.panels[0].type = 'graph'; },
  'is a `graph` panel, and this Grafana has no such panel plugin',
);

judge(
  'a `graph` panel inside a collapsed row is red too',
  (dashboard) => {
    const [first, ...rest] = dashboard.panels;
    dashboard.panels = [{ id: 99, type: 'row', title: 'Collapsed', collapsed: true, panels: [{ ...first, type: 'graph' }] }, ...rest];
  },
  'panels[0].panels[0] (id 1',
);

judge(
  'a data source variable over a plugin with nothing provisioned is red',
  (dashboard) => {
    dashboard.templating.list.find((variable) => variable.type === 'datasource').query = 'influxdb';
  },
  'none is provisioned',
);

judge(
  'a variable naming a data source Grafana does not provision is red',
  (dashboard) => {
    dashboard.templating.list.find((variable) => variable.name === 'route').datasource = 'Prometheus-old';
  },
  'which Grafana does not provision',
);

judge(
  'a panel pointing at an undefined data source variable is red',
  (dashboard) => { dashboard.panels[0].datasource = '${metrics}'; },
  'no data source variable of that name exists',
);

judge(
  'a dashboard without a uid is red',
  (dashboard) => { delete dashboard.uid; },
  'has no `uid`',
);

judge(
  'two dashboard files under one uid are red',
  null,
  'share uid plombir-git-main',
  { copy: true },
);

// The judge has no list of its own: the same `graph` panel is fine on an image
// that still ships the plugin, which is what a Grafana 11 pin was.
judge(
  'what the image reports decides, not a list in the gate',
  (dashboard) => { dashboard.panels[0].type = 'graph'; },
  null,
  { panelPlugins: new Set([...GRAFANA_13_PANELS, 'graph']) },
);
