#!/usr/bin/env node

// The observability compose file, read the way a parser reads it, for the two
// facts the `observability-config` job needs from it: which image tags the stack
// runs, and whether every file it bind-mounts exists.
//
// Why this exists: both facts were pulled out of the raw bytes by shell, in two
// identical copies — `.github/workflows/regression.yml` and its local mirror in
// `scripts/run-local-gates.mjs`. The tag read was
//
//     grep -oE "image: prom/${service}:[^[:space:]]+" "${COMPOSE}" | head -1 | cut -d' ' -f2
//
// and `head -1` hands the win to whichever line comes first, commented or not:
//
//     prometheus:
//       # image: prom/prometheus:v1.0.0-stale
//       image: prom/prometheus:v2.55.0
//
// reads back `prom/prometheus:v1.0.0-stale`. That is the exact defect the job's
// own comment exists to prevent — the tag is read out of compose so that the
// promtool validating the config is the promtool that loads it, and a stale
// commented pin validates it with a promtool the stack does not contain. The
// empty case was masked too: `head -1` of nothing is an empty string, so only a
// total absence of `prom/` lines tripped the "no longer pins" branch.
//
// The mount list had the same shape one step down —
// `sed -nE 's|^[[:space:]]+- (\./[^:]+):.*|\1|p'` — which reads a commented-out
// mount as a live one (a red run over a file nothing mounts) and cannot see a
// long-syntax `source:` at all (a green run over a mount nobody checked).
//
// This is the fourth language of that class. Rust, TypeScript and YAML each got
// a production view under `scripts/lib/` so that a construct which is merely
// commented out stops satisfying a textual assertion; the shell half was still
// grepping bytes because its reader is `bash`, not Node, and no view of ours is
// reachable from there. The answer is not a shell-side view: where the claim is
// structural — a service's `image`, a service's `volumes` — the parsed document
// is the subject, so the read happens here, once, and both shell halves ask this
// script instead of the file.
//
// The subject is deliberately this one compose file. `docker-compose.hostdir.
// yml` mounts `./plombir-git.toml` and `./data`, which the operator creates and
// the repository must not contain — asserting their existence here would fail on
// purpose, on a correct checkout.
//
// Output contract, because two callers depend on it:
//   - stdout carries `<service>=<image>` lines and nothing else, which is
//     `GITHUB_OUTPUT` syntax, so the workflow step is a redirect and the local
//     mirror is one `spawnSync`.
//   - everything a human reads goes to stderr, so a caller may consume stdout
//     without losing the log.
//   - exit 1 with the problems on stderr when any assertion fails.

import { existsSync, statSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const defaultRoot = resolve(scriptsDir, '..');

if (process.argv.length > 3) {
  console.error('Usage: node scripts/observability-compose-contract-check.mjs [repository-root]');
  process.exit(1);
}

const root = resolve(process.argv[2] ?? defaultRoot);
const composePath = join(root, 'deploy', 'docker-compose.observability.yml');
const composeDir = dirname(composePath);

// The services whose image the job has to run itself — `promtool` and `amtool`
// ship inside these images and nowhere else. Grafana is deliberately absent: it
// has no offline linter, so nothing here launches its image and a tag for it
// would be an unused value that silently rots.
//
// The repository is pinned as well as the tag. Dropping that turns a switch to a
// different registry into a silent pass, and the whole point of reading the tag
// from compose is that this gate runs the image the stack runs.
const IMAGE_SERVICES = [
  { service: 'prometheus', repository: 'prom/prometheus' },
  { service: 'alertmanager', repository: 'prom/alertmanager' },
];

function display(path) {
  return relative(root, path).split(sep).join('/');
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

const problems = [];
const notes = [];

const { parser, missing } = selectYamlParser();
if (!parser) {
  console.error(
    `No YAML parser available (tried ${missing.join(', ')}) — install either, `
      + 'or the observability compose file cannot be read as a document.',
  );
  process.exit(1);
}

if (!existsSync(composePath)) {
  console.error(`❌ ${display(composePath)} is missing — the observability stack has no compose file.`);
  process.exit(1);
}

const parsed = parseYamlFile(parser, composePath);
if (!parsed.ok) {
  if (parsed.kind === 'syntax') {
    console.error(
      `❌ ${display(composePath)} is not valid YAML — ${parser.name} rejects it:\n     `
        + parsed.diagnostic.split('\n').join('\n     '),
    );
  } else if (parsed.kind === 'spawn') {
    console.error(`${parser.name} could not be run on ${display(composePath)} — ${parsed.message}`);
  } else if (parsed.kind === 'output') {
    console.error(`${parser.name} produced unreadable output for ${display(composePath)} — ${parsed.message}`);
  } else {
    console.error(`${parser.name} failed on ${display(composePath)} (exit ${parsed.status})\n${parsed.diagnostic}`);
  }
  process.exit(1);
}

const services = parsed.document?.services;

// A document that parses into no services is the read breaking, not the compose
// file passing: every assertion below would be vacuously satisfied by it.
if (!isObject(services) || Object.keys(services).length === 0) {
  console.error(`❌ ${display(composePath)} parses, but declares no services — the read broke, not the stack.`);
  process.exit(1);
}

// ── The image pins the job has to run ──
const tags = [];

for (const { service, repository } of IMAGE_SERVICES) {
  const definition = services[service];
  if (!isObject(definition)) {
    problems.push(`${display(composePath)} no longer declares a \`${service}\` service.`);
    continue;
  }

  const image = definition.image;
  if (typeof image !== 'string' || image.trim() === '') {
    problems.push(
      `${display(composePath)}: services.${service}.image is `
        + `${image === undefined ? 'absent' : JSON.stringify(image)}, not an image reference.`,
    );
    continue;
  }

  const reference = image.trim();
  const separator = reference.lastIndexOf(':');
  const name = separator === -1 ? reference : reference.slice(0, separator);
  const tag = separator === -1 ? '' : reference.slice(separator + 1);

  if (name !== repository) {
    problems.push(
      `${display(composePath)}: services.${service}.image is \`${reference}\`, not a \`${repository}\` image — `
        + 'this gate runs that image to validate the config, so it must be the one the stack runs.',
    );
    continue;
  }
  // An unpinned tag makes the validating image drift away from the loading one
  // between two runs of the same commit, which is the false green this whole
  // read exists to prevent.
  if (tag === '' || tag === 'latest') {
    problems.push(
      `${display(composePath)}: services.${service}.image is \`${reference}\` — `
        + 'pin an explicit tag, `latest` is a different image on a different day.',
    );
    continue;
  }

  tags.push(`${service}=${reference}`);
  notes.push(`${service}: ${reference}`);
}

// ── Every file the stack bind-mounts ──
//
// Docker's failure mode here is silent: a bind-mount source that does not exist
// is CREATED as an empty directory, so a renamed alerts.yml gets Prometheus a
// directory where it expects a rule file. `docker compose config` validates the
// YAML and never looks at the paths.
//
// Only relative sources are checkable: they are files committed to this
// repository. An absolute path or a named volume belongs to the host and to
// Docker respectively, and asserting over either would fail on purpose.
function bindSource(volume, service, index) {
  const where = `${display(composePath)}: services.${service}.volumes[${index}]`;

  if (typeof volume === 'string') {
    const parts = volume.split(':');
    const target = parts.slice(1).find((part) => part.startsWith('/'));
    if (!target) {
      problems.push(`${where} (\`${volume}\`) has no container path — this gate cannot tell what it mounts.`);
      return null;
    }
    return parts[0];
  }

  if (isObject(volume)) {
    // Long syntax. A `volume`-type entry names a Docker volume, not a path.
    if (volume.type !== undefined && volume.type !== 'bind') return null;
    if (typeof volume.source !== 'string' || volume.source === '') {
      problems.push(`${where} is a bind mount with no readable \`source\`.`);
      return null;
    }
    return volume.source;
  }

  problems.push(`${where} is neither a string nor a mapping — this gate cannot read it.`);
  return null;
}

let relativeMounts = 0;

for (const [service, definition] of Object.entries(services)) {
  if (!isObject(definition)) {
    problems.push(`${display(composePath)}: services.${service} is not a mapping.`);
    continue;
  }
  const volumes = definition.volumes;
  if (volumes === undefined) continue;
  if (!Array.isArray(volumes)) {
    problems.push(`${display(composePath)}: services.${service}.volumes is not a list.`);
    continue;
  }

  volumes.forEach((volume, index) => {
    const source = bindSource(volume, service, index);
    if (source === null) return;
    if (isAbsolute(source) || !source.startsWith('.')) return;

    relativeMounts += 1;
    const path = resolve(composeDir, source);
    if (existsSync(path)) {
      notes.push(`ok  ${display(path)}${statSync(path).isDirectory() ? '/' : ''}`);
    } else {
      problems.push(
        `${display(composePath)}: services.${service} mounts ${source}, and ${display(path)} does not exist — `
          + 'Docker would create an empty directory there.',
      );
    }
  });
}

if (relativeMounts === 0) {
  problems.push(`${display(composePath)} declares no relative bind mounts — the read broke, not the stack.`);
}

for (const note of notes) console.error(note);

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

// stdout is the machine half and carries nothing else: `GITHUB_OUTPUT` lines for
// the workflow, the same lines for the local mirror.
process.stdout.write(`${tags.join('\n')}\n`);
console.error(
  `observability compose: ${tags.length} pinned image(s), ${relativeMounts} bind-mounted path(s) present`,
);
