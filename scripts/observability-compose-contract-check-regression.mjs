#!/usr/bin/env node

// Does `scripts/observability-compose-contract-check.mjs` still bite?
//
// The check it stands for replaced two shell reads of the raw compose bytes, so
// the fixtures are written in the vocabulary of that defect: a commented-out
// image pin sitting above the live one, a commented-out bind mount, a pin that
// went floating, a mount source that was renamed. The first two are the cases
// the old `grep … | head -1` and `sed -nE '…- (\./[^:]+):…'` got wrong in
// opposite directions — a stale tag read as live, a dead mount checked as live
// — and a green run of this stand is the evidence that the parsed read does not.
//
// Every fixture is a copied repository shape: the real check, its lib, and a
// real `deploy/` tree. Mutating the checkout instead would leave the production
// compose file mutated the moment a run is interrupted.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/observability-compose-contract-check.mjs';
const compose = 'deploy/docker-compose.observability.yml';

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-observability-compose-'));
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, check), join(fixture, check));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  cpSync(join(root, 'deploy'), join(fixture, 'deploy'), { recursive: true });
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [join(fixture, check)], {
    cwd: fixture,
    encoding: 'utf8',
  });
  return {
    status: result.status,
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? '',
  };
}

let failures = 0;

// `mutate` receives the compose text and returns the text the fixture gets.
// `assert` receives the check's result and returns a reason string when the
// expectation does not hold.
function fixture(name, mutate, assert) {
  const dir = fixtureRoot();
  try {
    const path = join(dir, compose);
    const source = readFileSync(path, 'utf8');
    const mutated = mutate(source, dir);
    if (mutated === source) throw new Error('the mutation changed nothing — the anchor it edits is gone');
    writeFileSync(path, mutated);

    const result = run(dir);
    const reason = assert(result);
    if (reason) {
      failures += 1;
      console.error(`❌ ${name}: ${reason}\n----- exit ${result.status} -----\n${result.stdout}${result.stderr}`);
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

const red = (fragment) => (result) => {
  if (result.status === 0) return 'the check passed';
  if (!`${result.stdout}${result.stderr}`.includes(fragment)) return `no diagnostic mentioning ${JSON.stringify(fragment)}`;
  return null;
};

const green = (expected, forbidden) => (result) => {
  if (result.status !== 0) return 'the check failed';
  if (!result.stdout.includes(expected)) return `stdout does not carry ${JSON.stringify(expected)}`;
  if (forbidden && result.stdout.includes(forbidden)) return `stdout carries ${JSON.stringify(forbidden)}`;
  return null;
};

// The baseline: an unmutated fixture is green, so every red below is the
// mutation and not the copy.
{
  const dir = fixtureRoot();
  const result = run(dir);
  rmSync(dir, { recursive: true, force: true });
  const reason = green('prometheus=prom/prometheus:', null)(result);
  if (reason) {
    failures += 1;
    console.error(`❌ the unmutated fixture: ${reason}\n${result.stdout}${result.stderr}`);
  } else {
    console.log('✅ the unmutated fixture');
  }
}

// The defect this check was written for: `grep … | head -1` read the commented
// line, so the gate validated the config with a promtool the stack does not run.
fixture(
  'a commented-out pin above the live one does not become the tag',
  (source) => source.replace(
    '    image: prom/prometheus:v2.55.0',
    '    # image: prom/prometheus:v1.0.0-stale\n    image: prom/prometheus:v2.55.0',
  ),
  green('prometheus=prom/prometheus:v2.55.0', 'v1.0.0-stale'),
);

// …and the same line commented out with nothing live behind it is not a tag
// either, which is where `head -1` of an empty result printed an empty string.
fixture(
  'a service whose only pin is commented out is red',
  (source) => source.replace(
    '    image: prom/prometheus:v2.55.0',
    '    # image: prom/prometheus:v2.55.0',
  ),
  red('services.prometheus.image is absent'),
);

fixture(
  'a floating tag is red',
  (source) => source.replace('image: prom/prometheus:v2.55.0', 'image: prom/prometheus:latest'),
  red('pin an explicit tag'),
);

fixture(
  'an untagged image is red',
  (source) => source.replace('image: prom/prometheus:v2.55.0', 'image: prom/prometheus'),
  red('pin an explicit tag'),
);

fixture(
  'another registry for the image this gate runs is red',
  (source) => source.replace(
    'image: prom/alertmanager:v0.27.0',
    'image: quay.io/prometheus/alertmanager:v0.27.0',
  ),
  red('not a `prom/alertmanager` image'),
);

fixture(
  'a renamed service is red',
  (source) => source.replace('  alertmanager:\n', '  alerts:\n'),
  red('no longer declares a `alertmanager` service'),
);

// Docker creates a missing bind-mount source as an empty directory, so this is
// the difference between Prometheus loading its rules and Prometheus finding a
// directory where a rule file belongs.
fixture(
  'a mount source that does not exist is red',
  (source) => source.replace('./prometheus/alerts.yml:', './prometheus/alerts-renamed.yml:'),
  red('does not exist'),
);

// The other direction of the same defect: `sed` read commented mounts as live,
// so a dead line demanded a file nobody mounts.
fixture(
  'a commented-out mount is not checked',
  (source) => source.replace(
    '      - ./prometheus/alerts.yml:',
    '      # - ./prometheus/alerts-deleted.yml:/etc/prometheus/alerts.yml:ro\n      - ./prometheus/alerts.yml:',
  ),
  green('prometheus=prom/prometheus:v2.55.0', null),
);

// A long-syntax bind mount is a mount `sed -nE '…- (\./…)'` could not see at
// all — the shape in which a missing file used to pass unnoticed.
fixture(
  'a long-syntax bind mount is checked too',
  (source) => source.replace(
    '      - ./alertmanager/alertmanager.yml:/etc/alertmanager/alertmanager.yml:ro',
    '      - type: bind\n        source: ./alertmanager/gone.yml\n        target: /etc/alertmanager/alertmanager.yml',
  ),
  red('deploy/alertmanager/gone.yml does not exist'),
);

// The floors: a read that finds nothing must be the read breaking, not the
// stack passing.
fixture(
  'a compose file with no relative bind mount is red',
  (source) => source.replace(/^ {6}- \.\/.*\n/gm, ''),
  red('declares no relative bind mounts'),
);

fixture(
  'a compose file with no services is red',
  (source) => source.replace(/^services:$/m, 'services: {}\nunused:'),
  red('declares no services'),
);

fixture(
  'a compose file that does not parse is red',
  (source) => `${source}\n  broken: [unclosed\n`,
  red('is not valid YAML'),
);

if (failures > 0) {
  console.error(`\n❌ ${failures} observability compose fixture(s) did not behave as the check promises.`);
  process.exit(1);
}

console.log('\n✅ the observability compose read still bites on every fixture.');
