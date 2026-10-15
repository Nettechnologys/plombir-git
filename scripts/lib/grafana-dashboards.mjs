// The judging half of `scripts/check-grafana-dashboards.mjs`: given the
// dashboards the repository provisions and what a running Grafana says it has,
// list every reference a dashboard makes that this Grafana cannot honour.
//
// It is pure — no Docker, no HTTP — so the regression stand can drive it with a
// mutated dashboard. What it compares against is never a list written down here:
// the panel and data source plugins come from the pinned image itself, so a bump
// of the Grafana pin re-decides what is valid without anybody editing this file.
//
// Why a parse is not enough: Grafana provisions a dashboard whose panel type it
// has no plugin for without a word in its log. The panel renders as "Panel
// plugin not found" — which is what all six panels of the overview dashboard
// would have been under Grafana 12+, where the Angular `graph` panel was
// removed, while the JSON parse in CI stayed green.

// `row` is layout, not a plugin, and is the only panel type Grafana resolves
// without one.
const LAYOUT_PANEL_TYPES = new Set(['row']);

// Built-in data source names and uids that never appear in /api/datasources.
const BUILTIN_DATASOURCES = new Set(['-- Grafana --', 'grafana', '-- Mixed --', '-- Dashboard --']);

const VARIABLE = /^\$\{?([A-Za-z0-9_]+)(?::[^}]*)?\}?$/;

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// Every panel, including the ones a collapsed row carries in its own `panels`.
function* walkPanels(panels, path) {
  if (!Array.isArray(panels)) return;
  for (const [index, panel] of panels.entries()) {
    if (!isObject(panel)) continue;
    const here = `${path}[${index}]`;
    yield { panel, where: `${here} (id ${panel.id ?? '?'}, ${JSON.stringify(panel.title ?? '')})` };
    yield* walkPanels(panel.panels, `${here}.panels`);
  }
}

/**
 * @param {object} input
 * @param {{ file: string, dashboard: unknown }[]} input.dashboards  parsed repository files
 * @param {Set<string>} input.panelPlugins       ids from /api/plugins?type=panel
 * @param {Set<string>} input.datasourcePlugins  ids from /api/plugins?type=datasource
 * @param {{ name: string, uid: string, type: string }[]} input.datasources  /api/datasources
 * @param {Set<string>} input.provisionedUids    dashboard uids /api/search returned
 * @returns {string[]} one line per problem; empty means every reference resolves
 */
export function dashboardProblems({
  dashboards,
  panelPlugins,
  datasourcePlugins,
  datasources,
  provisionedUids,
}) {
  const problems = [];
  if (panelPlugins.size === 0) {
    problems.push('Grafana reported no panel plugins — the read broke, not the dashboards.');
    return problems;
  }

  const byName = new Map(datasources.map((source) => [source.name, source]));
  const byUid = new Map(datasources.map((source) => [source.uid, source]));

  // Two files under one uid provision as one dashboard, and Grafana 13 does not
  // log which file lost.
  const filesByUid = new Map();
  for (const { file, dashboard } of dashboards) {
    if (typeof dashboard?.uid !== 'string' || dashboard.uid === '') continue;
    filesByUid.set(dashboard.uid, [...(filesByUid.get(dashboard.uid) ?? []), file]);
  }
  for (const [uid, files] of filesByUid) {
    if (files.length > 1) {
      problems.push(`${files.join(' and ')} share uid ${uid} — Grafana keeps one of them and says nothing.`);
    }
  }

  for (const { file, dashboard } of dashboards) {
    if (!isObject(dashboard)) {
      problems.push(`${file} is not a JSON object.`);
      continue;
    }
    if (typeof dashboard.uid !== 'string' || dashboard.uid === '') {
      problems.push(`${file} has no \`uid\` — Grafana invents one per instance, so links to it break.`);
    } else if (!provisionedUids.has(dashboard.uid)) {
      problems.push(`${file} (uid ${dashboard.uid}) was not provisioned — Grafana's search does not list it.`);
    }
    // Pre-5.0 layout. Grafana still migrates it in the browser, but nothing here
    // would walk its panels, so it is refused rather than checked vacuously.
    if (Array.isArray(dashboard.rows) && dashboard.rows.length > 0) {
      problems.push(`${file} uses the legacy \`rows\` layout — move its panels to the top-level \`panels\` list.`);
    }

    const variables = new Map();
    for (const variable of dashboard.templating?.list ?? []) {
      if (isObject(variable) && typeof variable.name === 'string') variables.set(variable.name, variable);
    }

    const checkReference = (reference, where) => {
      if (reference === undefined || reference === null) return;
      if (typeof reference === 'string') {
        const variable = VARIABLE.exec(reference);
        if (variable) {
          if (variables.get(variable[1])?.type !== 'datasource') {
            problems.push(`${file}: ${where} uses data source ${reference}, and no data source variable of that name exists.`);
          }
          return;
        }
        if (!BUILTIN_DATASOURCES.has(reference) && !byName.has(reference) && !byUid.has(reference)) {
          problems.push(`${file}: ${where} names data source ${JSON.stringify(reference)}, which Grafana does not provision.`);
        }
        return;
      }
      if (!isObject(reference)) {
        problems.push(`${file}: ${where} has a data source reference that is neither a string nor an object.`);
        return;
      }
      const { type, uid } = reference;
      if (typeof uid === 'string' && VARIABLE.test(uid)) {
        checkReference(uid, where);
        return;
      }
      if (typeof uid === 'string' && !BUILTIN_DATASOURCES.has(uid)) {
        const source = byUid.get(uid);
        if (!source) {
          problems.push(`${file}: ${where} points at data source uid ${uid}, which Grafana does not provision.`);
          return;
        }
        if (typeof type === 'string' && type !== source.type) {
          problems.push(`${file}: ${where} expects a ${type} data source, and uid ${uid} is ${source.type}.`);
        }
        return;
      }
      if (typeof type === 'string' && !BUILTIN_DATASOURCES.has(type) && type !== 'datasource'
          && !datasourcePlugins.has(type)) {
        problems.push(`${file}: ${where} wants a ${type} data source, and this Grafana has no such plugin.`);
      }
    };

    for (const [name, variable] of variables) {
      const where = `templating variable \`${name}\``;
      if (variable.type === 'datasource') {
        const pluginId = variable.query;
        if (typeof pluginId !== 'string' || !datasourcePlugins.has(pluginId)) {
          problems.push(`${file}: ${where} lists ${JSON.stringify(pluginId)} data sources, and this Grafana has no such plugin.`);
        } else if (!datasources.some((source) => source.type === pluginId)) {
          problems.push(`${file}: ${where} lists ${pluginId} data sources, and none is provisioned — it would be empty.`);
        }
      } else {
        checkReference(variable.datasource, where);
      }
    }

    for (const { panel, where } of walkPanels(dashboard.panels, 'panels')) {
      if (typeof panel.type !== 'string' || panel.type === '') {
        problems.push(`${file}: ${where} has no panel type.`);
      } else if (!LAYOUT_PANEL_TYPES.has(panel.type) && !panelPlugins.has(panel.type)) {
        problems.push(
          `${file}: ${where} is a \`${panel.type}\` panel, and this Grafana has no such panel plugin — `
            + 'it would render "Panel plugin not found".',
        );
      }
      checkReference(panel.datasource, where);
      for (const [index, target] of (Array.isArray(panel.targets) ? panel.targets : []).entries()) {
        if (isObject(target)) checkReference(target.datasource, `${where} targets[${index}]`);
      }
    }
  }
  return problems;
}
