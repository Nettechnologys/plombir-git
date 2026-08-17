#!/usr/bin/env node

// Grafana starts even when a provisioning file is malformed; the failure is
// only visible in its logs and the intended dashboards/data sources are absent.
// Parse every mounted provisioning document, then prove that the dashboard
// provider points at the repository directory compose actually mounts.

import { existsSync, readdirSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const defaultRoot = resolve(scriptsDir, '..');

if (process.argv.length > 3) {
  console.error('Usage: node scripts/grafana-provisioning-contract-check.mjs [repository-root]');
  process.exit(1);
}

const root = resolve(process.argv[2] ?? defaultRoot);
const composePath = join(root, 'deploy', 'docker-compose.observability.yml');
const provisioningRoot = join(root, 'deploy', 'grafana', 'provisioning');
const dashboardsRoot = join(root, 'deploy', 'grafana', 'dashboards');
const dashboardConfigsRoot = join(provisioningRoot, 'dashboards');
const datasourceConfigsRoot = join(provisioningRoot, 'datasources');

function display(path) {
  return relative(root, path).split(sep).join('/');
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isWithin(directory, path) {
  const suffix = relative(directory, path);
  return suffix !== '' && suffix !== '..' && !suffix.startsWith(`..${sep}`) && !isAbsolute(suffix);
}

function collectYamlFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...collectYamlFiles(path));
    } else if (entry.isFile() && /\.ya?ml$/i.test(entry.name)) {
      files.push(path);
    }
  }
  return files.sort();
}

function mountFromShortSyntax(value, composeDir) {
  const parts = value.split(':');
  const target = parts.slice(1).find((part) => part.startsWith('/'));
  if (!target) return null;
  const source = parts[0];
  const sourcePath = source.startsWith('.') || source.startsWith('/')
    ? resolve(composeDir, source)
    : null;
  return { sourcePath, target };
}

function grafanaMounts(compose, problems) {
  const volumes = compose?.services?.grafana?.volumes;
  if (!Array.isArray(volumes) || volumes.length === 0) {
    problems.push(`${display(composePath)} has no services.grafana.volumes list.`);
    return [];
  }

  const composeDir = dirname(composePath);
  return volumes.flatMap((volume, index) => {
    if (typeof volume === 'string') {
      const mount = mountFromShortSyntax(volume, composeDir);
      if (!mount) problems.push(`${display(composePath)}: grafana volume #${index + 1} has no container target.`);
      return mount ? [mount] : [];
    }
    if (isObject(volume) && typeof volume.target === 'string') {
      const bindSource = volume.type === 'bind' && typeof volume.source === 'string'
        ? resolve(composeDir, volume.source)
        : null;
      return [{ sourcePath: bindSource, target: volume.target }];
    }
    problems.push(`${display(composePath)}: grafana volume #${index + 1} has unsupported syntax.`);
    return [];
  });
}

const { parser, missing } = selectYamlParser();
if (!parser) {
  console.error(
    `No YAML parser available (tried ${missing.join(', ')}) — install either, `
      + 'or Grafana provisioning cannot be validated.',
  );
  process.exit(1);
}

const problems = [];

function loadYaml(path) {
  const result = parseYamlFile(parser, path);
  if (result.ok) return result.document;
  if (result.kind === 'syntax') {
    problems.push(
      `${display(path)} is not valid YAML — ${parser.name} rejects it:\n     `
        + result.diagnostic.split('\n').join('\n     '),
    );
    return null;
  }
  if (result.kind === 'spawn') {
    console.error(`${parser.name} could not be run on ${display(path)} — ${result.message}`);
  } else if (result.kind === 'output') {
    console.error(`${parser.name} produced unreadable output for ${display(path)} — ${result.message}`);
  } else {
    console.error(
      `${parser.name} failed on ${display(path)} (exit ${result.status})\n${result.diagnostic}`,
    );
  }
  process.exit(1);
}

if (!existsSync(composePath)) {
  problems.push(`${display(composePath)} is missing.`);
}
if (!existsSync(provisioningRoot)) {
  problems.push(`${display(provisioningRoot)}/ is missing — the provisioning glob matched nothing.`);
}

let compose = null;
if (existsSync(composePath)) compose = loadYaml(composePath);

let provisioningFiles = [];
if (existsSync(provisioningRoot)) {
  try {
    provisioningFiles = collectYamlFiles(provisioningRoot);
  } catch (error) {
    console.error(`Cannot enumerate ${display(provisioningRoot)}/ — ${error.message}`);
    process.exit(1);
  }
  if (provisioningFiles.length === 0) {
    problems.push(`${display(provisioningRoot)}/ contains no *.yml or *.yaml files.`);
  }
}

const mounts = isObject(compose) ? grafanaMounts(compose, problems) : [];
if (compose !== null && !isObject(compose)) {
  problems.push(`${display(composePath)} does not parse into a YAML mapping.`);
}

if (!mounts.some((mount) => mount.sourcePath === provisioningRoot && mount.target === '/etc/grafana/provisioning')) {
  problems.push(
    `${display(composePath)} does not bind-mount ${display(provisioningRoot)}/ `
      + 'at /etc/grafana/provisioning.',
  );
}

for (const file of provisioningFiles) {
  const document = loadYaml(file);
  if (document === null) continue;
  if (!isObject(document)) {
    problems.push(`${display(file)} does not parse into a YAML mapping.`);
    continue;
  }
  if (document.apiVersion !== 1) {
    problems.push(`${display(file)} must declare numeric apiVersion: 1.`);
  }

  if (isWithin(dashboardConfigsRoot, file)) {
    if (!Array.isArray(document.providers) || document.providers.length === 0) {
      problems.push(`${display(file)} must declare at least one dashboard provider.`);
      continue;
    }
    for (const [index, provider] of document.providers.entries()) {
      const path = provider?.options?.path;
      if (typeof path !== 'string' || path.trim() === '') {
        problems.push(`${display(file)}: providers[${index}].options.path is missing or empty.`);
        continue;
      }
      if (!mounts.some((mount) => mount.sourcePath === dashboardsRoot && mount.target === path)) {
        problems.push(
          `${display(file)}: providers[${index}].options.path (${path}) is not the target of `
            + `the ${display(dashboardsRoot)}/ bind mount in ${display(composePath)}.`,
        );
      }
    }
  }

  if (isWithin(datasourceConfigsRoot, file)
      && (!Array.isArray(document.datasources) || document.datasources.length === 0)) {
    problems.push(`${display(file)} must declare at least one datasource.`);
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

console.log(
  `grafana provisioning: ${provisioningFiles.length} YAML file(s) parse under ${parser.name}; `
    + 'provisioning and dashboard bind mounts agree with compose',
);
