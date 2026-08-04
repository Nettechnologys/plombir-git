#!/usr/bin/env node

// Regression fixtures for the label-preservation part of the observability
// contract. Run the real checker in a copied repository shape so the fixture
// cannot accidentally weaken production alerts or bypass its parse floors.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/observability-contract-check.mjs';

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-observability-contract-'));
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  mkdirSync(join(fixture, 'crates', 'rg-http', 'src'), { recursive: true });
  cpSync(join(root, check), join(fixture, check));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  cpSync(join(root, 'crates', 'rg-http', 'src', 'metrics.rs'), join(fixture, 'crates', 'rg-http', 'src', 'metrics.rs'));
  cpSync(join(root, 'deploy'), join(fixture, 'deploy'), { recursive: true });
  return fixture;
}

function runFixture(name, rule, expectedStatus, expectedOutput = '', appendLast = false) {
  const fixture = fixtureRoot();
  try {
    const alertsPath = join(fixture, 'deploy', 'prometheus', 'alerts.yml');
    const alerts = readFileSync(alertsPath, 'utf8');
    const nextRule = '      # Slow requests';
    if (!alerts.includes(nextRule)) throw new Error('fixture anchor for the next alert rule disappeared');
    const fixtureAlerts = appendLast
      ? `${alerts.trimEnd()}\n\n${rule}\n`
      : alerts.replace(nextRule, `${rule}\n\n${nextRule}`);
    writeFileSync(alertsPath, fixtureAlerts);

    const result = spawnSync(process.execPath, [join(fixture, check)], {
      cwd: fixture,
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (result.status !== expectedStatus || (expectedOutput && !output.includes(expectedOutput))) {
      throw new Error(
        `${name}: expected exit ${expectedStatus}${expectedOutput ? ` and ${JSON.stringify(expectedOutput)}` : ''}, ` +
          `got exit ${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

runFixture(
  'without retains labels not named by the modifier',
  `      - alert: WithoutKeepsRoute
        expr: sum without (status) (http_requests_total)
        annotations:
          summary: "Route {{ $labels.route }} remains available"`,
  0,
);

runFixture(
  'all-foreign aggregations do not claim exporter label knowledge',
  `      - alert: ForeignMetricLabelsAreUnknown
        expr: sum by (instance) (node_filesystem_avail_bytes)
        annotations:
          summary: "Filesystem {{ $labels.mountpoint }}"`,
  0,
);

runFixture(
  'by omitting an interpolated label fails the contract',
  `      - alert: GroupingDropsRoute
        expr: sum by (instance) (http_requests_total)
        annotations:
          summary: "Route {{ $labels.route }} disappeared"`,
  1,
  'alerts.yml: GroupingDropsRoute interpolates {{ $labels.route }}, but its expr does not retain `route`',
);

runFixture(
  'last alert rule with a lost label fails the contract',
  `      - alert: FinalGroupingDropsRoute
        expr: sum by (instance) (http_requests_total)
        annotations:
          summary: "Route {{ $labels.route }} disappeared"`,
  1,
  'alerts.yml: FinalGroupingDropsRoute interpolates {{ $labels.route }}, but its expr does not retain `route`',
  true,
);
