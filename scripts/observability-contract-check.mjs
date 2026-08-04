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

import { stripRustComments } from './lib/rust-source.mjs';

const root = process.cwd();
const metricsPath = path.join(root, 'crates/rg-http/src/metrics.rs');
const alertsPath = path.join(root, 'deploy/prometheus/alerts.yml');
const promPath = path.join(root, 'deploy/prometheus/prometheus.yml');
const dashboardsDir = path.join(root, 'deploy/grafana/dashboards');
const readmePath = path.join(root, 'deploy/README.md');

const failures = [];

// Floors: the exporter and its consumers as they stand today. These only ever
// move up. A parse that silently stops matching drops below them and reddens,
// instead of reporting "everything referenced exists" over an empty set.
const MIN_METRICS = 20;
const MIN_ALERT_REFERENCES = 10;
const MIN_DASHBOARD_REFERENCES = 10;

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

for (const statement of stripRustComments(readFileSync(metricsPath, 'utf8')).split(';')) {
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

// Target labels attached by Prometheus at scrape time, plus the two it always
// adds. Read out of prometheus.yml rather than hardcoded, so a target label
// added there is usable in a rule the same day.
const prometheusYml = readFileSync(promPath, 'utf8');
const targetLabels = new Set(['job', 'instance', 'alertname', 'severity']);
for (const block of prometheusYml.matchAll(/^\s*labels:\s*$([\s\S]*?)(?=^\s*(?:-|\w))/gm)) {
  for (const [, label] of block[1].matchAll(/^\s+([a-z_][a-z0-9_]*):/gm)) targetLabels.add(label);
}
for (const [, label] of (prometheusYml.match(/external_labels:[\s\S]*?(?=\n\w)/)?.[0] ?? '')
  .matchAll(/^\s+([a-z_][a-z0-9_]*):/gm)) {
  targetLabels.add(label);
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

const alerts = readFileSync(alertsPath, 'utf8');
let alertReferences = 0;

// `expr:` is either inline or a `|` block; both end at the next key at or above
// the rule's indentation.
// `(?![\s\S])` is JavaScript's end-of-input assertion. `\Z` is a literal
// `Z` in JavaScript, and `$` would also match every line ending under `m`.
const ALERT_RULE = /^([ \t]*)- alert:\s*(\S+)([\s\S]*?)(?=^\1- alert:|^[ \t]{0,4}- name:|(?![\s\S]))/gm;
const declaredAlertRules = [...alerts.matchAll(/^[ \t]*- alert:\s*\S+/gm)];
const rules = [...alerts.matchAll(ALERT_RULE)];
if (rules.length === 0) {
  failures.push(`No alert rules parsed out of ${path.relative(root, alertsPath)}`);
} else if (rules.length !== declaredAlertRules.length) {
  failures.push(
    `Parsed ${rules.length} of ${declaredAlertRules.length} alert rules from ${path.relative(root, alertsPath)}`,
  );
}

for (const [, , name, body] of rules) {
  const expr = body.match(/\bexpr:\s*(\|[-+]?\s*\n([\s\S]*?)(?=^\s*(?:for|labels|annotations):)|(.+))/m);
  if (!expr) {
    failures.push(`alerts.yml: rule ${name} has no readable expr`);
    continue;
  }
  const expression = expr[2] ?? expr[3];
  alertReferences += checkExpression(expression, `alerts.yml: ${name}`);

  // An annotation interpolating `$labels.x` is a promise that x is on the
  // alert. A raw vector retains its labels, `by` retains only its list, and
  // `without` retains everything except its list. We cannot know labels from
  // a foreign exporter, so do not make a false claim about an all-foreign rule.
  const references = metricReferences(expression);
  const allMetricsForeign = references.length > 0 && references.every(({ metric }) => FOREIGN_METRIC.test(metric));
  for (const [, label] of body.matchAll(/\$labels\.([a-z_][a-z0-9_]*)/g)) {
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
// 4. Grafana dashboards.
// ---------------------------------------------------------------------------

const dashboards = readdirSync(dashboardsDir).filter((name) => name.endsWith('.json')).sort();
if (dashboards.length === 0) {
  failures.push(`No dashboards found in ${path.relative(root, dashboardsDir)}`);
}

let dashboardReferences = 0;

for (const file of dashboards) {
  let dashboard;
  try {
    dashboard = JSON.parse(readFileSync(path.join(dashboardsDir, file), 'utf8'));
  } catch (error) {
    failures.push(`${file}: not valid JSON — ${error.message}`);
    continue;
  }

  const variables = new Set((dashboard.templating?.list ?? []).map((v) => v.name));

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

  for (const panel of dashboard.panels ?? []) {
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
// 5. The metric tables in deploy/README.md.
// ---------------------------------------------------------------------------

// The deployment guide is the third consumer of this contract: an operator
// writes their own queries from its Labels column, so a stale row there is the
// same drift arriving one step later.
const readme = readFileSync(readmePath, 'utf8');
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

// ---------------------------------------------------------------------------

if (failures.length > 0) {
  console.error('Observability contract failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `Observability contract ok (${exported.size} metrics exported, ` +
    `${alertReferences} alert + ${dashboardReferences} dashboard references, ` +
    `${documented} documented)`,
);
