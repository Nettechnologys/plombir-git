#!/usr/bin/env node

// Mutation stand for route-consumer-contract-check.mjs.
//
// That check answers "does anything call this mounted mutating route", and the
// two ways it can lie are opposite. It can go quiet — stop finding the client,
// stop reading the router, and report a tree it never looked at as clean. Or it
// can go blind the other way — let a client path made entirely of parameters
// stand for every route of that length, and report everything as called.
//
// Each fixture below breaks exactly one thing and names the sentence the check
// must produce. A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'route-consumer-contract-check.mjs');

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-route-consumer-'));
  mkdirSync(join(fixture, 'crates', 'rg-http', 'src'), { recursive: true });
  cpSync(
    join(root, 'crates', 'rg-http', 'src', 'routes.rs'),
    join(fixture, 'crates', 'rg-http', 'src', 'routes.rs'),
  );
  cpSync(join(root, 'web', 'src'), join(fixture, 'web', 'src'), { recursive: true });
  return fixture;
}

function edit(file, before, after) {
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
        router: join(fixture, 'crates', 'rg-http', 'src', 'routes.rs'),
        client: join(fixture, 'web', 'src'),
        auth: join(fixture, 'web', 'src', 'lib', 'api', 'auth.ts'),
      });
    }
    const result = spawnSync(process.execPath, [check], { cwd: fixture, encoding: 'utf8' });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (result.status !== expectedStatus || !output.includes(expectedOutput)) {
      throw new Error(
        `${name}: expected exit ${expectedStatus} and ${JSON.stringify(expectedOutput)}, got exit ` +
          `${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The copied tree is the shipped tree: if this one is not green, every red
// below is about the copy rather than about the mutation.
runFixture('an unmutated copy of the tree passes', null, 0, 'route consumer contract ok');

// The defect the check exists for, in the shape it was last found in: a route
// mounted, gated and handled, whose only caller was never written
// (card_2cd2d40f27d2).
runFixture(
  'a mounted route whose client call is deleted is reported',
  ({ auth }) => {
    edit(
      auth,
      "    request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {",
      '    request<{ unlinked: boolean }>(`/auth/sso/__deleted__`, {',
    );
  },
  1,
  'DELETE /api/v1/auth/sso/{slug}/unlink',
);

// The same call, commented out rather than deleted. The client sources are read
// through `productionTsSource` precisely so that this is not a way to keep a
// gate green while the feature is gone.
runFixture(
  'a client call that is only commented out does not count',
  ({ auth }) => {
    edit(
      auth,
      '    request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {',
      '    // request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {\n' +
        '    request<{ unlinked: boolean }>(`/auth/sso/__commented__`, {',
    );
  },
  1,
  'DELETE /api/v1/auth/sso/{slug}/unlink',
);

// The blind direction. A client path of nothing but parameters matches any
// route of the same length, so the index would call every route consumed — and
// the check's own vacuity probe is what has to notice.
runFixture(
  'a client path made only of parameters cannot stand for every route',
  ({ client }) => {
    writeFileSync(
      join(client, 'lib', 'api', 'zz_wildcard_fixture.ts'),
      'export const anything = (a: string, b: string, c: string) =>\n' +
        '  fetch(`/${a}/${b}/${c}`, { method: \'DELETE\' });\n',
    );
  },
  1,
  'no longer discriminates between routes',
);

// The client is gone. Every route would read as an orphan, which is a loud
// failure — but it must be loud about the *cause*, not print a hundred
// accusations against a tree it never read.
runFixture(
  'a client tree it cannot read is reported as such, not as a hundred orphans',
  ({ client }) => {
    rmSync(join(client, 'lib'), { recursive: true, force: true });
    rmSync(join(client, 'routes'), { recursive: true, force: true });
  },
  1,
  'fix the path, not the code',
);

// The allowlist is a ratchet. An entry naming a route the router stopped
// serving must fail rather than sit there describing a server that is gone.
runFixture(
  'an allowlist entry for a route the router no longer mounts fails',
  ({ router }) => {
    edit(router, '"/runners/{id}/heartbeat"', '"/runners/{id}/heartbeat-renamed"');
  },
  1,
  'the router does not mount it any more',
);

// The other end of the same ratchet: an exemption that stopped applying,
// because somebody wired the route up after all.
runFixture(
  'an allowlist entry for a route that gained a caller fails',
  ({ client }) => {
    writeFileSync(
      join(client, 'lib', 'api', 'zz_runner_fixture.ts'),
      "export const heartbeat = (id: string) =>\n" +
        "  fetch(`/runners/${id}/heartbeat`, { method: 'POST' });\n",
    );
  },
  1,
  'Delete the entry',
);

console.log('route consumer contract regression stand green');
