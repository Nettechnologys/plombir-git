#!/usr/bin/env node

// What the shipped deployment files expose, and with which secrets.
//
// Why this exists: three defaults of `deploy/` handed an instance to whoever
// found it first, and nothing read them.
//
//   - The try-out compose published `8080` on every interface while
//     registration was open and the first account became the instance
//     administrator: an operator who started it "to try" on a VPS and went for
//     coffee could come back to a stranger's instance (card_3f6d3099bb31).
//   - The observability compose published Prometheus, Alertmanager, Grafana and
//     node-exporter on every interface, Grafana with admin/admin: anyone could
//     silence the alerts or read the host (card_ab8a1ca92a56).
//   - The quick start generated ONE secret and wrote it into both
//     `PLOMBIR_GIT_JWT_SECRET` and `PLOMBIR_GIT_ENCRYPTION_KEY`, so a leaked
//     signing secret was a leaked encryption key (card_60b16673b390).
//
// So, read as the documents they are:
//
//   1. Every `ports:` entry of every `deploy/docker-compose*.yml` binds
//      loopback, unless it is named in PUBLIC_ON_PURPOSE with the reason. The
//      compose files are read through the YAML parser: a mapping that is only
//      commented out publishes nothing, and a raw scan would count it.
//   2. No shell variable in `deploy/README.md` is written into both secrets,
//      and `deploy/.env.example` ships the at-rest key empty (the server then
//      generates its own). The README is prose an operator pastes from, so its
//      text is the subject.
//   3. The observability compose has no Grafana admin-password fallback, and
//      `deploy/.env.example` ships none to copy.

import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.argv[2] ?? join(scriptsDir, '..'));
const failures = [];

// Mappings that are public by design, each with why. Everything else in the
// shipped compose files is loopback.
const PUBLIC_ON_PURPOSE = [
  {
    file: 'deploy/docker-compose.hostdir.yml',
    service: 'plombir-git',
    target: '2222',
    why: 'git-over-SSH has to reach the clients that push and clone; it authenticates every exec',
  },
];

const LOOPBACK_HOSTS = new Set(['127.0.0.1', '::1', '[::1]']);

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** `{ host, target }` of one compose `ports:` entry, short or long syntax. */
function readMapping(entry) {
  if (isObject(entry)) {
    return { host: entry.host_ip === undefined ? null : String(entry.host_ip), target: String(entry.target) };
  }
  const text = String(entry).split('/')[0];
  // `[::1]:8080:8080`, `127.0.0.1:${PORT:-8080}:8080`, `8080:8080`, `8080`.
  const bracketed = /^(\[[^\]]+\]):(.+):([^:]+)$/.exec(text);
  if (bracketed) return { host: bracketed[1], target: bracketed[3] };
  // A `${VAR:-default}` host port carries colons of its own; take the host IP
  // only when the text opens with a dotted IPv4 literal.
  const ipv4 = /^(\d{1,3}(?:\.\d{1,3}){3}):(.+):([^:]+)$/.exec(text);
  if (ipv4) return { host: ipv4[1], target: ipv4[3] };
  const target = text.slice(text.lastIndexOf(':') + 1);
  return { host: null, target };
}

const { parser, missing } = selectYamlParser();
if (!parser) {
  console.error(`No YAML parser available (tried ${missing.join(', ')}); the compose files cannot be read.`);
  process.exit(1);
}

const composeFiles = readdirSync(join(root, 'deploy'))
  .filter((name) => /^docker-compose.*\.yml$/.test(name))
  .sort()
  .map((name) => `deploy/${name}`);
if (composeFiles.length === 0) {
  failures.push('deploy/ holds no docker-compose*.yml — the glob no longer matches anything');
}

let mappingsRead = 0;
const documents = new Map();
for (const file of composeFiles) {
  const parsed = parseYamlFile(parser, join(root, file));
  if (!parsed.ok) {
    failures.push(`${file}: cannot be read as YAML (${parsed.kind})`);
    continue;
  }
  documents.set(file, parsed.document);
  const services = parsed.document?.services;
  if (!isObject(services)) {
    failures.push(`${file}: parses, but declares no services — the read broke`);
    continue;
  }
  for (const [service, definition] of Object.entries(services)) {
    const ports = isObject(definition) ? definition.ports : undefined;
    if (ports === undefined) continue;
    if (!Array.isArray(ports)) {
      failures.push(`${file}: services.${service}.ports is not a list`);
      continue;
    }
    for (const [index, entry] of ports.entries()) {
      mappingsRead += 1;
      const { host, target } = readMapping(entry);
      if (host !== null && LOOPBACK_HOSTS.has(host)) continue;
      if (PUBLIC_ON_PURPOSE.some((p) => p.file === file && p.service === service && p.target === target)) {
        continue;
      }
      failures.push(
        `${file}: services.${service}.ports[${index}] (${JSON.stringify(entry)}) publishes on `
          + `${host ?? 'every interface'}; bind it to 127.0.0.1, or name it in PUBLIC_ON_PURPOSE with the reason`,
      );
    }
  }
}
if (composeFiles.length > 0 && mappingsRead === 0) {
  failures.push('no ports: entry was read from any compose file — the read broke, not the exposure');
}

// ── The two secrets ─────────────────────────────────────────────────────────
const readme = readFileSync(join(root, 'deploy', 'README.md'), 'utf8');
const writtenInto = new Map();
for (const match of readme.matchAll(/PLOMBIR_GIT_(JWT_SECRET|ENCRYPTION_KEY)=\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?/g)) {
  const [, secret, variable] = match;
  if (!writtenInto.has(variable)) writtenInto.set(variable, new Set());
  writtenInto.get(variable).add(secret);
}
if (![...writtenInto.values()].some((secrets) => secrets.has('JWT_SECRET'))) {
  failures.push('deploy/README.md: no quick-start line writes PLOMBIR_GIT_JWT_SECRET any more — the read broke');
}
for (const [variable, secrets] of writtenInto) {
  if (secrets.size > 1) {
    failures.push(
      `deploy/README.md: \`$${variable}\` is written into both PLOMBIR_GIT_JWT_SECRET and `
        + 'PLOMBIR_GIT_ENCRYPTION_KEY — a leaked signing secret would be a leaked encryption key',
    );
  }
}

const envExample = readFileSync(join(root, 'deploy', '.env.example'), 'utf8');
function envValue(name) {
  const line = envExample.split('\n').find((candidate) => candidate.startsWith(`${name}=`));
  return line === undefined ? undefined : line.slice(name.length + 1).trim();
}
if (envValue('PLOMBIR_GIT_ENCRYPTION_KEY') !== '') {
  failures.push(
    'deploy/.env.example: PLOMBIR_GIT_ENCRYPTION_KEY must ship empty, so the server generates a key of its own',
  );
}
if (envValue('PLOMBIR_GIT_REGISTRATION') !== 'closed') {
  failures.push(
    'deploy/.env.example: PLOMBIR_GIT_REGISTRATION must ship `closed` — on an empty instance the first '
      + 'account to register becomes the administrator',
  );
}

// ── Grafana admin ────────────────────────────────────────────────────────────
if (envValue('GRAFANA_ADMIN_PASSWORD') !== '') {
  failures.push('deploy/.env.example: GRAFANA_ADMIN_PASSWORD must ship empty — a shipped value is a known password');
}
const observability = documents.get('deploy/docker-compose.observability.yml');
const grafanaEnvironment = observability?.services?.grafana?.environment;
const grafanaPassword = Array.isArray(grafanaEnvironment)
  ? grafanaEnvironment.map(String).find((entry) => entry.startsWith('GF_SECURITY_ADMIN_PASSWORD='))
      ?.slice('GF_SECURITY_ADMIN_PASSWORD='.length)
  : isObject(grafanaEnvironment)
    ? grafanaEnvironment.GF_SECURITY_ADMIN_PASSWORD
    : undefined;
if (grafanaPassword === undefined) {
  failures.push('deploy/docker-compose.observability.yml: grafana sets no GF_SECURITY_ADMIN_PASSWORD — Grafana falls back to admin/admin');
} else if (!/^\$\{GRAFANA_ADMIN_PASSWORD:\?[^}]*\}$/.test(String(grafanaPassword))) {
  failures.push(
    `deploy/docker-compose.observability.yml: GF_SECURITY_ADMIN_PASSWORD is \`${grafanaPassword}\`; it must be `
      + '`${GRAFANA_ADMIN_PASSWORD:?...}`, so a missing password stops the stack instead of defaulting',
  );
}

if (failures.length > 0) {
  console.error('❌ deploy exposure contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ deploy exposure contract: ${mappingsRead} published ports across ${composeFiles.length} compose files are loopback `
    + `or public on purpose; the secrets are distinct; Grafana has no default password`,
);
