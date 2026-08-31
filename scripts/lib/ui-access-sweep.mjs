import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { outrankingRoutes } from './route-specificity.mjs';

export const REQUIRED_PERSONAS = Object.freeze(['owner', 'outsider']);
export const BROWSER_COVERAGE = 'browser';

const OUTSIDER_ALLOWED = new Set(['Public', 'PublicFiltered', 'User']);
const DENIED_STATUSES = new Set([401, 403, 404]);

function requireObject(value, where) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${where} must be an object`);
  }
  return value;
}

function requireString(value, where) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${where} must be a non-empty string`);
  }
  return value;
}

function samePersonas(personas) {
  return Array.isArray(personas)
    && personas.length === REQUIRED_PERSONAS.length
    && REQUIRED_PERSONAS.every((persona, index) => personas[index] === persona);
}

function samePersonaSet(personas) {
  return Array.isArray(personas)
    && personas.length === REQUIRED_PERSONAS.length
    && REQUIRED_PERSONAS.every((persona) => personas.includes(persona));
}

export function loadUiAccessSweepSpec(root) {
  const path = join(root, 'docs/ui-access-sweep.json');
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw new Error(`docs/ui-access-sweep.json cannot be read as JSON: ${error.message}`);
  }
  return parsed;
}

export function inventorySweepEntries(inventory) {
  const entries = [];
  for (const page of inventory.pages || []) {
    for (const control of page.controls || []) {
      for (const call of control.calls || []) {
        if (!call.matched || !call.routeUrl) continue;
        entries.push({
          page: page.route,
          kind: 'control',
          control: control.handler || control.href || control.label || `<${control.tag}>`,
          line: control.line,
          call,
        });
      }
    }
    for (const call of page.passive || []) {
      if (!call.matched || !call.routeUrl) continue;
      entries.push({ page: page.route, kind: 'passive', control: null, line: null, call });
    }
  }
  return entries;
}

function coverageLabel(coverage) {
  const control = coverage.kind === 'control' ? ` control ${coverage.control}` : ' passive load';
  return `${coverage.page}${control}: ${coverage.method} ${coverage.routeUrl}`;
}

function entryMatches(entry, coverage) {
  return entry.page === coverage.page
    && entry.kind === coverage.kind
    && (coverage.kind !== 'control' || entry.control === coverage.control)
    && (coverage.line === undefined || entry.line === coverage.line)
    && entry.call.symbol === coverage.symbol
    && entry.call.method === coverage.method
    && entry.call.routeUrl === coverage.routeUrl
    && entry.call.access === coverage.access;
}

export function validateUiAccessSweep(inventory, spec) {
  requireObject(inventory, 'UI inventory');
  requireObject(spec, 'UI access sweep spec');
  if (spec.version !== 1) throw new Error(`UI access sweep version must be 1, got ${JSON.stringify(spec.version)}`);
  if (!samePersonas(spec.personas)) {
    throw new Error(`UI access sweep personas must be exactly ${REQUIRED_PERSONAS.join(', ')} in that order`);
  }
  const ratchet = requireObject(spec.ratchet, 'UI access sweep ratchet');
  if (!Number.isInteger(ratchet.maxUiRoutesWithoutFrontendTest) || ratchet.maxUiRoutesWithoutFrontendTest < 0) {
    throw new Error('ratchet.maxUiRoutesWithoutFrontendTest must be a non-negative integer');
  }
  if (!Array.isArray(spec.scenarios) || spec.scenarios.length === 0) {
    throw new Error('UI access sweep must declare at least one scenario');
  }

  const inventoryEntries = inventorySweepEntries(inventory);
  if (inventoryEntries.length === 0) {
    throw new Error('UI inventory contains no matched control/passive calls — the sweep would be vacuous');
  }
  const routeByLabel = new Map(
    (inventory.routes || []).map((route) => [`${route.method} ${route.url}`, route]),
  );
  const scenarioIds = new Set();
  const coveredEntries = [];

  for (const [scenarioIndex, scenarioValue] of spec.scenarios.entries()) {
    const scenario = requireObject(scenarioValue, `scenarios[${scenarioIndex}]`);
    const id = requireString(scenario.id, `scenarios[${scenarioIndex}].id`);
    if (scenarioIds.has(id)) throw new Error(`duplicate UI access sweep scenario id: ${id}`);
    scenarioIds.add(id);
    if (scenario.personaOrder !== undefined && !samePersonaSet(scenario.personaOrder)) {
      throw new Error(
        `scenario ${id} personaOrder must contain exactly ${REQUIRED_PERSONAS.join(', ')}`,
      );
    }
    if (!Array.isArray(scenario.covers) || scenario.covers.length === 0) {
      throw new Error(`scenario ${id} covers no inventory entries`);
    }
    for (const [coverageIndex, coverageValue] of scenario.covers.entries()) {
      const coverage = requireObject(coverageValue, `${id}.covers[${coverageIndex}]`);
      for (const field of ['page', 'kind', 'symbol', 'method', 'routeUrl', 'access']) {
        requireString(coverage[field], `${id}.covers[${coverageIndex}].${field}`);
      }
      if (!['control', 'passive'].includes(coverage.kind)) {
        throw new Error(`${id}.covers[${coverageIndex}].kind must be control or passive`);
      }
      if (coverage.kind === 'control') requireString(coverage.control, `${id}.covers[${coverageIndex}].control`);
      if (coverage.line !== undefined && (!Number.isInteger(coverage.line) || coverage.line <= 0)) {
        throw new Error(`${id}.covers[${coverageIndex}].line must be a positive integer when present`);
      }
      if (coverage.access.startsWith('Foreign')) {
        throw new Error(`${id} cannot claim browser-session coverage of foreign credential route ${coverageLabel(coverage)}`);
      }

      const matches = inventoryEntries.filter((entry) => entryMatches(entry, coverage));
      if (matches.length !== 1) {
        throw new Error(
          `${id} coverage ${coverageLabel(coverage)} matched ${matches.length} inventory entries; `
            + 'the manifest must name one live control or passive call exactly',
        );
      }
      const route = routeByLabel.get(`${coverage.method} ${coverage.routeUrl}`);
      if (!route || !route.reachedFromUi) {
        throw new Error(`${id} claims ${coverage.method} ${coverage.routeUrl}, which is not a UI-reached route`);
      }
      coveredEntries.push({ scenario: id, coverage, entry: matches[0] });
    }
  }

  const duplicateCoverage = new Map();
  for (const row of coveredEntries) {
    const key = [
      row.entry.page,
      row.entry.kind,
      row.entry.control || '',
      row.entry.call.symbol,
      row.entry.call.method,
      row.entry.call.routeUrl,
    ].join('\u0000');
    if (!duplicateCoverage.has(key)) duplicateCoverage.set(key, []);
    duplicateCoverage.get(key).push(row.scenario);
  }
  const duplicates = [...duplicateCoverage.values()].filter((ids) => ids.length > 1);
  if (duplicates.length > 0) {
    throw new Error(`UI inventory entry is claimed by multiple scenarios: ${duplicates.map((ids) => ids.join(', ')).join(' | ')}`);
  }

  return { inventoryEntries, coveredEntries, scenarioIds };
}

export function applyUiAccessSweepCoverage(inventory, spec) {
  const report = validateUiAccessSweep(inventory, spec);
  const routeScenarios = new Map();
  for (const row of report.coveredEntries) {
    const scenarios = row.entry.call.browserScenarios || [];
    if (!scenarios.includes(row.scenario)) scenarios.push(row.scenario);
    row.entry.call.browserScenarios = scenarios.sort();
    if (!row.entry.call.testedIn.includes(BROWSER_COVERAGE)) {
      row.entry.call.testedIn.push(BROWSER_COVERAGE);
    }

    const label = `${row.coverage.method} ${row.coverage.routeUrl}`;
    if (!routeScenarios.has(label)) routeScenarios.set(label, new Set());
    routeScenarios.get(label).add(row.scenario);
  }

  for (const route of inventory.routes || []) {
    const scenarios = routeScenarios.get(`${route.method} ${route.url}`);
    if (scenarios) {
      route.browserScenarios = [...scenarios].sort();
      if (!route.testedIn.includes(BROWSER_COVERAGE)) route.testedIn.push(BROWSER_COVERAGE);
    }
  }

  inventory.browserSweep = {
    personas: [...REQUIRED_PERSONAS],
    scenarios: report.scenarioIds.size,
    inventoryEntries: report.inventoryEntries.length,
    coveredEntries: report.coveredEntries.length,
    coveredRoutes: routeScenarios.size,
    maxUiRoutesWithoutFrontendTest: spec.ratchet.maxUiRoutesWithoutFrontendTest,
  };
  return report;
}

export function uiRoutesWithoutFrontendTest(inventory) {
  return (inventory.routes || []).filter(
    (route) => route.reachedFromUi
      && !route.testedIn.some((suite) => ['web', 'smoke', BROWSER_COVERAGE].includes(suite)),
  );
}

export function ratchetFailure(inventory, spec) {
  const actual = uiRoutesWithoutFrontendTest(inventory).length;
  const expected = spec.ratchet.maxUiRoutesWithoutFrontendTest;
  if (actual === expected) return null;
  if (actual > expected) {
    return `UI frontend-test debt grew from the ratchet ${expected} to ${actual}`;
  }
  return `UI frontend-test debt fell to ${actual}, but the ratchet is still ${expected}; lower it to keep the win`;
}

export function expectedForAccess(access, persona) {
  if (!REQUIRED_PERSONAS.includes(persona)) throw new Error(`unknown UI sweep persona: ${persona}`);
  if (persona === 'owner') return 'allowed';
  return OUTSIDER_ALLOWED.has(access) ? 'allowed' : 'denied';
}

function escapeRegex(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function routeTemplateRegex(routeUrl) {
  const segments = routeUrl.split('/').map((segment) => {
    if (/^\{\*[^}]+\}$/.test(segment)) return '.+';
    if (/^\{[^}]+\}$/.test(segment)) return '[^/]+';
    return escapeRegex(segment);
  });
  return new RegExp(`^${segments.join('/')}$`);
}

const ownerMatchers = new WeakMap();

function routeOwnerMatcher(routeUrl, routeUrls) {
  if (!Array.isArray(routeUrls) || routeUrls.length === 0) {
    throw new Error('UI sweep route ownership requires the non-empty router path table');
  }
  let byRoute = ownerMatchers.get(routeUrls);
  if (!byRoute) {
    byRoute = new Map();
    ownerMatchers.set(routeUrls, byRoute);
  }
  let matcher = byRoute.get(routeUrl);
  if (!matcher) {
    const claimed = routeTemplateRegex(routeUrl);
    const rivals = outrankingRoutes(routeUrl, routeUrls).map(routeTemplateRegex);
    matcher = (path) => claimed.test(path) && !rivals.some((rival) => rival.test(path));
    byRoute.set(routeUrl, matcher);
  }
  return matcher;
}

export function routeOwnsPath(routeUrl, path, routeUrls) {
  return routeOwnerMatcher(routeUrl, routeUrls)(path);
}

export function examplePathFor(routeUrl) {
  return routeUrl
    .replace(/\{\*[^}]+\}/g, 'fixture/path')
    .replace(/\{[^}]+\}/g, 'fixture');
}

function responsePath(response) {
  try { return new URL(response.url, 'http://127.0.0.1').pathname; } catch { return String(response.url || ''); }
}

export function assertPersonaResults(scenario, observedByPersona, routeUrls) {
  requireObject(scenario, 'scenario');
  if (!(observedByPersona instanceof Map)) throw new Error(`${scenario.id}: observed results must be a Map`);

  for (const persona of REQUIRED_PERSONAS) {
    if (!observedByPersona.has(persona)) {
      throw new Error(`${scenario.id}: no result for required persona ${persona}`);
    }
    const responses = observedByPersona.get(persona);
    if (!Array.isArray(responses)) throw new Error(`${scenario.id}: ${persona} results must be a list`);

    for (const coverage of scenario.covers) {
      const matches = responses.filter(
        (response) => response.method === coverage.method
          && routeOwnsPath(coverage.routeUrl, responsePath(response), routeUrls),
      );
      if (matches.length === 0) {
        throw new Error(`${scenario.id}: ${persona} never reached ${coverage.method} ${coverage.routeUrl}`);
      }
      const expected = expectedForAccess(coverage.access, persona);
      const wrong = matches.find((response) => expected === 'allowed'
        ? response.status < 200 || response.status >= 400
        : !DENIED_STATUSES.has(response.status));
      if (wrong) {
        throw new Error(
          `${scenario.id}: ${persona} ${coverage.method} ${coverage.routeUrl} returned ${wrong.status}; `
            + `expected ${expected === 'allowed' ? '2xx/3xx' : '401/403/404 denial'} for ${coverage.access}`,
        );
      }
    }
  }
}
