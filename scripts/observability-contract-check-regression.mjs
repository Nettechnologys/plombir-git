#!/usr/bin/env node

// Regression fixtures for the label-preservation and inhibition parts of the
// observability contract. Run the real checker in a copied repository shape so
// the fixture cannot accidentally weaken production alerts or bypass its parse
// floors.

import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/observability-contract-check.mjs';

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-observability-contract-'));
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  mkdirSync(join(fixture, 'crates', 'rg-http', 'src'), { recursive: true });
  cpSync(join(root, check), join(fixture, check));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  cpSync(join(root, 'crates', 'rg-http', 'src', 'metrics.rs'), join(fixture, 'crates', 'rg-http', 'src', 'metrics.rs'));
  cpSync(join(root, 'deploy'), join(fixture, 'deploy'), { recursive: true });
  return fixture;
}

function runCheck(name, fixture, expectedStatus, expectedOutput = '') {
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
}

function documentFixtureAlert(fixture, rule) {
  const name = rule.match(/^[ \t]*- alert:\s*(\S+)/m)?.[1];
  if (!name) throw new Error('fixture alert rule has no readable name');

  const readmePath = join(fixture, 'deploy', 'README.md');
  const readme = readFileSync(readmePath, 'utf8');
  const nextSection = '\n## 🔭 Distributed Tracing (OpenTelemetry)';
  if (!readme.includes(nextSection)) throw new Error('fixture anchor after Alert Rules disappeared');
  writeFileSync(
    readmePath,
    readme.replace(nextSection, `\n- **${name}**: regression fixture (warning)\n${nextSection}`),
  );
}

function runFixture(
  name,
  rule,
  expectedStatus,
  expectedOutput = '',
  { appendLast = false, document = true, mutateFixture = null } = {},
) {
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
    if (document) documentFixtureAlert(fixture, rule);
    if (mutateFixture) mutateFixture(fixture);

    runCheck(name, fixture, expectedStatus, expectedOutput);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

function runMutationFixture(name, mutate, expectedOutput, expectedStatus = 1) {
  const fixture = fixtureRoot();
  try {
    mutate(fixture);
    runCheck(name, fixture, expectedStatus, expectedOutput);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

function replaceRequired(file, before, after) {
  const text = readFileSync(file, 'utf8');
  if (!text.includes(before)) throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  writeFileSync(file, text.replace(before, after));
}

function mutateMainDashboard(fixture, mutate) {
  const dashboardPath = join(fixture, 'deploy', 'grafana', 'dashboards', 'plombir-git-main.json');
  const dashboard = JSON.parse(readFileSync(dashboardPath, 'utf8'));
  mutate(dashboard);
  writeFileSync(dashboardPath, `${JSON.stringify(dashboard, null, 2)}\n`);
}

function runHelperFixture(
  name,
  mutate,
  expectedStatus,
  expectedOutput,
  expectedCurl = '',
  {
    plombirGitPorts = [['8080', '8080'], ['2222', '2222']],
    sidecarPorts = [],
  } = {},
) {
  const fixture = fixtureRoot();
  try {
    if (mutate) mutate(fixture);

    const bin = join(fixture, 'fake-bin');
    const curlLog = join(fixture, 'curl.log');
    const composeConfig = join(fixture, 'compose-config.yml');
    mkdirSync(bin);
    const renderService = (name, ports) => {
      const renderedPorts = ports.map(([published, target]) => `      - mode: ingress
        protocol: tcp
        published: "${published}"
        target: ${target}`).join('\n');
      return `  ${name}:
    ports:
${renderedPorts}
`;
    };
    writeFileSync(
      composeConfig,
      `services:
${renderService('plombir-git', plombirGitPorts)}${sidecarPorts.length > 0 ? renderService('sidecar', sidecarPorts) : ''}`,
    );
    writeFileSync(
      join(bin, 'docker'),
      `#!/usr/bin/env bash
if [ "$1" = "compose" ] && [ "$4" = "config" ]; then
    cat "$FAKE_COMPOSE_CONFIG"
fi
exit 0
`,
      { mode: 0o755 },
    );
    writeFileSync(join(bin, 'sleep'), '#!/usr/bin/env bash\nexit 0\n', { mode: 0o755 });
    writeFileSync(
      join(bin, 'curl'),
      '#!/usr/bin/env bash\nprintf \'%s\\n\' "$*" >>"$CURL_LOG"\nprintf \'200\'\n',
      { mode: 0o755 },
    );

    const result = spawnSync('bash', [join(fixture, 'deploy', 'start-observability.sh')], {
      cwd: fixture,
      encoding: 'utf8',
      env: {
        ...process.env,
        FAKE_COMPOSE_CONFIG: composeConfig,
        CURL_LOG: curlLog,
        PATH: `${bin}:${process.env.PATH ?? ''}`,
      },
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const curls = existsSync(curlLog) ? readFileSync(curlLog, 'utf8') : '';
    if (
      result.status !== expectedStatus ||
      (expectedOutput && !output.includes(expectedOutput)) ||
      (expectedCurl && !curls.includes(expectedCurl))
    ) {
      throw new Error(
        `${name}: expected exit ${expectedStatus}, output ${JSON.stringify(expectedOutput)}, ` +
          `curl ${JSON.stringify(expectedCurl)}; got exit ${result.status}\n${output}\nCURLS:\n${curls}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// ── Compose / Prometheus / helper / README endpoint ───────────────────────

runMutationFixture(
  'a changed compose host port without updated operator URLs fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'docker-compose.yml'),
    '- "8080:8080"   # HTTP',
    '- "8181:8080"   # HTTP',
  ),
  'Quick Start Access URL disagrees with the compose published HTTP port: 8080 != 8181',
);

runMutationFixture(
  'a changed compose container port without an updated scrape target fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'docker-compose.yml'),
    '- "8080:8080"   # HTTP',
    '- "8080:8181"   # HTTP',
  ),
  'Prometheus Plombir Git target disagrees with the compose container HTTP port: 8080 != 8181',
);

runMutationFixture(
  'a changed Prometheus target without an updated compose fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
    "targets: ['plombir-git:8080']",
    "targets: ['plombir-git:8181']",
  ),
  'Prometheus Plombir Git target disagrees with the compose container HTTP port: 8181 != 8080',
);

runMutationFixture(
  'a quoted following scrape job cannot lend its target to the Plombir Git job',
  (fixture) => {
    const prometheus = join(fixture, 'deploy', 'prometheus', 'prometheus.yml');
    replaceRequired(prometheus, "targets: ['plombir-git:8080']", "targets: ['sidecar:9999']");
    replaceRequired(prometheus, "  - job_name: 'prometheus'", '  - "job_name": "prometheus"');
    replaceRequired(prometheus, "targets: ['localhost:9090']", "targets: ['plombir-git:8080']");
  },
  'must contain exactly one plombir-git:PORT target in the plombir-git job; parsed 0',
);

runMutationFixture(
  'a quoted job_name key in the Plombir Git scrape job keeps the same contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
    "  - job_name: 'plombir-git'",
    '  - "job_name": "plombir-git"',
  ),
  'Observability contract ok',
  0,
);

runMutationFixture(
  'a second Plombir Git scrape job fails instead of choosing one implicitly',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
    "  # Prometheus itself\n  - job_name: 'prometheus'",
    `  # Duplicate Plombir Git job
  - job_name: 'plombir-git'
    static_configs:
      - targets: ['plombir-git:8080']

  # Prometheus itself
  - job_name: 'prometheus'`,
  ),
  'must contain exactly one scrape_configs job named "plombir-git"; parsed 2',
);

runMutationFixture(
  'multiple Plombir Git targets fail instead of choosing one implicitly',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
    "targets: ['plombir-git:8080']",
    "targets: ['plombir-git:8080', 'plombir-git:8181']",
  ),
  'must contain exactly one plombir-git:PORT target in the plombir-git job; parsed 2',
);

runMutationFixture(
  'a quoted sidecar cannot lend its HTTP mapping to the Plombir Git service',
  (fixture) => {
    const compose = join(fixture, 'deploy', 'docker-compose.yml');
    replaceRequired(compose, '      - "8080:8080"   # HTTP\n', '');
    replaceRequired(
      compose,
      '\nvolumes:\n',
      `\n  "sidecar":
    image: busybox:1.36
    ports:
      - "8080:8080"   # HTTP

volumes:
`,
    );
  },
  'deploy/docker-compose.yml # HTTP mapping "8080:8080" must identify exactly one services.plombir-git.ports entry',
);

runMutationFixture(
  'the HTTP marker cannot move to a quoted sidecar with the same port value',
  (fixture) => {
    const compose = join(fixture, 'deploy', 'docker-compose.yml');
    replaceRequired(compose, '# HTTP', '# WEB');
    replaceRequired(
      compose,
      '\nvolumes:\n',
      `\n  "sidecar":
    image: busybox:1.36
    ports:
      - "8080:8080"   # HTTP

volumes:
`,
    );
  },
  'deploy/docker-compose.yml # HTTP mapping "8080:8080" must identify exactly one services.plombir-git.ports entry',
);

runMutationFixture(
  'an unreadable compose HTTP mapping fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'docker-compose.yml'),
    '# HTTP',
    '# WEB',
  ),
  'must contain exactly one numeric "HOST:CONTAINER" Plombir Git port mapping marked "# HTTP"; parsed 0',
);

runMutationFixture(
  'a hardcoded helper app endpoint fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'start-observability.sh'),
    'http://localhost:${PLOMBIR_GIT_HOST_PORT}/health',
    'http://localhost:9999/health',
  ),
  'start-observability.sh hardcodes Plombir Git app endpoint(s): localhost:9999/health',
);

runMutationFixture(
  'a stale architecture metrics port fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'README.md'),
    '│  :8080/metrics',
    '│  :8181/metrics',
  ),
  'deploy/README.md architecture disagrees with the compose container HTTP port: 8181 != 8080',
);

runHelperFixture(
  'the helper checks and prints the shipped compose host port',
  null,
  0,
  'Plombir Git:      http://localhost:8080/metrics',
  'http://localhost:8080/health',
);

runHelperFixture(
  'the helper follows a changed compose host port without a second literal',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'docker-compose.yml'),
    '- "8080:8080"   # HTTP',
    '- "8181:8080"   # HTTP',
  ),
  0,
  'Plombir Git:      http://localhost:8181/metrics',
  'http://localhost:8181/health',
  { plombirGitPorts: [['8181', '8080'], ['2222', '2222']] },
);

runHelperFixture(
  'the helper rejects an unreadable compose HTTP mapping',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'docker-compose.yml'),
    '# HTTP',
    '# WEB',
  ),
  1,
  'expected exactly one numeric HOST:CONTAINER Plombir Git port mapping marked # HTTP',
);

runHelperFixture(
  'the helper rejects an HTTP mapping owned by a quoted sidecar service',
  (fixture) => {
    const compose = join(fixture, 'deploy', 'docker-compose.yml');
    replaceRequired(compose, '      - "8080:8080"   # HTTP\n', '');
    replaceRequired(
      compose,
      '\nvolumes:\n',
      `\n  "sidecar":
    image: busybox:1.36
    ports:
      - "8181:8080"   # HTTP

volumes:
`,
    );
  },
  1,
  'does not identify one unique services.plombir-git.ports entry',
  '',
  {
    plombirGitPorts: [['2222', '2222']],
    sidecarPorts: [['8181', '8080']],
  },
);

runHelperFixture(
  'the helper rejects a sidecar marker even when Plombir Git exposes the same ports',
  (fixture) => {
    const compose = join(fixture, 'deploy', 'docker-compose.yml');
    replaceRequired(compose, '# HTTP', '# WEB');
    replaceRequired(
      compose,
      '\nvolumes:\n',
      `\n  "sidecar":
    image: busybox:1.36
    ports:
      - "8080:8080"   # HTTP

volumes:
`,
    );
  },
  1,
  'does not identify one unique services.plombir-git.ports entry',
  '',
  { sidecarPorts: [['8080', '8080']] },
);

// ── Prometheus alert-rule structure ────────────────────────────────────────

runMutationFixture(
  'a quoted alert key keeps the same contract as its unquoted spelling',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'alerts.yml'),
    '      - alert: HighInFlightRequests',
    '      - "alert": HighInFlightRequests',
  ),
  'Observability contract ok',
  0,
);

runMutationFixture(
  'quoted alert rule fields keep the same structured contract',
  (fixture) => {
    const alerts = join(fixture, 'deploy', 'prometheus', 'alerts.yml');
    replaceRequired(alerts, '      - alert: HighErrorRate', '      - "alert": HighErrorRate');
    replaceRequired(alerts, '        expr: |', '        "expr": |');
    replaceRequired(alerts, '        labels:', '        "labels":');
    replaceRequired(alerts, '        annotations:', '        "annotations":');
  },
  'Observability contract ok',
  0,
);

runMutationFixture(
  'an alert name with the wrong YAML type fails at its structural path',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'alerts.yml'),
    '      - alert: HighErrorRate',
    '      - alert: [HighErrorRate]',
  ),
  'deploy/prometheus/alerts.yml: groups[0].rules[0].alert must be a non-empty string',
);

runMutationFixture(
  'a following rule cannot lend its expression to an incomplete alert',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'alerts.yml'),
    '        expr: |',
    '        expression: |',
  ),
  'deploy/prometheus/alerts.yml: groups[0].rules[0].expr must be a non-empty string',
);

runMutationFixture(
  'a non-mapping alert labels value fails at its structural path',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'alerts.yml'),
    `        labels:
          severity: critical
          service: plombir-git`,
    '        labels: critical',
  ),
  'deploy/prometheus/alerts.yml: groups[0].rules[0].labels must be a mapping',
);

runMutationFixture(
  'a non-mapping alert annotations value fails at its structural path',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'prometheus', 'alerts.yml'),
    `        annotations:
          summary: "High HTTP 5xx error rate on {{ $labels.route }}"
          description: "{{ $labels.route }} has error rate {{ $value | humanizePercentage }} for 5+ minutes."`,
    '        annotations: unreadable',
  ),
  'deploy/prometheus/alerts.yml: groups[0].rules[0].annotations must be a mapping',
);

runFixture(
  'a selector can use a label from the Plombir Git scrape target',
  `      - alert: PlombirGitTargetLabel
        expr: http_requests_in_flight{component="api"} > 200
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Plombir Git API target"`,
  0,
);

runFixture(
  'a label on a neighboring scrape job cannot satisfy a Plombir Git selector',
  `      - alert: NeighborTargetLabel
        expr: http_requests_in_flight{neighbor_only="yes"} > 200
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Neighbor-only target label"`,
  1,
  'asks `http_requests_in_flight` for label `neighbor_only`, which the exporter does not emit',
  {
    mutateFixture: (fixture) => replaceRequired(
      join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
      "          service: 'prometheus'",
      "          service: 'prometheus'\n          neighbor_only: 'yes'",
    ),
  },
);

runFixture(
  'quoted Plombir Git target label keys and values keep the same contract',
  `      - alert: QuotedPlombirGitTargetLabel
        expr: http_requests_in_flight{component="api"} > 200
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Quoted Plombir Git API target"`,
  0,
  '',
  {
    mutateFixture: (fixture) => replaceRequired(
      join(fixture, 'deploy', 'prometheus', 'prometheus.yml'),
      "          component: 'api'",
      '          "component": "api"',
    ),
  },
);

runFixture(
  'a global external label is not a locally queryable Plombir Git target label',
  `      - alert: ExternalLabelIsNotTargetLabel
        expr: http_requests_in_flight{cluster="plombir-git"} > 200
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "External label is not a selector label"`,
  1,
  'asks `http_requests_in_flight` for label `cluster`, which the exporter does not emit',
);

runFixture(
  'without retains labels not named by the modifier',
  `      - alert: WithoutKeepsRoute
        expr: sum without (status) (http_requests_total)
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Route {{ $labels.route }} remains available"`,
  0,
);

runFixture(
  'all-foreign aggregations do not claim exporter label knowledge',
  `      - alert: ForeignMetricLabelsAreUnknown
        expr: sum by (instance) (node_filesystem_avail_bytes)
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Filesystem {{ $labels.mountpoint }}"`,
  0,
);

runFixture(
  'by omitting an interpolated label fails the contract',
  `      - alert: GroupingDropsRoute
        expr: sum by (instance) (http_requests_total)
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Route {{ $labels.route }} disappeared"`,
  1,
  'alerts.yml: GroupingDropsRoute interpolates {{ $labels.route }}, but its expr does not retain `route`',
);

runFixture(
  'last alert rule with a lost label fails the contract',
  `      - alert: FinalGroupingDropsRoute
        expr: sum by (instance) (http_requests_total)
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Route {{ $labels.route }} disappeared"`,
  1,
  'alerts.yml: FinalGroupingDropsRoute interpolates {{ $labels.route }}, but its expr does not retain `route`',
  { appendLast: true },
);

// ── README dashboard panel inventory ───────────────────────────────────────

runMutationFixture(
  'a dashboard panel added without documentation fails the contract',
  (fixture) => mutateMainDashboard(fixture, (dashboard) => {
    dashboard.panels.push({ id: 99, title: '🧪 Undocumented Panel', targets: [] });
  }),
  'deploy/README.md does not document Grafana panel `99` "🧪 Undocumented Panel"',
);

runMutationFixture(
  'a dashboard panel removed without removing its documentation fails the contract',
  (fixture) => mutateMainDashboard(fixture, (dashboard) => {
    dashboard.panels = dashboard.panels.filter(({ id }) => id !== 13);
  }),
  'deploy/README.md documents Grafana panel `13` "🖥️ CPU Usage", which plombir-git-main does not contain',
);

runMutationFixture(
  'a dashboard panel title typo in the README fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'README.md'),
    '**📊 Request Rate (QPS)**',
    '**📊 Request Rte (QPS)**',
  ),
  'Grafana panel `1` is titled "📊 Request Rate (QPS)", but the guide says "📊 Request Rte (QPS)"',
);

runMutationFixture(
  'a duplicate dashboard panel id in the README fails the contract',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'README.md'),
    'Grafana panel `2`:',
    'Grafana panel `1`:',
  ),
  'deploy/README.md documents Grafana panel id 1 more than once',
);

runMutationFixture(
  'a duplicate dashboard panel id in Grafana JSON fails the contract',
  (fixture) => mutateMainDashboard(fixture, (dashboard) => {
    dashboard.panels[1].id = dashboard.panels[0].id;
  }),
  'plombir-git-main.json: plombir-git-main defines panel id 1 more than once',
);

runMutationFixture(
  'an unreadable Dashboard Panels section fails closed',
  (fixture) => replaceRequired(
    join(fixture, 'deploy', 'README.md'),
    '## 📋 Dashboard Panels',
    '## 📋 Dashboard Catalog',
  ),
  'deploy/README.md has no "Dashboard Panels" section',
);

// ── README alert inventory ─────────────────────────────────────────────────

runFixture(
  'an alert added without documentation fails the contract',
  `      - alert: UndocumentedAlert
        expr: http_requests_in_flight > 200
        labels:
          severity: warning
          service: plombir-git
        annotations:
          summary: "Regression fixture"`,
  1,
  'deploy/README.md does not document alert `UndocumentedAlert` from alerts.yml',
  { document: false },
);

runMutationFixture(
  'an alert removed without removing its documentation fails the contract',
  (fixture) => {
    const alertsPath = join(fixture, 'deploy', 'prometheus', 'alerts.yml');
    const alerts = readFileSync(alertsPath, 'utf8');
    const lastRule = /\n      - alert: BackupRunsFailing[\s\S]*$/;
    if (!lastRule.test(alerts)) throw new Error('fixture anchor for BackupRunsFailing disappeared');
    writeFileSync(alertsPath, alerts.replace(lastRule, '\n'));
  },
  'deploy/README.md documents alert `BackupRunsFailing`, which alerts.yml does not define',
);

runMutationFixture(
  'an alert name typo in the README fails the contract',
  (fixture) => {
    const readmePath = join(fixture, 'deploy', 'README.md');
    const readme = readFileSync(readmePath, 'utf8');
    const anchor = '**PlombirGitDown**';
    if (!readme.includes(anchor)) throw new Error('fixture anchor for PlombirGitDown disappeared');
    writeFileSync(readmePath, readme.replace(anchor, '**PlombirGitDwn**'));
  },
  'deploy/README.md does not document alert `PlombirGitDown` from alerts.yml',
);

runMutationFixture(
  'an alert severity typo in the README fails the contract',
  (fixture) => {
    const readmePath = join(fixture, 'deploy', 'README.md');
    const readme = readFileSync(readmePath, 'utf8');
    const before = '- **HighMemoryUsage**: Memory > 90% for 10+ minutes (warning)';
    if (!readme.includes(before)) throw new Error('fixture anchor for HighMemoryUsage disappeared');
    writeFileSync(readmePath, readme.replace(before, before.replace('(warning)', '(critical)')));
  },
  'alert `HighMemoryUsage` says severity `critical`, but alerts.yml labels it `warning`',
);

runMutationFixture(
  'an unreadable Alert Rules section fails closed',
  (fixture) => {
    const readmePath = join(fixture, 'deploy', 'README.md');
    const readme = readFileSync(readmePath, 'utf8');
    const heading = '## 🔔 Alert Rules';
    if (!readme.includes(heading)) throw new Error('fixture Alert Rules heading disappeared');
    writeFileSync(readmePath, readme.replace(heading, '## 🔔 Alert Catalog'));
  },
  'deploy/README.md has no "Alert Rules" section',
);

// ── Inhibition ─────────────────────────────────────────────────────────────
//
// The same shape one file over: run the real checker against a fixture whose
// `inhibit_rules:` has been edited, so the assertion is exercised rather than
// assumed. The first fixture is the config as it stood before card_2206bf4038e7
// — valid YAML, accepted by `amtool check-config`, and measurably wrong.

function runInhibitFixture(
  name,
  rules,
  expectedStatus,
  expectedOutput = '',
  { wholeDocument = false } = {},
) {
  const fixture = fixtureRoot();
  try {
    const configPath = join(fixture, 'deploy', 'alertmanager', 'alertmanager.yml');
    const config = readFileSync(configPath, 'utf8');
    const anchor = /^inhibit_rules:\s*\n[\s\S]*$/m;
    if (!anchor.test(config)) throw new Error('fixture anchor for inhibit_rules disappeared');
    const replacement = wholeDocument ? rules : `inhibit_rules:\n${rules}`;
    writeFileSync(configPath, config.replace(anchor, `${replacement}\n`));

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

const DOWN_RULE = `  - source_match:
      alertname: 'PlombirGitDown'
    target_match_re:
      service: 'plombir-git'
    equal: ['service']`;

// `alertname` is attached by Prometheus when an alerting rule fires; it is not
// inherited from the expression's input series. DOWN_RULE exercises that fact
// for the foreign `up` metric, while this pair exercises it for Plombir Git's own
// metrics and also proves that the route label still comes from those metrics.
runInhibitFixture(
  'alertname matchers select existing local alerts',
  `${DOWN_RULE}

  - source_match:
      alertname: 'HighErrorRate'
    target_match:
      alertname: 'SlowRequestDuration'
    equal: ['service', 'route']`,
  0,
);

runInhibitFixture(
  'an alertname matcher cannot invent an alert',
  `${DOWN_RULE}

  - source_match:
      alertname: 'NoSuchAlert'
    target_match:
      alertname: 'SlowRequestDuration'
    equal: ['service', 'route']`,
  1,
  'no alert in alerts.yml can match its source',
);

runInhibitFixture(
  'alertname presence does not invent arbitrary metric labels',
  `${DOWN_RULE}

  - source_match:
      alertname: 'HighErrorRate'
    target_match:
      alertname: 'SlowRequestDuration'
    equal: ['service', 'route', 'not_a_real_label']`,
  1,
  'equals on `not_a_real_label`, but HighErrorRate matches its source without carrying that label',
);

runInhibitFixture(
  'equal on a label the source side does not require fails the contract',
  `${DOWN_RULE}

  - source_match:
      severity: 'critical'
    target_match:
      severity: 'warning'
    equal: ['service', 'route']`,
  1,
  'equals on `route`, but HighGitOperationFailure matches its source without carrying that label',
);

runInhibitFixture(
  'a quoted equal key cannot hide an unsafe inhibition rule',
  `${DOWN_RULE}

  - source_match:
      severity: 'critical'
    target_match:
      severity: 'warning'
    "equal": ['service', 'route']`,
  1,
  'equals on `route`, but HighGitOperationFailure matches its source without carrying that label',
);

runInhibitFixture(
  'requiring the label on both sides passes',
  `${DOWN_RULE}

  - source_matchers:
      - 'severity = "critical"'
      - 'route != ""'
    target_matchers:
      - 'severity = "warning"'
      - 'route != ""'
    equal: ['service', 'route']`,
  0,
);

runInhibitFixture(
  'quoted current matcher keys select the same alerts',
  `${DOWN_RULE}

  - "source_matchers":
      - 'severity = "critical"'
      - 'route != ""'
    'target_matchers':
      - 'severity = "warning"'
      - 'route != ""'
    equal: ['service', 'route']`,
  0,
);

runInhibitFixture(
  'quoted legacy matcher keys select the same alerts',
  `${DOWN_RULE}

  - "source_match":
      severity: 'critical'
    "source_match_re":
      route: '.+'
    'target_match':
      severity: 'warning'
    'target_match_re':
      route: '.+'
    equal: ['service', 'route']`,
  0,
);

runInhibitFixture(
  'quoted inhibit_rules and rule keys preserve the safe contract',
  `"inhibit_rules":
  - "source_match":
      alertname: 'PlombirGitDown'
    "target_match_re":
      service: 'plombir-git'
    "equal": ['service']

  - "source_matchers":
      - 'severity = "critical"'
      - 'route != ""'
    "target_matchers":
      - 'severity = "warning"'
      - 'route != ""'
    "equal": ['service', 'route']`,
  0,
  '',
  { wholeDocument: true },
);

runInhibitFixture(
  'a matcher this check cannot read fails instead of passing over it',
  `${DOWN_RULE}

  - source_matchers:
      - 'severity is critical'
    target_matchers:
      - 'severity = "warning"'
    equal: ['service']`,
  1,
  'is not a matcher this check can read',
);

runInhibitFixture(
  'an inhibit list the parser stops understanding fails closed',
  `  - source_match:
      alertname: 'PlombirGitDown'
    equal: ['service']`,
  1,
  'inhibit rule(s) parsed out of alertmanager.yml',
);

runInhibitFixture(
  'a non-list inhibit_rules value fails closed',
  `inhibit_rules:
  source_match:
    alertname: 'PlombirGitDown'`,
  1,
  'deploy/alertmanager/alertmanager.yml.inhibit_rules must be a list',
  { wholeDocument: true },
);

runInhibitFixture(
  'a non-mapping inhibit rule fails closed',
  `${DOWN_RULE}

  - not-a-rule`,
  1,
  'deploy/alertmanager/alertmanager.yml.inhibit_rules[1] must be a mapping',
);

runInhibitFixture(
  'a malformed matcher collection fails even without an equal list',
  `${DOWN_RULE}

  - source_matchers: 'severity = "critical"'
    target_matchers:
      - 'severity = "warning"'`,
  1,
  'deploy/alertmanager/alertmanager.yml.inhibit_rules[1].source_matchers must be a list',
);
