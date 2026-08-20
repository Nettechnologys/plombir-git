#!/usr/bin/env node

// Asserts that everything which *queries* ForgeKeep's metrics asks for series
// the exporter actually produces.
//
// Why this exists: `deploy/prometheus/` and `deploy/grafana/` are production
// artifacts that no CI job used to open. `deploy-config` looks like coverage
// and is not — `docker compose config` merges compose YAML and never reads the
// files compose bind-mounts into the containers. So the alert rules and the
// dashboard drifted away from `crates/rg-http/src/metrics.rs` and nothing said
// so: `SlowRequestDuration` grouped `by (le, route)` over a histogram that had
// no `route` label, and two dashboard panels put the template variable `$route`
// in the *range-interval* position (`..._bucket[$route]`).
//
// That failure is silent by construction, which is what makes it worth a gate.
// A rule grouping by a label nobody exports is valid PromQL: it evaluates, it
// collapses to one global series, and it fires with an empty
// `{{ $labels.route }}` — the on-call gets "P95 on  exceeds 1s". `promtool
// check rules` (see the `observability-config` job) accepts it happily, because
// syntactically there is nothing wrong with it. Only a comparison against the
// exporter catches it.
//
// The check is deliberately fail-closed. Every stage asserts a floor on what it
// extracted, so a parse that stops understanding its input goes red instead of
// vacuously green — the failure mode that made this class expensive in the
// first place (a check that runs and no longer asserts anything).

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource } from './lib/rust-source.mjs';
import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';
import { yamlAnnotatedLines } from './lib/yaml-source.mjs';

const root = process.cwd();
const metricsPath = path.join(root, 'crates/rg-http/src/metrics.rs');
const alertsPath = path.join(root, 'deploy/prometheus/alerts.yml');
const promPath = path.join(root, 'deploy/prometheus/prometheus.yml');
const alertmanagerPath = path.join(root, 'deploy/alertmanager/alertmanager.yml');
const dashboardsDir = path.join(root, 'deploy/grafana/dashboards');
const readmePath = path.join(root, 'deploy/README.md');
const composePath = path.join(root, 'deploy/docker-compose.yml');
const hostdirComposePath = path.join(root, 'deploy/docker-compose.hostdir.yml');
const helperPath = path.join(root, 'deploy/start-observability.sh');

const readme = readFileSync(readmePath, 'utf8');
const composeYml = readFileSync(composePath, 'utf8');
const helper = readFileSync(helperPath, 'utf8');

const failures = [];

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

const { parser: yamlParser, missing: missingYamlParsers } = selectYamlParser();
if (!yamlParser) {
  console.error(
    `No YAML parser available (tried ${missingYamlParsers.join(', ')}) — install either, `
      + 'or the observability contract cannot inspect shipped compose services.',
  );
  process.exit(1);
}

function loadYaml(file, where) {
  const result = parseYamlFile(yamlParser, file);
  if (result.ok) return result.document;
  if (result.kind === 'syntax') {
    failures.push(
      `${where} is not valid YAML — ${yamlParser.name} rejects it:\n     `
        + result.diagnostic.split('\n').join('\n     '),
    );
    return null;
  }
  if (result.kind === 'spawn') {
    console.error(`${yamlParser.name} could not be run on ${where} — ${result.message}`);
  } else if (result.kind === 'output') {
    console.error(`${yamlParser.name} produced unreadable output for ${where} — ${result.message}`);
  } else {
    console.error(`${yamlParser.name} failed on ${where} (exit ${result.status})\n${result.diagnostic}`);
  }
  process.exit(1);
}

function servicePorts(document, service, where) {
  if (!isObject(document)) {
    failures.push(`${where} does not parse into a YAML mapping`);
    return [];
  }
  if (!isObject(document.services)) {
    failures.push(`${where} has no services mapping`);
    return [];
  }
  const definition = document.services[service];
  if (!isObject(definition)) {
    failures.push(`${where} has no services.${service} mapping`);
    return [];
  }
  if (!Array.isArray(definition.ports)) {
    failures.push(`${where} has no services.${service}.ports list`);
    return [];
  }
  return definition.ports;
}

function servicePortOwners(document, value) {
  if (!isObject(document?.services)) return [];
  const owners = [];
  for (const [service, definition] of Object.entries(document.services)) {
    if (!isObject(definition) || !Array.isArray(definition.ports)) continue;
    for (const [index, port] of definition.ports.entries()) {
      if (port === value) owners.push({ service, index });
    }
  }
  return owners;
}

function staticConfigsForJob(document, jobName, where) {
  if (!isObject(document)) {
    failures.push(`${where} does not parse into a YAML mapping`);
    return [];
  }
  if (!Array.isArray(document.scrape_configs)) {
    failures.push(`${where} has no scrape_configs list`);
    return [];
  }

  const jobs = document.scrape_configs.filter(
    (job) => isObject(job) && job.job_name === jobName,
  );
  if (jobs.length !== 1) {
    failures.push(
      `${where} must contain exactly one scrape_configs job named ${JSON.stringify(jobName)}; `
        + `parsed ${jobs.length}`,
    );
    return [];
  }
  if (!Array.isArray(jobs[0].static_configs)) {
    failures.push(`${where} scrape job ${JSON.stringify(jobName)} has no static_configs list`);
    return [];
  }

  return jobs[0].static_configs;
}

function stringMapping(value, where) {
  if (!isObject(value)) {
    failures.push(`${where} must be a mapping`);
    return null;
  }

  const entries = new Map();
  let valid = true;
  for (const [key, item] of Object.entries(value)) {
    if (typeof item !== 'string') {
      failures.push(`${where}.${key} must be a string`);
      valid = false;
      continue;
    }
    entries.set(key, item);
  }
  return valid ? entries : null;
}

function alertRules(document, where) {
  if (!isObject(document)) {
    failures.push(`${where} does not parse into a YAML mapping`);
    return [];
  }
  if (!Array.isArray(document.groups)) {
    failures.push(`${where} has no groups list`);
    return [];
  }

  const parsed = [];
  for (const [groupIndex, group] of document.groups.entries()) {
    const groupWhere = `${where}: groups[${groupIndex}]`;
    if (!isObject(group)) {
      failures.push(`${groupWhere} must be a mapping`);
      continue;
    }
    if (!Array.isArray(group.rules)) {
      failures.push(`${groupWhere}.rules must be a list`);
      continue;
    }

    for (const [ruleIndex, rule] of group.rules.entries()) {
      const ruleWhere = `${groupWhere}.rules[${ruleIndex}]`;
      if (!isObject(rule)) {
        failures.push(`${ruleWhere} must be a mapping`);
        continue;
      }

      const name = rule.alert;
      const expression = rule.expr;
      let valid = true;
      if (typeof name !== 'string' || name.trim() === '') {
        failures.push(`${ruleWhere}.alert must be a non-empty string`);
        valid = false;
      }
      if (typeof expression !== 'string' || expression.trim() === '') {
        failures.push(`${ruleWhere}.expr must be a non-empty string`);
        valid = false;
      }
      const staticLabels = stringMapping(rule.labels, `${ruleWhere}.labels`);
      const annotations = stringMapping(rule.annotations, `${ruleWhere}.annotations`);
      if (!staticLabels || !annotations) valid = false;

      if (valid) parsed.push({ name, expression, staticLabels, annotations });
    }
  }
  return parsed;
}

// Floors: the exporter and its consumers as they stand today. These only ever
// move up. A parse that silently stops matching drops below them and reddens,
// instead of reporting "everything referenced exists" over an empty set.
const MIN_METRICS = 20;
const MIN_ALERT_REFERENCES = 10;
const MIN_DASHBOARD_REFERENCES = 10;

// ---------------------------------------------------------------------------
// 0. The app endpoint shared by compose, Prometheus, the helper and the guide.
// ---------------------------------------------------------------------------

function exactlyOnePort(text, pattern, where) {
  const matches = [...text.matchAll(pattern)];
  if (matches.length !== 1) {
    failures.push(`${where}; parsed ${matches.length}`);
    return undefined;
  }
  return matches[0][1];
}

const composeDocument = loadYaml(composePath, 'deploy/docker-compose.yml');
const hostdirComposeDocument = loadYaml(hostdirComposePath, 'deploy/docker-compose.hostdir.yml');
const prometheusDocument = loadYaml(promPath, 'deploy/prometheus/prometheus.yml');
const alertsDocument = loadYaml(alertsPath, 'deploy/prometheus/alerts.yml');
const alertmanagerDocument = loadYaml(alertmanagerPath, 'deploy/alertmanager/alertmanager.yml');

const composeForgekeepPorts = servicePorts(
  composeDocument,
  'forgekeep',
  'deploy/docker-compose.yml',
);
// YAML parsers intentionally discard comments, so `# HTTP` may select the
// candidate value but cannot prove who owns it. Require that value to have one
// owner in the parsed graph: otherwise a marked sidecar can borrow an identical
// unmarked ForgeKeep mapping and make a value-only check pass.
//
// The marker is read through `yamlAnnotatedLines`, which splits each line the
// way the parser would, rather than with one regex over the raw bytes. A regex
// cannot tell a comment from a `#` inside a quoted scalar, and it cannot tell a
// live mapping from a commented-out one — so a marked entry that compose never
// publishes would have selected the port the rest of this file then checks
// everything else against.
const composeHttpMappings = yamlAnnotatedLines(composeYml)
  .filter(({ comment }) => comment === 'HTTP')
  .map(({ code }) => /^[ \t]+-[ \t]*"([0-9]+):([0-9]+)"[ \t]*$/.exec(code))
  .filter((match) => match !== null);
if (composeHttpMappings.length !== 1) {
  failures.push(
    'deploy/docker-compose.yml must contain exactly one numeric "HOST:CONTAINER" ' +
      `ForgeKeep port mapping marked "# HTTP"; parsed ${composeHttpMappings.length}`,
  );
}
let composeHostPort;
let composeContainerPort;
if (composeHttpMappings.length === 1) {
  const [, published, target] = composeHttpMappings[0];
  const value = `${published}:${target}`;
  const owners = servicePortOwners(composeDocument, value);
  const forgekeepMatches = composeForgekeepPorts.filter((port) => port === value);
  if (owners.length !== 1 || owners[0]?.service !== 'forgekeep' || forgekeepMatches.length !== 1) {
    const found = owners.map(({ service, index }) => `services.${service}.ports[${index}]`);
    failures.push(
      `deploy/docker-compose.yml # HTTP mapping "${value}" must identify exactly one `
        + `services.forgekeep.ports entry; found ${found.length > 0 ? found.join(', ') : 'none'}`,
    );
  } else {
    composeHostPort = published;
    composeContainerPort = target;
  }
}

const hostdirForgekeepPorts = servicePorts(
  hostdirComposeDocument,
  'forgekeep',
  'deploy/docker-compose.hostdir.yml',
);
const hostdirHttpMappings = hostdirForgekeepPorts.flatMap((port) => {
  if (typeof port !== 'string') return [];
  const match = /^127\.0\.0\.1:\$\{FORGEKEEP_HTTP_PORT:-([0-9]+)\}:([0-9]+)$/.exec(port);
  return match ? [match] : [];
});
if (hostdirHttpMappings.length !== 1) {
  failures.push(
    'deploy/docker-compose.hostdir.yml must contain exactly one loopback ForgeKeep mapping ' +
      'with a numeric ${FORGEKEEP_HTTP_PORT:-DEFAULT}; parsed ' + hostdirHttpMappings.length,
  );
}
const hostdirDefaultHostPort = hostdirHttpMappings[0]?.[1];
const hostdirContainerPort = hostdirHttpMappings[0]?.[2];

const forgekeepStaticConfigs = staticConfigsForJob(
  prometheusDocument,
  'forgekeep',
  'deploy/prometheus/prometheus.yml',
);
const forgekeepTargets = [];
for (const [index, config] of forgekeepStaticConfigs.entries()) {
  if (!isObject(config) || !Array.isArray(config.targets)) {
    failures.push(
      'deploy/prometheus/prometheus.yml scrape job "forgekeep" has no targets list at '
        + `static_configs[${index}]`,
    );
    continue;
  }
  forgekeepTargets.push(...config.targets);
}
const prometheusPorts = forgekeepTargets.flatMap((target) => {
  if (typeof target !== 'string') return [];
  const match = /^forgekeep:([0-9]+)$/.exec(target);
  return match ? [match[1]] : [];
});
if (prometheusPorts.length !== 1) {
  failures.push(
    'deploy/prometheus/prometheus.yml must contain exactly one forgekeep:PORT target in the '
      + `forgekeep job; parsed ${prometheusPorts.length}`,
  );
}
const prometheusPort = prometheusPorts[0];

const readmeAccessPort = exactlyOnePort(
  readme,
  /^Access:\s+\*\*http:\/\/localhost:([0-9]+)\*\*\s*$/gm,
  'deploy/README.md must contain exactly one numeric Quick Start Access URL',
);
const readmeHostdirCurlPort = exactlyOnePort(
  readme,
  /^curl -sf http:\/\/127\.0\.0\.1:([0-9]+)\/health && echo OK\s*$/gm,
  'deploy/README.md must contain exactly one numeric hostdir health-check URL',
);
const readmeHostdirProsePort = exactlyOnePort(
  readme,
  /^HTTP is published on `127\.0\.0\.1:([0-9]+)` only/gm,
  'deploy/README.md must contain exactly one numeric hostdir published-port statement',
);
const readmeScrapePort = exactlyOnePort(
  readme,
  /Prometheus scrapes the app at `forgekeep:([0-9]+)`/g,
  'deploy/README.md must contain exactly one numeric ForgeKeep Prometheus target',
);
const readmeArchitecturePort = exactlyOnePort(
  readme,
  /^│  :([0-9]+)\/metrics\s+│/gm,
  'deploy/README.md architecture must contain exactly one numeric ForgeKeep metrics endpoint',
);

const hardcodedHelperEndpoints = [
  ...[...helper.matchAll(/http:\/\/localhost:([0-9]+)\/health/g)]
    .map(([, port]) => `localhost:${port}/health`),
  ...[...helper.matchAll(/^echo "  ForgeKeep:[^\n]*http:\/\/localhost:([0-9]+)\/metrics"$/gm)]
    .map(([, port]) => `localhost:${port}/metrics`),
];
if (hardcodedHelperEndpoints.length > 0) {
  failures.push(
    `deploy/start-observability.sh hardcodes ForgeKeep app endpoint(s): ${hardcodedHelperEndpoints.join(', ')}`,
  );
}
for (const endpoint of ['health', 'metrics']) {
  if (helper.includes(`http://localhost:\${FORGEKEEP_HOST_PORT}/${endpoint}`)) continue;
  failures.push(
    `deploy/start-observability.sh must use the compose-derived \${FORGEKEEP_HOST_PORT} for /${endpoint}`,
  );
}

function comparePorts(actual, expected, message) {
  if (actual !== undefined && expected !== undefined && actual !== expected) {
    failures.push(`${message}: ${actual} != ${expected}`);
  }
}

comparePorts(
  hostdirDefaultHostPort,
  composeHostPort,
  'the two shipped app compose files disagree on their default published HTTP port',
);
comparePorts(
  hostdirContainerPort,
  composeContainerPort,
  'the two shipped app compose files disagree on the ForgeKeep container HTTP port',
);
comparePorts(
  prometheusPort,
  composeContainerPort,
  'Prometheus ForgeKeep target disagrees with the compose container HTTP port',
);
comparePorts(
  readmeAccessPort,
  composeHostPort,
  'deploy/README.md Quick Start Access URL disagrees with the compose published HTTP port',
);
comparePorts(
  readmeHostdirCurlPort,
  hostdirDefaultHostPort,
  'deploy/README.md hostdir health check disagrees with its compose default host port',
);
comparePorts(
  readmeHostdirProsePort,
  hostdirDefaultHostPort,
  'deploy/README.md hostdir prose disagrees with its compose default host port',
);
comparePorts(
  readmeScrapePort,
  prometheusPort,
  'deploy/README.md Prometheus target disagrees with prometheus.yml',
);
comparePorts(
  readmeArchitecturePort,
  composeContainerPort,
  'deploy/README.md architecture disagrees with the compose container HTTP port',
);

// ---------------------------------------------------------------------------
// 1. What the exporter actually publishes.
// ---------------------------------------------------------------------------

// A metric declaration in metrics.rs is always "<name>", "<help>" — two
// adjacent string literals — whether it is spelled `Opts::new(..)`,
// `HistogramOpts::new(..)` or through the `register_counter!` macro. Anchoring
// on that pair rather than on a constructor name keeps this from pinning the
// shape of our own code: a fourth spelling of the same declaration is still
// picked up, and the label slice `&["a", "b"]` in the same statement is what
// makes it a vector.
const METRIC_NAME_AND_HELP = /"([a-z][a-z0-9_]*)"\s*,\s*"/;
const LABEL_SLICE = /&\[\s*("(?:[a-z][a-z0-9_]*)"(?:\s*,\s*"(?:[a-z][a-z0-9_]*)")*)\s*,?\s*\]/;

/** @type {Map<string, {labels: Set<string>, histogram: boolean}>} */
const exported = new Map();

for (const statement of productionRustSource(readFileSync(metricsPath, 'utf8')).split(';')) {
  const named = statement.match(METRIC_NAME_AND_HELP);
  if (!named) continue;

  const labelSlice = statement.match(LABEL_SLICE);
  const labels = labelSlice
    ? new Set([...labelSlice[1].matchAll(/"([a-z][a-z0-9_]*)"/g)].map((m) => m[1]))
    : new Set();

  exported.set(named[1], { labels, histogram: /HistogramOpts::new/.test(statement) });
}

if (exported.size < MIN_METRICS) {
  failures.push(
    `Only ${exported.size} metrics parsed out of ${path.relative(root, metricsPath)} ` +
      `(floor ${MIN_METRICS}) — the declaration parse no longer matches the source, ` +
      `so nothing below is really being checked.`,
  );
}

// ---------------------------------------------------------------------------
// 2. Labels that exist without the exporter emitting them.
// ---------------------------------------------------------------------------

// Target labels attached by Prometheus to ForgeKeep's own static scrape
// configs, plus the labels the existing expression/alert contract treats as
// built in. Read the parsed job graph so quoted YAML has identical semantics
// and a neighboring scrape job cannot lend ForgeKeep one of its labels.
const targetLabels = new Set(['job', 'instance', 'alertname', 'severity']);
for (const [index, config] of forgekeepStaticConfigs.entries()) {
  if (!isObject(config)) continue; // the target validation above already reports this entry
  if (config.labels === undefined) continue;
  if (!isObject(config.labels)) {
    failures.push(
      'deploy/prometheus/prometheus.yml scrape job "forgekeep" has a non-mapping labels value at '
        + `static_configs[${index}]`,
    );
    continue;
  }
  for (const label of Object.keys(config.labels)) targetLabels.add(label);
}

// Metric families that come from somewhere other than ForgeKeep's exporter.
const FOREIGN_METRIC = /^(up|node_|prometheus_|go_|process_|alertmanager_|grafana_)/;

// PromQL aggregation operators. `topk`/`bottomk` (and the experimental
// sampling aggregators) retain the original series labels; the others drop
// every label unless a `by` or `without` modifier says otherwise.
const AGGREGATION = /\b(sum|avg|min|max|count|stddev|stdvar|group|count_values|topk|bottomk|quantile|limitk|limit_ratio)\b\s*(?:(by|without)\s*\(([^)]*)\)\s*)?\(/g;
const LABEL_PRESERVING_AGGREGATIONS = new Set(['topk', 'bottomk', 'limitk', 'limit_ratio']);

// PromQL identifiers that are syntax, not series.
const PROMQL_KEYWORDS = new Set([
  'and', 'or', 'unless', 'by', 'without', 'on', 'ignoring', 'group_left', 'group_right',
  'offset', 'bool', 'start', 'end', 'inf', 'nan',
]);

function closingParen(expr, open) {
  let depth = 0;
  for (let i = open; i < expr.length; i += 1) {
    if (expr[i] === '(') depth += 1;
    else if (expr[i] === ')') {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

function labelsIn(list) {
  return new Set([...list.matchAll(/[a-z_][a-z0-9_]*/gi)].map((m) => m[0]));
}

/** Parse aggregation modifiers while keeping their expression boundaries. */
function aggregations(expr) {
  const found = [];
  for (const match of expr.matchAll(AGGREGATION)) {
    const open = match.index + match[0].length - 1;
    const close = closingParen(expr, open);
    if (close === -1) continue;

    const trailing = expr.slice(close + 1).match(/^\s*(by|without)\s*\(([^)]*)\)/);
    const modifier = match[2] ?? trailing?.[1];
    const list = match[3] ?? trailing?.[2];
    found.push({
      operator: match[1],
      modifier,
      labels: list === undefined ? new Set() : labelsIn(list),
      open,
      close,
    });
  }
  return found;
}

/** Whether every aggregation in an alert expression leaves a label available. */
function alertExpressionKeepsLabel(expr, label) {
  return aggregations(expr).every(({ operator, modifier, labels }) => {
    if (LABEL_PRESERVING_AGGREGATIONS.has(operator)) return true;
    if (modifier === 'by') return labels.has(label);
    if (modifier === 'without') return !labels.has(label);
    return false;
  });
}

/**
 * Metric references in one PromQL expression, each with the labels the
 * expression demands of it (matchers plus the `by (...)` it is aggregated under).
 *
 * @returns {Array<{metric: string, labels: Set<string>}>}
 */
function metricReferences(expr) {
  /** @type {Map<number, Set<string>>} */
  const groupingAt = new Map();

  // Attribute each `by (...)` to the metrics inside the aggregation it belongs
  // to, not to the whole expression: `sum by (le, route) (rate(x[5m])) / sum(y)`
  // must not claim `y` is grouped by route. Both spellings PromQL accepts are
  // handled — `sum by (L) (...)` and `sum(...) by (L)`.
  for (const aggregation of aggregations(expr)) {
    if (aggregation.modifier !== 'by') continue;
    for (let i = aggregation.open; i < aggregation.close; i += 1) {
      groupingAt.set(i, new Set([...(groupingAt.get(i) ?? []), ...aggregation.labels]));
    }
  }

  // Blank out the label lists of the aggregation modifiers, so `le` and `route`
  // inside `by (le, route)` are read as the grouping they are (already
  // attributed above) and not as two series named `le` and `route`. Replaced
  // with spaces rather than removed: the `groupingAt` offsets were taken on the
  // original string and have to keep lining up.
  const masked = expr.replace(
    /\b(by|without|on|ignoring|group_left|group_right)\s*\([^)]*\)/g,
    (match) => ' '.repeat(match.length),
  );

  const references = [];
  const SELECTOR = /\b([a-zA-Z_][a-zA-Z0-9_:]*)\s*(\{[^}]*\})?/g;
  for (const selector of masked.matchAll(SELECTOR)) {
    const [whole, metric, matchers] = selector;
    // A name followed by `(` is a function, and one preceded by `.` or `$` is a
    // template fragment, not a series.
    if (!matchers && /^\s*\(/.test(masked.slice(selector.index + whole.length))) continue;
    if (/[.$"'`]/.test(masked[selector.index - 1] ?? '')) continue;
    if (PROMQL_KEYWORDS.has(metric)) continue;

    const labels = new Set(groupingAt.get(selector.index) ?? []);
    for (const [, label] of (matchers ?? '').matchAll(/([a-z_][a-z0-9_]*)\s*(?:=~|!~|!=|=)/g)) {
      labels.add(label);
    }
    references.push({ metric, labels });
  }
  return references;
}

/** Check one expression against the exporter; `where` names it in failures. */
function checkExpression(expr, where) {
  const references = metricReferences(expr);

  for (const { metric, labels } of references) {
    if (FOREIGN_METRIC.test(metric)) continue;

    // `_bucket` / `_sum` / `_count` are the series a histogram expands into;
    // `le` exists only on the bucket series.
    const suffix = metric.match(/_(bucket|sum|count)$/)?.[1];
    const base = suffix ? metric.slice(0, -(suffix.length + 1)) : metric;
    const declared = exported.get(base);

    if (!declared) {
      failures.push(`${where}: queries \`${metric}\`, which the exporter does not register`);
      continue;
    }
    if (suffix && !declared.histogram) {
      failures.push(
        `${where}: queries \`${metric}\`, but \`${base}\` is not a histogram — ` +
          `no ${suffix} series exists`,
      );
      continue;
    }

    const allowed = new Set([...declared.labels, ...targetLabels]);
    if (suffix === 'bucket') allowed.add('le');

    for (const label of labels) {
      if (allowed.has(label)) continue;
      failures.push(
        `${where}: asks \`${metric}\` for label \`${label}\`, which the exporter does not emit ` +
          `(it emits: ${[...declared.labels].join(', ') || 'no labels'})`,
      );
    }
  }

  return references.filter(({ metric }) => !FOREIGN_METRIC.test(metric)).length;
}

// ---------------------------------------------------------------------------
// 3. Alert rules.
// ---------------------------------------------------------------------------

let alertReferences = 0;

// Prometheus reads a YAML graph, so the contract must not attach semantics to
// one textual spelling of a key. The structural walk also keeps fields owned
// by their exact groups[i].rules[j] entry instead of borrowing from a neighbor.
const rules = alertRules(alertsDocument, 'deploy/prometheus/alerts.yml');
if (rules.length === 0) {
  failures.push(`No alert rules parsed out of ${path.relative(root, alertsPath)}`);
}

/** Every parsed alert, keyed by name: its static labels and its expression. */
const alertsByName = new Map();

for (const { name, expression, staticLabels, annotations } of rules) {
  alertsByName.set(name, { staticLabels, expression });
  alertReferences += checkExpression(expression, `alerts.yml: ${name}`);

  // An annotation interpolating `$labels.x` is a promise that x is on the
  // alert. A raw vector retains its labels, `by` retains only its list, and
  // `without` retains everything except its list. We cannot know labels from
  // a foreign exporter, so do not make a false claim about an all-foreign rule.
  const references = metricReferences(expression);
  const allMetricsForeign = references.length > 0 && references.every(({ metric }) => FOREIGN_METRIC.test(metric));
  const annotationText = [...annotations.values()].join('\n');
  for (const [, label] of annotationText.matchAll(/\$labels\.([a-z_][a-z0-9_]*)/g)) {
    if (allMetricsForeign || targetLabels.has(label) || alertExpressionKeepsLabel(expression, label)) continue;
    failures.push(
      `alerts.yml: ${name} interpolates {{ $labels.${label} }}, but its expr does not retain ` +
        `\`${label}\` — the alert would fire with that placeholder empty`,
    );
  }
}

if (alertReferences < MIN_ALERT_REFERENCES) {
  failures.push(
    `Only ${alertReferences} ForgeKeep metric references found in alerts.yml ` +
      `(floor ${MIN_ALERT_REFERENCES}) — the rule parse has stopped reading the file`,
  );
}

// ---------------------------------------------------------------------------
// 3b. Alertmanager inhibition rules.
// ---------------------------------------------------------------------------
//
// An `equal:` list is the one place in the observability stack where a label
// that is *absent* silently means something. Alertmanager compares label
// VALUES, and a missing label is the empty string — so two alerts that both
// lack the label compare equal and the rule matches them. A rule written for
// one pair of route-scoped alerts therefore also fires between every pair of
// alerts that has nothing to do with routes.
//
// That is not a syntax error, so `amtool check-config` (the `observability-
// config` job) accepts it, and it is not visible in the rule either: it takes
// the alert definitions to see which labels can be absent. Measured on
// Alertmanager v0.27.0 with the config in this repository: `HighGitOperation-
// Failure` suppressed `LowDiskSpace`, `HighMemoryUsage` and `CIJobQueueBuildup`
// while it burned (card_2206bf4038e7).
//
// So the rule asserted here is: every label named in `equal:` must be carried
// by every alert that can match the source, and by every alert that can match
// the target.

/**
 * Whether an alert carries `label` on every instance it can produce.
 *
 * Four ways to have it: an intrinsic alert label, a static `labels:` entry, a
 * target label Prometheus attaches to every scraped series, or a series the
 * expression reads that the exporter emits with that label and whose
 * aggregations do not drop it. Asking only "does the expression retain it"
 * would answer yes for every raw-vector alert — the operator retains all
 * labels, including ones the metric never had.
 *
 * Foreign metrics keep their benefit of the doubt, the same way section 3 does:
 * we cannot know what somebody else's exporter emits, and claiming a label is
 * absent there would be a fabricated failure.
 */
// Labels Prometheus attaches to every scraped series. `alertname` and
// `severity` live in `targetLabels` because an expression may legitimately
// reference them, but on an alert they come from the rule's own `labels:`
// block — treating them as always-present would make every alert look like it
// carries a severity it never declared.
const ATTACHED_LABELS = new Set([...targetLabels].filter((label) => label !== 'alertname' && label !== 'severity'));
// Prometheus derives this label from the alerting rule itself. It exists on
// every produced alert regardless of which labels its expression carries.
const INTRINSIC_ALERT_LABELS = new Set(['alertname']);

function alertCarriesLabel(alert, label) {
  if (
    INTRINSIC_ALERT_LABELS.has(label)
    || alert.staticLabels.has(label)
    || ATTACHED_LABELS.has(label)
  ) return true;
  if (!alertExpressionKeepsLabel(alert.expression, label)) return false;

  const references = metricReferences(alert.expression);
  if (references.length === 0) return false;
  return references.some(({ metric }) => {
    if (FOREIGN_METRIC.test(metric)) return true;
    const suffix = metric.match(/_(bucket|sum|count)$/)?.[1];
    const base = suffix ? metric.slice(0, -(suffix.length + 1)) : metric;
    return exported.get(base)?.labels.has(label) ?? false;
  });
}

/**
 * Label constraints of one side of an inhibit rule.
 *
 * Both the legacy `*_match` / `*_match_re` maps and the current `*_matchers`
 * list are read, because the file has historically mixed them. A constraint is
 * recorded as `{ value, requiresPresence }`: `route != ""` requires the label
 * to exist without pinning a value, which is exactly the shape that fixes the
 * defect above and must not read as "no constraint".
 */
function sideConstraints(rule, side, where) {
  const constraints = new Map();

  for (const key of [`${side}_match`, `${side}_match_re`]) {
    const block = rule[key];
    if (block === undefined) continue;
    if (!isObject(block)) {
      failures.push(`${where}.${key} must be a mapping`);
      continue;
    }
    for (const [label, value] of Object.entries(block)) {
      if (!/^[a-z_][a-z0-9_]*$/.test(label) || typeof value !== 'string') {
        failures.push(`${where}.${key}.${label} must be a string matcher for a valid label name`);
        continue;
      }
      constraints.set(label, { value, regex: key.endsWith('_re'), negated: false, requiresPresence: value !== '' });
    }
  }

  const key = `${side}_matchers`;
  const matchers = rule[key];
  if (matchers !== undefined && !Array.isArray(matchers)) {
    failures.push(`${where}.${key} must be a list`);
    return constraints;
  }
  for (const [index, matcher] of (matchers ?? []).entries()) {
    if (typeof matcher !== 'string') {
      failures.push(`${where}.${key}[${index}] must be a string matcher`);
      continue;
    }
    const parsed = matcher.match(/^([a-z_][a-z0-9_]*)\s*(=~|!~|!=|=)\s*"((?:[^"\\]|\\.)*)"$/);
    if (!parsed) {
      failures.push(`${where}.${key}[${index}] ${JSON.stringify(matcher)} is not a matcher this check can read`);
      continue;
    }
    const [, label, operator, value] = parsed;
    // `label = "x"` and `label =~ "x"` require the label; so does `label != ""`,
    // which is the idiomatic "must be present". `label != "x"` does not.
    const negated = operator.startsWith('!');
    const requiresPresence = negated ? value === '' : value !== '';
    constraints.set(label, { value, regex: operator.includes('~'), negated, requiresPresence });
  }

  return constraints;
}

/**
 * Parsed inhibit rules, preserving ownership of every field by its list item.
 *
 * Alertmanager reads a YAML graph. Reading that same graph here makes quoted
 * keys and harmless indentation choices equivalent, while explicit type checks
 * make a shape the checker cannot understand fail instead of disappearing.
 */
function inhibitRules(document, where) {
  if (!isObject(document)) {
    failures.push(`${where} does not parse into a YAML mapping`);
    return [];
  }
  if (!Array.isArray(document.inhibit_rules)) {
    failures.push(`${where}.inhibit_rules must be a list`);
    return [];
  }
  if (document.inhibit_rules.length < 2) {
    failures.push(
      `Only ${document.inhibit_rules.length} inhibit rule(s) parsed out of alertmanager.yml — ` +
        'the list form changed, fix this check rather than letting it pass over rules it cannot read',
    );
  }

  const parsed = [];
  for (const [index, rule] of document.inhibit_rules.entries()) {
    const ruleWhere = `${where}.inhibit_rules[${index}]`;
    if (!isObject(rule)) {
      failures.push(`${ruleWhere} must be a mapping`);
      continue;
    }

    let equal = null;
    if (rule.equal !== undefined) {
      if (!Array.isArray(rule.equal)) {
        failures.push(`${ruleWhere}.equal must be a list`);
      } else {
        equal = [];
        for (const [labelIndex, label] of rule.equal.entries()) {
          if (typeof label !== 'string' || !/^[a-z_][a-z0-9_]*$/.test(label)) {
            failures.push(`${ruleWhere}.equal[${labelIndex}] must be a valid label name string`);
            continue;
          }
          equal.push(label);
        }
        if (equal.length === 0) {
          failures.push(`${ruleWhere}.equal must contain at least one readable label name`);
        }
      }
    }
    parsed.push({ rule, where: ruleWhere, equal });
  }
  return parsed;
}

/**
 * The alerts whose declared labels can satisfy every constraint of one side.
 *
 * Values are only compared where the alert fixes one — its `alertname` and its
 * static `labels:`. A label an alert gains from the series it reads has a value
 * this check cannot know, so such an alert stays a candidate rather than being
 * excluded by a guess; presence is still decided, which is the part `equal:`
 * turns on.
 */
function alertsMatching(constraints) {
  return [...alertsByName].filter(([name, alert]) => {
    for (const [label, { value, regex, negated, requiresPresence }] of constraints) {
      const present = alertCarriesLabel(alert, label);
      if (requiresPresence && !present) return false;

      const fixed = label === 'alertname' ? name : alert.staticLabels.get(label);
      // Absent means the empty string to Alertmanager; dynamic means unknown.
      if (fixed === undefined && present) continue;
      const declared = fixed ?? '';
      const hit = regex ? new RegExp(`^(?:${value})$`).test(declared) : declared === value;
      if (negated ? hit : !hit) return false;
    }
    return true;
  });
}

for (const { rule, where, equal } of inhibitRules(
  alertmanagerDocument,
  'deploy/alertmanager/alertmanager.yml',
)) {
  const constraintsBySide = new Map(
    ['source', 'target'].map((side) => [side, sideConstraints(rule, side, where)]),
  );
  if (equal === null || equal.length === 0) continue; // a rule without `equal` has no trap to spring
  for (const side of ['source', 'target']) {
    const matching = alertsMatching(constraintsBySide.get(side));
    if (matching.length === 0) {
      failures.push(
        `${where}: no alert in alerts.yml can match its ${side} — the rule is either dead or its ` +
          'matchers name something the rules no longer declare',
      );
      continue;
    }
    for (const label of equal) {
      for (const [name, alert] of matching) {
        if (alertCarriesLabel(alert, label)) continue;
        failures.push(
          `${where} equals on \`${label}\`, but ${name} matches its ${side} without carrying that ` +
            'label. Alertmanager compares label values and reads an absent label as the empty ' +
            `string, so this rule also fires between every pair of alerts that has no \`${label}\` ` +
            '— constrain the side to alerts that carry it, or drop it from equal:.',
        );
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 4. Grafana dashboards.
// ---------------------------------------------------------------------------

const dashboards = readdirSync(dashboardsDir).filter((name) => name.endsWith('.json')).sort();
if (dashboards.length === 0) {
  failures.push(`No dashboards found in ${path.relative(root, dashboardsDir)}`);
}

let dashboardReferences = 0;
let mainDashboardCount = 0;
let mainDashboardPanels;

for (const file of dashboards) {
  let dashboard;
  try {
    dashboard = JSON.parse(readFileSync(path.join(dashboardsDir, file), 'utf8'));
  } catch (error) {
    failures.push(`${file}: not valid JSON — ${error.message}`);
    continue;
  }

  const variables = new Set((dashboard.templating?.list ?? []).map((v) => v.name));
  const panels = dashboard.panels;
  if (!Array.isArray(panels)) {
    failures.push(`${file}: panels must be an array`);
    continue;
  }

  if (dashboard.uid === 'forgekeep-main') {
    mainDashboardCount += 1;
    const panelsById = new Map();
    const panelIdsByTitle = new Map();

    if (panels.length === 0) {
      failures.push(`${file}: forgekeep-main has no panels — the panel inventory is empty`);
    }

    for (const panel of panels) {
      const id = panel.id;
      const title = String(panel.title ?? '').trim();
      if (!Number.isInteger(id) || id <= 0) {
        failures.push(`${file}: forgekeep-main panel has invalid id ${JSON.stringify(id)}`);
        continue;
      }
      if (!title) {
        failures.push(`${file}: forgekeep-main panel ${id} has no non-empty title`);
        continue;
      }
      if (panelsById.has(id)) {
        failures.push(`${file}: forgekeep-main defines panel id ${id} more than once`);
        continue;
      }
      const duplicateTitleId = panelIdsByTitle.get(title);
      if (duplicateTitleId !== undefined) {
        failures.push(
          `${file}: forgekeep-main panels ${duplicateTitleId} and ${id} share title ${JSON.stringify(title)}`,
        );
      }
      panelsById.set(id, title);
      panelIdsByTitle.set(title, id);
    }

    if (mainDashboardPanels === undefined) mainDashboardPanels = panelsById;
  }

  // Template queries are expressions too: `label_values(metric, label)` names
  // both a metric and a label and drifts exactly like a panel does.
  for (const variable of dashboard.templating?.list ?? []) {
    const labelValues = String(variable.query ?? '').match(
      /label_values\(\s*([a-zA-Z_][a-zA-Z0-9_:]*)[^,]*,\s*([a-z_][a-z0-9_]*)\s*\)/,
    );
    if (!labelValues) continue;
    dashboardReferences += checkExpression(
      `${labelValues[1]}{${labelValues[2]}="x"}`,
      `${file}: variable $${variable.name}`,
    );
  }

  for (const panel of panels) {
    for (const target of panel.targets ?? []) {
      const expr = String(target.expr ?? '');
      const where = `${file}: panel ${panel.id} "${panel.title}" (${target.refId})`;

      // A template variable in the range-interval position is not a label
      // mismatch, it is a syntax error: `rate(x[$route])` asks Prometheus to
      // read a duration where the dashboard substitutes a route name. The
      // panel renders an error, not a graph — which is how it survived here.
      for (const [, inside] of expr.matchAll(/\[([^\]]*)\]/g)) {
        if (!/^\s*\d+(\.\d+)?(ms|s|m|h|d|w|y)(\s*:\s*\d+\w+)?\s*$/.test(inside)) {
          failures.push(
            `${where}: \`[${inside}]\` is not a duration — a range interval cannot hold a ` +
              `template variable or expression`,
          );
        }
      }

      for (const [, variable] of expr.matchAll(/\$(?!__)([a-zA-Z_][a-zA-Z0-9_]*)/g)) {
        if (!variables.has(variable)) {
          failures.push(`${where}: uses $${variable}, which the dashboard does not declare`);
        }
      }

      // Substitute declared variables away before the PromQL walk, so
      // `{route=~"$route"}` reads as a matcher on `route` rather than a
      // reference to a series named after the variable.
      dashboardReferences += checkExpression(expr.replace(/\$[a-zA-Z_][a-zA-Z0-9_]*/g, 'x'), where);
    }
  }
}

if (dashboardReferences < MIN_DASHBOARD_REFERENCES) {
  failures.push(
    `Only ${dashboardReferences} ForgeKeep metric references found across the dashboards ` +
      `(floor ${MIN_DASHBOARD_REFERENCES}) — the panel walk has stopped reading them`,
  );
}

// ---------------------------------------------------------------------------
// 5. The observability inventories in deploy/README.md.
// ---------------------------------------------------------------------------

// The deployment guide is the third consumer of this contract: an operator
// writes their own queries from its Labels column, so a stale row there is the
// same drift arriving one step later.
let documented = 0;

// Only the metric tables: the guide has other tables whose first column is also
// a backticked identifier (env vars, compose services, troubleshooting symptoms).
const metricsSection = readme.match(/^## .*Available Metrics[\s\S]*?(?=^## )/m)?.[0];
if (!metricsSection) {
  failures.push('deploy/README.md has no "Available Metrics" section — the guide no longer documents the exporter');
}

for (const [, name, , labelCell] of (metricsSection ?? '').matchAll(
  /^\|\s*`([a-z][a-z0-9_]*)`\s*\|\s*([^|]*?)\s*\|\s*([^|]*?)\s*\|/gm,
)) {
  const declared = exported.get(name);
  if (!declared) {
    failures.push(`deploy/README.md documents \`${name}\`, which the exporter does not register`);
    continue;
  }
  documented += 1;

  // The Business table has no Labels column — its third cell is the
  // description. Only rows whose cell reads as a label list are compared.
  const cellIsLabelList = labelCell.trim() === '-' || /^[`a-z_,\s|()\\]*$/.test(labelCell);
  if (!cellIsLabelList) continue;

  for (const label of declared.labels) {
    if (new RegExp(`\\b${label}\\b`).test(labelCell)) continue;
    failures.push(
      `deploy/README.md: \`${name}\` is exported with label \`${label}\`, but its Labels ` +
        `column says "${labelCell}"`,
    );
  }
}

if (documented === 0) {
  failures.push('No metric rows parsed out of deploy/README.md — the table format changed');
}

// The dashboard panel list is an exact operator-facing inventory. IDs make the
// mapping stable and titles remain exact because that is what an operator sees
// in Grafana. Checking both directions catches a panel silently removed from
// the dashboard and a newly shipped panel omitted from the guide.
const dashboardPanelSection = readme.match(
  /^## [^\n]*Dashboard Panels[^\n]*\n[\s\S]*?(?=^## |(?![\s\S]))/m,
)?.[0];
if (!dashboardPanelSection) {
  failures.push('deploy/README.md has no "Dashboard Panels" section — the guide no longer inventories forgekeep-main');
}

const documentedPanels = new Map();
const documentedPanelIdsByTitle = new Map();
for (const [, rawId, rawTitle] of (dashboardPanelSection ?? '').matchAll(
  /^- Grafana panel `([1-9][0-9]*)`: \*\*(.+?)\*\* — .+$/gm,
)) {
  const id = Number(rawId);
  const title = rawTitle.trim();
  if (documentedPanels.has(id)) {
    failures.push(`deploy/README.md documents Grafana panel id ${id} more than once`);
    continue;
  }
  const duplicateTitleId = documentedPanelIdsByTitle.get(title);
  if (duplicateTitleId !== undefined) {
    failures.push(
      `deploy/README.md documents Grafana panels ${duplicateTitleId} and ${id} with the same title ${JSON.stringify(title)}`,
    );
  }
  documentedPanels.set(id, title);
  documentedPanelIdsByTitle.set(title, id);
}

if (documentedPanels.size === 0) {
  failures.push('No Grafana panel rows parsed out of deploy/README.md — the Dashboard Panels list format changed');
}

if (mainDashboardCount !== 1) {
  failures.push(
    `Expected exactly one dashboard with uid \`forgekeep-main\`, found ${mainDashboardCount}`,
  );
}

for (const [id, title] of mainDashboardPanels ?? []) {
  const documentedTitle = documentedPanels.get(id);
  if (documentedTitle === undefined) {
    failures.push(`deploy/README.md does not document Grafana panel \`${id}\` ${JSON.stringify(title)}`);
  } else if (documentedTitle !== title) {
    failures.push(
      `deploy/README.md: Grafana panel \`${id}\` is titled ${JSON.stringify(title)}, but the guide says ` +
        JSON.stringify(documentedTitle),
    );
  }
}

for (const [id, title] of documentedPanels) {
  if (mainDashboardPanels?.has(id)) continue;
  failures.push(
    `deploy/README.md documents Grafana panel \`${id}\` ${JSON.stringify(title)}, which forgekeep-main does not contain`,
  );
}

// The same guide also carries an operator-facing inventory of alert rules. It
// must be an exact inventory, not a sample: a missing row tells the operator an
// alert does not exist, while a stale row promises a notification Prometheus
// will never produce. Severity is cheap to compare because alertsByName already
// holds the rule's static labels; thresholds remain prose and are deliberately
// outside this structural contract.
const alertSection = readme.match(/^## [^\n]*Alert Rules[^\n]*\n[\s\S]*?(?=^## |(?![\s\S]))/m)?.[0];
if (!alertSection) {
  failures.push('deploy/README.md has no "Alert Rules" section — the guide no longer inventories alerts.yml');
}

/** Every documented alert, keyed by name, with the severity promised to operators. */
const documentedAlerts = new Map();
for (const [, name, description] of (alertSection ?? '').matchAll(
  /^- \*\*([A-Za-z][A-Za-z0-9_]*)\*\*:\s*(.+)$/gm,
)) {
  if (documentedAlerts.has(name)) {
    failures.push(`deploy/README.md documents alert \`${name}\` more than once`);
    continue;
  }

  const severity = description.match(/\b(critical|warning|info)\b/i)?.[1].toLowerCase();
  if (!severity) {
    failures.push(
      `deploy/README.md: alert \`${name}\` has no readable critical/warning/info severity`,
    );
  }
  documentedAlerts.set(name, severity);
}

if (documentedAlerts.size === 0) {
  failures.push('No alert rows parsed out of deploy/README.md — the alert list format changed');
}

for (const [name, { staticLabels }] of alertsByName) {
  if (!documentedAlerts.has(name)) {
    failures.push(`deploy/README.md does not document alert \`${name}\` from alerts.yml`);
    continue;
  }

  const documentedSeverity = documentedAlerts.get(name);
  const configuredSeverity = staticLabels.get('severity');
  if (documentedSeverity && documentedSeverity !== configuredSeverity) {
    failures.push(
      `deploy/README.md: alert \`${name}\` says severity \`${documentedSeverity}\`, but ` +
        `alerts.yml labels it \`${configuredSeverity ?? 'none'}\``,
    );
  }
}

for (const name of documentedAlerts.keys()) {
  if (!alertsByName.has(name)) {
    failures.push(`deploy/README.md documents alert \`${name}\`, which alerts.yml does not define`);
  }
}

// ---------------------------------------------------------------------------

if (failures.length > 0) {
  console.error('Observability contract failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `Observability contract ok (${exported.size} metrics exported, ` +
    `${alertReferences} alert + ${dashboardReferences} dashboard references, ` +
    `${documented} metric rows + ${documentedAlerts.size} alert rules + ` +
    `${documentedPanels.size} dashboard panels documented, ` +
    `ForgeKeep ${composeHostPort}:${composeContainerPort})`,
);
