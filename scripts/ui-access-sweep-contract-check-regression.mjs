#!/usr/bin/env node

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const copied = [
  'scripts/ui-access-sweep-contract-check.mjs',
  'scripts/ui-access-sweep-e2e.mjs',
  'scripts/ui-access-sweep-e2e.sh',
  'scripts/lib/js-source.mjs',
  'scripts/lib/route-specificity.mjs',
  'scripts/lib/shell-source.mjs',
  'scripts/lib/ts-source.mjs',
  'scripts/lib/ui-access-sweep.mjs',
  'scripts/lib/ui-access-sweep-scenarios.mjs',
  'docs/ui-access-sweep.json',
  'docs/ui-inventory.json',
  'web/package.json',
];

function baseline(fixture) {
  for (const path of copied) {
    const target = join(fixture, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, path), target);
  }
}

function patch(fixture, path, from, to) {
  const target = join(fixture, path);
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) throw new Error(`mutation cannot find ${JSON.stringify(from)} in ${path}`);
  writeFileSync(target, source.replace(from, to));
}

function run(fixture) {
  const result = spawnSync(process.execPath, [join(fixture, 'scripts/ui-access-sweep-contract-check.mjs')], {
    cwd: fixture,
    env: { ...process.env, FORGEKEEP_UI_ACCESS_SWEEP_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const mutations = [
  {
    name: 'the outsider persona is removed from the manifest',
    apply: (fixture) => patch(fixture, 'docs/ui-access-sweep.json', '    "owner",\n    "outsider"', '    "owner"'),
    expect: 'personas must be exactly owner, outsider',
  },
  {
    name: 'the shared oracle stops requiring an outsider result',
    apply: (fixture) => patch(
      fixture,
      'scripts/lib/ui-access-sweep.mjs',
      'for (const persona of REQUIRED_PERSONAS) {\n    if (!observedByPersona.has(persona)) {',
      'for (const persona of observedByPersona.keys()) {\n    if (!observedByPersona.has(persona)) {',
    ),
    expect: 'accepted a result with no outsider persona',
  },
  {
    name: 'the shared oracle lets a static sibling satisfy a placeholder claim',
    apply: (fixture) => patch(
      fixture,
      'scripts/lib/ui-access-sweep.mjs',
      'matcher = (path) => claimed.test(path) && !rivals.some((rival) => rival.test(path));',
      'matcher = (path) => claimed.test(path);',
    ),
    expect: 'credited a static sibling to a placeholder route',
  },
  {
    name: 'the ratchet is raised after coverage lands',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      spec.ratchet.maxUiRoutesWithoutFrontendTest += 1;
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
    },
    expect: 'lower it to keep the win',
  },
  {
    name: 'an uncovered UI route is added while the ratchet stays put',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-inventory.json');
      const inventory = JSON.parse(readFileSync(path, 'utf8'));
      inventory.routes.push({
        method: 'GET',
        url: '/api/v1/mutation-only-uncovered',
        access: 'User',
        handler: 'mutation',
        testedIn: ['rust'],
        reachedFromUi: true,
        browserScenarios: [],
      });
      writeFileSync(path, `${JSON.stringify(inventory, null, 2)}\n`);
    },
    expect: 'debt grew from the ratchet',
  },
  {
    name: 'a manifest entry stops naming a live inventory call',
    apply: (fixture) => patch(
      fixture,
      'docs/ui-access-sweep.json',
      '"routeUrl": "/api/v1/orgs"',
      '"routeUrl": "/api/v1/orgs-typo"',
    ),
    expect: 'matched 0 inventory entries',
  },
  {
    name: 'a declared scenario loses its runtime',
    apply: (fixture) => patch(
      fixture,
      'scripts/lib/ui-access-sweep-scenarios.mjs',
      "  ['private-repository-blob', privateRepositoryBlob],\n",
      '',
    ),
    expect: 'manifest scenario(s) have no runtime',
  },
  {
    name: 'one UI-reached InstanceAdmin route loses browser coverage',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      const scenario = spec.scenarios.find(({ id }) => id === 'admin-users-unlock');
      scenario.covers = scenario.covers.filter(
        ({ method, routeUrl }) => method !== 'POST' || routeUrl !== '/api/v1/admin/users/{id}/unlock',
      );
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
    },
    expect: 'UI-reached InstanceAdmin route(s) lack browser coverage',
  },
  {
    name: 'the RepoOwner transfer scenario disappears from both manifest and runtime',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      spec.scenarios = spec.scenarios.filter(({ id }) => id !== 'repo-transfer');
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
      patch(
        fixture,
        'scripts/lib/ui-access-sweep-scenarios.mjs',
        "  ['repo-transfer', repoTransfer],\n",
        '',
      );
    },
    expect: 'required privileged UI route lacks browser coverage',
  },
  {
    name: 'one UI-reached RepoAdmin route loses browser coverage',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      const scenario = spec.scenarios.find(({ id }) => id === 'repo-deploy-keys');
      scenario.covers = scenario.covers.filter(
        ({ method, routeUrl }) => method !== 'DELETE' || routeUrl !== '/api/v1/repos/{owner}/{name}/keys/{id}',
      );
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
    },
    expect: 'UI-reached RepoAdmin route(s) lack browser coverage',
  },
  {
    name: 'one UI-reached OrgAdmin route loses browser coverage',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      const scenario = spec.scenarios.find(({ id }) => id === 'organization-admin');
      scenario.covers = scenario.covers.filter(
        ({ method, routeUrl }) => method !== 'PATCH' || routeUrl !== '/api/v1/orgs/{name}',
      );
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
    },
    expect: 'UI-reached OrgAdmin route(s) lack browser coverage',
  },
  {
    name: 'a destructive admin scenario runs the owner before the outsider',
    apply: (fixture) => {
      const path = join(fixture, 'docs/ui-access-sweep.json');
      const spec = JSON.parse(readFileSync(path, 'utf8'));
      spec.scenarios.find(({ id }) => id === 'admin-users-delete').personaOrder = ['owner', 'outsider'];
      writeFileSync(path, `${JSON.stringify(spec, null, 2)}\n`);
    },
    expect: 'must run outsider before owner',
  },
  {
    name: 'a privileged scenario loses its outsider browser action',
    apply: (fixture) => patch(
      fixture,
      'scripts/lib/ui-access-sweep-scenarios.mjs',
      "  ['admin-users-delete', adminUsersDelete],\n",
      "  ['admin-users-delete', { owner: adminUsersDelete.owner }],\n",
    ),
    expect: 'must declare separate owner and outsider browser actions',
  },
  {
    name: 'the browser stand is allowed to serve a stale frontend bundle',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.sh',
      'STAND_REBUILD_FRONTEND=1 ',
      'STAND_REBUILD_FRONTEND=0 # STAND_REBUILD_FRONTEND=1\n',
    ),
    expect: 'may serve a stale web/build',
  },
  {
    name: 'the browser runtime ignores the declared outsider-first order behind an inline decoy',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.mjs',
      'for (const persona of personaOrder) {',
      'for (const persona of REQUIRED_PERSONAS) { // for (const persona of personaOrder) {',
    ),
    expect: 'no longer drives every scenario persona in its declared order',
  },
  {
    name: 'the browser runtime drops the Access oracle behind an inline decoy',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.mjs',
      'assertPersonaResults(scenario, observed, routeUrls);',
      'void observed; // assertPersonaResults(scenario, observed, routeUrls);',
    ),
    expect: 'no longer hands observed network responses to the shared Access oracle',
  },
  {
    name: 'browser navigation readiness is detached from the accepted loader',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.mjs',
      'readiness.followNavigation(result);',
      'readiness.followNavigation({});',
    ),
    expect: 'no longer binds navigation readiness to the Page.navigate loader',
  },
  {
    name: 'browser navigation advances at load before page API requests settle',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.mjs',
      "readyEvent: 'networkAlmostIdle',",
      "readyEvent: 'load',",
    ),
    expect: 'no longer binds navigation readiness to the Page.navigate loader',
  },
  {
    name: 'the webhook fixture falls back to an ad-hoc short timeout',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-access-sweep-e2e.mjs',
      'description: \'webhook fixture delivery id\',\n    timeoutMs: UI_WAIT_MS,',
      'description: \'webhook fixture delivery id\',\n    timeoutMs: 5_000,',
    ),
    expect: 'webhook delivery fixture no longer uses the shared bounded UI wait',
  },
];

let fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ui-access-sweep-contract.'));
try {
  baseline(fixture);
  const clean = run(fixture);
  if (clean.status !== 0) {
    console.error(`❌ UI access sweep baseline fixture is red, so mutations prove nothing:\n${clean.output}`);
    process.exit(1);
  }

  patch(
    fixture,
    'scripts/ui-access-sweep-e2e.sh',
    'STAND_REBUILD_FRONTEND=1 ',
    "PROBE='# literal hash' STAND_REBUILD_FRONTEND=1 ",
  );
  patch(
    fixture,
    'scripts/ui-access-sweep-e2e.mjs',
    'for (const persona of personaOrder) {',
    "void 'https://forgekeep.invalid/persona'; for (const persona of personaOrder) {",
  );
  const literalAware = run(fixture);
  if (literalAware.status !== 0) {
    console.error(
      `❌ source views rejected quoted # or // data that remains live code:\n${literalAware.output}`,
    );
    process.exit(1);
  }
  console.log('✅ source views preserve quoted shell hashes and JavaScript string literals');

  for (const mutation of mutations) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ui-access-sweep-contract.'));
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
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
