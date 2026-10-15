#!/usr/bin/env node

// Boots the Grafana image the observability stack pins, with the repository's
// provisioning mounted the way compose mounts it, and asks that Grafana whether
// it can show every dashboard we ship.
//
// Why this exists: the `observability-config` job checked Prometheus and
// Alertmanager with their own tools, and Grafana with `JSON.parse`. Moving the
// stack from Grafana 11 to 13 (943ca8ca) turned all six graph panels of the
// overview dashboard into "Panel plugin not found" — Grafana 12 removed the
// Angular `graph` panel — and that gate would have stayed green either way: a
// parser does not know what a panel type is, and Grafana provisions such a
// dashboard without logging anything. Only the image can say which plugins it
// has, so the image is asked.
//
// What it asserts, all against the running pinned image:
//   - it becomes healthy, and provisioning logs no error for dashboards or
//     data sources;
//   - every dashboard file is listed by /api/search under its own uid;
//   - every panel type, and every data source a panel, target or variable
//     names, resolves to a plugin and a provisioned data source this Grafana
//     has (`scripts/lib/grafana-dashboards.mjs` does the judging).
//
// Usage: GRAFANA_IMAGE=<image> node scripts/check-grafana-dashboards.mjs [repository-root]
// The image comes from `scripts/observability-compose-contract-check.mjs`, never
// from a second pin: a duplicate would validate with a Grafana the stack does not
// run the moment somebody bumps it.
//
// The container publishes on a random loopback port and is removed on every
// exit path, so two gates running at once on one host do not collide.

import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { dashboardProblems } from './lib/grafana-dashboards.mjs';
import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));

if (process.argv.length > 3) {
  console.error('Usage: GRAFANA_IMAGE=<image> node scripts/check-grafana-dashboards.mjs [repository-root]');
  process.exit(1);
}

const root = resolve(process.argv[2] ?? join(scriptsDir, '..'));
const image = process.env.GRAFANA_IMAGE ?? '';
if (image.trim() === '') {
  console.error('GRAFANA_IMAGE is empty — pass the image observability-compose-contract-check.mjs printed for grafana.');
  process.exit(1);
}

const provisioning = join(root, 'deploy', 'grafana', 'provisioning');
const dashboardsDir = join(root, 'deploy', 'grafana', 'dashboards');
for (const dir of [provisioning, dashboardsDir]) {
  if (!existsSync(dir)) {
    console.error(`${dir} is missing — there is nothing to provision.`);
    process.exit(1);
  }
}

const dashboards = [];
for (const name of readdirSync(dashboardsDir).filter((entry) => entry.endsWith('.json')).sort()) {
  const file = `deploy/grafana/dashboards/${name}`;
  try {
    dashboards.push({ file, dashboard: JSON.parse(readFileSync(join(dashboardsDir, name), 'utf8')) });
  } catch (error) {
    console.error(`❌ ${file} is not valid JSON — ${error.message}`);
    process.exit(1);
  }
}
if (dashboards.length === 0) {
  console.error('deploy/grafana/dashboards/ no longer holds a *.json.');
  process.exit(1);
}

// The settings compose gives Grafana, so the instance judged here is configured
// like the one that serves the dashboards. Entries compose interpolates from
// the operator's .env (the admin login) are this check's own.
function composeEnvironment() {
  const { parser, missing } = selectYamlParser();
  if (!parser) {
    console.error(`No YAML parser available (tried ${missing.join(', ')}) — cannot read compose's Grafana settings.`);
    process.exit(1);
  }
  const parsed = parseYamlFile(parser, join(root, 'deploy', 'docker-compose.observability.yml'));
  if (!parsed.ok) {
    console.error(`deploy/docker-compose.observability.yml could not be read (${parsed.kind}).`);
    process.exit(1);
  }
  const environment = parsed.document?.services?.grafana?.environment;
  const entries = Array.isArray(environment)
    ? environment.map(String)
    : Object.entries(environment ?? {}).map(([name, value]) => `${name}=${value ?? ''}`);
  return entries.filter((entry) => entry.includes('=') && !entry.includes('$'));
}

function docker(args) {
  return spawnSync('docker', args, { encoding: 'utf8' });
}

const container = `plombir-git-grafana-check-${randomBytes(6).toString('hex')}`;
const password = randomBytes(16).toString('hex');
let started = false;

function removeContainer() {
  if (started) {
    docker(['rm', '-f', container]);
    started = false;
  }
}
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    removeContainer();
    process.exit(1);
  });
}

// `withLogs` is for the container misbehaving; a judged problem is already
// specific and Grafana's log would only bury it.
function fail(message, { withLogs = true } = {}) {
  console.error(`❌ ${message}`);
  if (started && withLogs) {
    const logs = docker(['logs', '--tail', '40', container]);
    console.error(`--- last Grafana log lines ---\n${logs.stdout}${logs.stderr}`);
  }
  removeContainer();
  process.exit(1);
}

// The same hardening compose applies (read-only root, no capabilities), so a
// Grafana that cannot start under it fails here and not on the server. The data
// volume compose names is a tmpfs here: nothing has to outlive the check. No
// `--rm`: Grafana exits on a data source it cannot provision, and its log is
// the only place that says why — `removeContainer` deletes it afterwards.
const run = docker([
  'run', '-d', '--name', container,
  '--read-only', '--tmpfs', '/tmp', '--tmpfs', '/var/lib/grafana:mode=1777',
  '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true',
  '-p', '127.0.0.1::3000',
  ...composeEnvironment().flatMap((entry) => ['-e', entry]),
  '-e', 'GF_SECURITY_ADMIN_USER=admin',
  '-e', `GF_SECURITY_ADMIN_PASSWORD=${password}`,
  '-v', `${provisioning}:/etc/grafana/provisioning:ro`,
  '-v', `${dashboardsDir}:/var/lib/grafana/dashboards:ro`,
  image,
]);
if (run.status !== 0) {
  console.error(`❌ docker run ${image} failed (exit ${run.status ?? run.error?.message})\n${run.stderr}`);
  process.exit(1);
}
started = true;

const port = docker(['port', container, '3000/tcp']).stdout.split('\n').find((line) => /^127\.0\.0\.1:\d+$/.test(line.trim()));
if (!port) fail(`docker port reported no loopback port for ${container}.`);
const base = `http://${port.trim()}`;
const auth = `Basic ${Buffer.from(`admin:${password}`).toString('base64')}`;

async function api(path) {
  const response = await fetch(`${base}${path}`, { headers: { Authorization: auth } });
  if (!response.ok) throw new Error(`GET ${path} answered ${response.status}`);
  return response.json();
}

// Polls `probe` until it returns something truthy. On the deadline it returns
// null with the last reason, and the caller decides whether that is fatal.
async function until(deadlineMs, probe) {
  const deadline = Date.now() + deadlineMs;
  let last = 'no attempt';
  while (Date.now() < deadline) {
    const state = docker(['inspect', '--format', '{{.State.Running}} {{.State.ExitCode}}', container]);
    if (state.stdout.startsWith('false')) {
      fail(`Grafana exited (code ${state.stdout.trim().split(' ')[1]}) — the provisioning it was given stopped it.`);
    }
    try {
      const value = await probe();
      if (value) return { value, last: null };
      last = 'not yet';
    } catch (error) {
      last = error.message;
    }
    await new Promise((done) => setTimeout(done, 1000));
  }
  return { value: null, last };
}

try {
  const healthy = await until(180_000, async () => {
    const body = await api('/api/health');
    return body.database === 'ok' ? body : null;
  });
  if (!healthy.value) fail(`Grafana did not become healthy within 180s (last: ${healthy.last}).`);
  const health = healthy.value;
  console.log(`grafana ${health.version} is up on ${base}`);

  // Provisioning is asynchronous to the HTTP server: health answers before the
  // dashboard provisioner has read its directory, and a file it rejects is
  // only logged. Judge nothing until it says it is done.
  const logText = () => {
    const logs = docker(['logs', container]);
    return `${logs.stdout}${logs.stderr}`;
  };
  const provisioned = await until(60_000, async () =>
    /logger=provisioning\.dashboard\b.*msg="finished to provision dashboards"/.test(logText()));
  if (!provisioned.value) fail('Grafana never logged "finished to provision dashboards" within 60s.');
  const provisionedUids = new Set((await api('/api/search?type=dash-db')).map((entry) => entry.uid));

  const [panels, sources, datasources] = await Promise.all([
    api('/api/plugins?type=panel'),
    api('/api/plugins?type=datasource'),
    api('/api/datasources'),
  ]);

  const problems = dashboardProblems({
    dashboards,
    panelPlugins: new Set(panels.map((plugin) => plugin.id)),
    datasourcePlugins: new Set(sources.map((plugin) => plugin.id)),
    datasources: datasources.map(({ name, uid, type }) => ({ name, uid, type })),
    provisionedUids,
  });

  // Only the two provisioners this repository feeds. The others (plugins,
  // alerting) log an error for their absent directory on every clean start.
  // A plugin installer at work means this Grafana is fetching code the image
  // pin does not cover, and the plugin list read above was a moving target.
  const installs = [];
  for (const line of logText().split('\n')) {
    if (/level=error/.test(line) && /logger=provisioning\.(dashboard|datasources)/.test(line)) {
      problems.push(`provisioning logged an error: ${line.trim()}`);
    }
    if (/logger=plugin\.(backgroundinstaller|installer)\b/.test(line)) installs.push(line.trim());
  }
  if (installs.length > 0) {
    problems.push(
      `Grafana ran its plugin installer at start (${installs.length} log line(s)), fetching code the image `
        + `pin does not cover — compose must set GF_PLUGINS_PREINSTALL_DISABLED=true:\n     `
        + installs.slice(0, 5).join('\n     '),
    );
  }

  if (problems.length > 0) {
    for (const problem of problems) console.error(`❌ ${problem}`);
    fail(`${problems.length} problem(s) in what Grafana ${health.version} was given.`, { withLogs: false });
  }

  const panelCount = dashboards.reduce((count, { dashboard }) => {
    const walk = (list) => (Array.isArray(list) ? list.reduce((n, panel) => n + 1 + walk(panel?.panels), 0) : 0);
    return count + walk(dashboard.panels);
  }, 0);
  console.log(
    `grafana dashboards: ${dashboards.length} dashboard(s), ${panelCount} panel(s) and their data sources `
      + `resolve on grafana ${health.version}`,
  );
} catch (error) {
  fail(error.message);
}
removeContainer();
