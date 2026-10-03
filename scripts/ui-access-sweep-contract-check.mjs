#!/usr/bin/env node

// Cheap, import-safe proof around the expensive two-person browser sweep. The
// real ephemeral run remains the behavioral authority; this check keeps its
// inventory join, runner registry, persona matrix and exact debt ratchet from
// silently shrinking between those runs.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { shellCodeOnly } from './lib/shell-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';
import { UI_ACCESS_SWEEP_SCENARIOS } from './lib/ui-access-sweep-scenarios.mjs';
import {
  REQUIRED_PERSONAS,
  assertPersonaResults,
  examplePathFor,
  expectedForAccess,
  ratchetFailure,
  validateUiAccessSweep,
} from './lib/ui-access-sweep.mjs';

const root = resolve(
  process.env.PLOMBIR_GIT_UI_ACCESS_SWEEP_ROOT || join(dirname(fileURLToPath(import.meta.url)), '..'),
);
const failures = [];

function read(path) {
  try { return readFileSync(join(root, path), 'utf8'); } catch (error) {
    failures.push(`${path} cannot be read: ${error.message}`);
    return '';
  }
}

function json(path) {
  try { return JSON.parse(read(path)); } catch (error) {
    failures.push(`${path} is not valid JSON: ${error.message}`);
    return {};
  }
}

const inventory = json('docs/ui-inventory.json');
const spec = json('docs/ui-access-sweep.json');
const packageJson = json('web/package.json');
const routeUrls = (inventory.routes || []).map((route) => route.url).filter(Boolean);
let report = null;
try { report = validateUiAccessSweep(inventory, spec); } catch (error) {
  failures.push(error.message);
}

if (report) {
  if (report.inventoryEntries.length < 120) {
    failures.push(
      `the browser sweep traverses only ${report.inventoryEntries.length} matched inventory entries; `
        + 'the inventory parser or the traversal probably shrank',
    );
  }
  if (!report.coveredEntries.some((row) =>
    row.coverage.kind === 'control'
      && expectedForAccess(row.coverage.access, 'outsider') === 'denied')) {
    failures.push(
      'the browser sweep covers no privileged UI control — the same button-to-route path must allow its owner and deny an outsider',
    );
  }

  const declared = [...report.scenarioIds].sort();
  const runnable = [...UI_ACCESS_SWEEP_SCENARIOS.keys()].sort();
  const missing = declared.filter((id) => !UI_ACCESS_SWEEP_SCENARIOS.has(id));
  const stale = runnable.filter((id) => !report.scenarioIds.has(id));
  if (missing.length > 0) failures.push(`manifest scenario(s) have no runtime: ${missing.join(', ')}`);
  if (stale.length > 0) failures.push(`runtime scenario(s) are absent from the manifest: ${stale.join(', ')}`);

  const ratchet = ratchetFailure(inventory, spec);
  if (ratchet) failures.push(ratchet);

  const coveredRoutes = new Set(
    report.coveredEntries.map((row) => `${row.coverage.method} ${row.coverage.routeUrl}`),
  );
  const requiredPrivilegedRoutes = new Map([
    ['POST /api/v1/repos/{owner}/{name}/transfer', 'RepoOwner'],
  ]);
  for (const [label, access] of requiredPrivilegedRoutes) {
    const route = (inventory.routes || []).find(
      (candidate) => `${candidate.method} ${candidate.url}` === label,
    );
    if (!route || !route.reachedFromUi || route.access !== access) {
      failures.push(`required privileged UI route contract drifted: ${access} ${label}`);
    } else if (!coveredRoutes.has(label)) {
      failures.push(`required privileged UI route lacks browser coverage: ${access} ${label}`);
    }
  }
  const privilegedBaselines = new Map([
    ['InstanceAdmin', 19],
    ['RepoAdmin', 28],
    ['OrgAdmin', 8],
  ]);
  for (const [access, baseline] of privilegedBaselines) {
    const routes = (inventory.routes || []).filter(
      (route) => route.reachedFromUi && route.access === access,
    );
    const uncovered = routes.filter(
      (route) => !coveredRoutes.has(`${route.method} ${route.url}`),
    );
    if (routes.length < baseline) {
      failures.push(
        `UI inventory exposes only ${routes.length} ${access} routes; expected at least the ${baseline}-route baseline`,
      );
    }
    if (uncovered.length > 0) {
      failures.push(
        `UI-reached ${access} route(s) lack browser coverage: `
          + uncovered.map((route) => `${route.method} ${route.url}`).join(', '),
      );
    }
  }

  const explicitAccess = new Set(privilegedBaselines.keys());
  const explicitPersonaScenarios = spec.scenarios.filter((scenario) =>
    scenario.covers.some((coverage) => explicitAccess.has(coverage.access))
      || scenario.covers.some((coverage) =>
        coverage.method === 'DELETE'
          && expectedForAccess(coverage.access, 'outsider') === 'denied'));
  for (const scenario of explicitPersonaScenarios) {
    if (scenario.personaOrder?.join(',') !== 'outsider,owner') {
      failures.push(
        `${scenario.id} must run outsider before owner so a privileged owner action cannot make the denial check vacuous`,
      );
    }
    const actions = UI_ACCESS_SWEEP_SCENARIOS.get(scenario.id);
    if (typeof actions?.owner !== 'function' || typeof actions?.outsider !== 'function') {
      failures.push(`${scenario.id} must declare separate owner and outsider browser actions`);
    }
  }

  // The production oracle is exercised with the exact manifest. A complete
  // synthetic matrix must pass, and dropping the outsider result must fail.
  // The regression stand mutates the shared oracle itself, so this is stronger
  // than grepping the runtime for the word "outsider".
  const privileged = spec.scenarios.find((scenario) =>
    scenario.covers.some((coverage) => expectedForAccess(coverage.access, 'outsider') === 'denied'));
  if (privileged) {
    const complete = new Map(REQUIRED_PERSONAS.map((persona) => [
      persona,
      privileged.covers.map((coverage) => ({
        method: coverage.method,
        url: `http://127.0.0.1:1${examplePathFor(coverage.routeUrl)}`,
        status: expectedForAccess(coverage.access, persona) === 'allowed' ? 200 : 403,
      })),
    ]));
    try { assertPersonaResults(privileged, complete, routeUrls); } catch (error) {
      failures.push(`the complete synthetic persona matrix is rejected: ${error.message}`);
    }

    const ownerOnly = new Map([['owner', complete.get('owner')]]);
    let rejected = false;
    try { assertPersonaResults(privileged, ownerOnly, routeUrls); } catch (error) {
      rejected = /outsider/.test(error.message);
    }
    if (!rejected) failures.push('the shared sweep oracle accepted a result with no outsider persona');
  }

  const specificityClaim = {
    id: 'route-specificity-fixture',
    covers: [{
      method: 'GET',
      routeUrl: ['', 'api', 'v1', 'repos', '{owner}', '{name}', 'pipelines', '{id}'].join('/'),
      access: 'RepoRead',
    }],
  };
  const staticNeighborOnly = new Map(REQUIRED_PERSONAS.map((persona) => [
    persona,
    [{
      method: 'GET',
      url: ['http://127.0.0.1:1', 'api', 'v1', 'repos', 'owner', 'repo', 'pipelines', 'workflow-dispatch'].join('/'),
      status: persona === 'owner' ? 200 : 403,
    }],
  ]));
  let rejectedStaticNeighbor = false;
  try { assertPersonaResults(specificityClaim, staticNeighborOnly, routeUrls); } catch (error) {
    rejectedStaticNeighbor = /never reached/.test(error.message);
  }
  if (!rejectedStaticNeighbor) {
    failures.push('the shared sweep oracle credited a static sibling to a placeholder route');
  }
}

const runner = shellCodeOnly(read('scripts/ui-access-sweep-e2e.sh'));
for (const [needle, message] of [
  ['STAND_REBUILD_FRONTEND=1', 'UI access sweep may serve a stale web/build instead of the current frontend source'],
  ['scripts/ephemeral-stand.sh', 'UI access sweep no longer delegates stand ownership to ephemeral-stand.sh'],
  ['--frontend', 'UI access sweep no longer starts the real frontend'],
  ['--no-founder', 'UI access sweep must create both personas itself on an empty database'],
  ['ui-access-sweep-e2e.mjs', 'UI access sweep wrapper no longer executes its browser runtime'],
]) {
  if (!runner.includes(needle)) failures.push(message);
}

const runtime = productionTsSource(read('scripts/ui-access-sweep-e2e.mjs'));
if (!/for \(const scenario of spec\.scenarios\)/.test(runtime)) {
  failures.push('browser runtime no longer iterates every manifest scenario');
}
if (!/const personaOrder = scenario\.personaOrder \|\| REQUIRED_PERSONAS/.test(runtime)
  || !/for \(const persona of personaOrder\)/.test(runtime)) {
  failures.push('browser runtime no longer drives every scenario persona in its declared order');
}
if (!/assertPersonaResults\(scenario, observed, routeUrls\)/.test(runtime)) {
  failures.push('browser runtime no longer hands observed network responses to the shared Access oracle');
}
if (!/responseMatches\(coverage, response, routeUrls\)/.test(runtime)) {
  failures.push('browser runtime no longer filters observed network responses by the winning route registration');
}
if (!/createPageReadinessWaiters\(\{[\s\S]*?timeoutMs: UI_WAIT_MS,[\s\S]*?readyEvent: 'networkAlmostIdle'/.test(runtime)
  || !/lifecycleEvents: true/.test(runtime)
  || !/const readiness = tab\.pageLoads\.wait\(`document for \$\{path\}`\)/.test(runtime)
  || !/readiness\.followNavigation\(result\)/.test(runtime)
  || !/await readiness\.promise/.test(runtime)) {
  failures.push('browser runtime no longer binds navigation readiness to the Page.navigate loader');
}
if (!/const delivery = await waitForValue\(\{[\s\S]*?description: 'webhook fixture delivery id',\s*timeoutMs: UI_WAIT_MS/.test(runtime)) {
  failures.push('webhook delivery fixture no longer uses the shared bounded UI wait');
}

if (packageJson.scripts?.['e2e:ui-access-sweep'] !== 'bash ../scripts/ui-access-sweep-e2e.sh') {
  failures.push('web/package.json must expose the sweep as e2e:ui-access-sweep');
}

if (failures.length > 0) {
  console.error(`❌ UI access sweep contract failed (${failures.length}):`);
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `✅ UI access sweep contract: ${report.inventoryEntries.length} inventory entries traversed, `
    + `${report.coveredEntries.length} covered, owner + outsider enforced, `
    + `ratchet ${spec.ratchet.maxUiRoutesWithoutFrontendTest}`,
);
