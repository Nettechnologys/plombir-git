#!/usr/bin/env node

// Every third-party image pinned where no Dependabot ecosystem looks is pinned
// a second time in `.github/ci-images/Dockerfile`, which one does — and this
// check keeps the two pins the same reference.
//
// Why this exists: a digest fixes the bytes, and without an updater it also
// fixes the patch level forever. Dependabot's `docker` ecosystem read the root
// Dockerfile's `FROM` lines and its `docker-compose` ecosystem read the
// observability stack; nothing read the PostgreSQL and MySQL `services:` of the
// smoke jobs, the nginx and Caddy images the reverse-proxy examples are loaded
// by, or the `# syntax=` directive that picks the BuildKit frontend. The weekly
// Dependabot logs of 2026-10-15 carry no `Checking if` line for any of them
// (card_2d33ee054ad2).
//
// `.github/ci-images/Dockerfile` is never built; it is a list Dependabot can
// read. A consumer that can read a file reads it (the reverse-proxy script); a
// consumer that cannot — a workflow service image, a parser directive — repeats
// the reference, and this is what fails while the copy and the source differ.
// A Dependabot pull request for one of those is therefore red until the copy
// follows, and the message below names the line.
//
// What is asserted:
//   1. the pin file is one `FROM <name>:<tag>@sha256:<digest> AS <stage>` per
//      line and nothing else, every stage named once — the shell reader in
//      check-reverse-proxy-examples.sh understands exactly that shape;
//   2. `.github/dependabot.yml` has a `docker` entry for that directory;
//   3. every `services.*.image` and `container.image` of every workflow job is
//      one of the pinned references, so a service added later cannot slip in
//      unwatched;
//   4. the root Dockerfile's `# syntax=` directive is the `dockerfile-frontend`
//      pin;
//   5. the reverse-proxy script pins no digest of its own and reads both images
//      out of the pin file.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { shellCodeOnly } from './lib/shell-source.mjs';
import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.argv[2] ?? join(scriptsDir, '..'));

const PIN_FILE = '.github/ci-images/Dockerfile';
const PIN_DIRECTORY = '/.github/ci-images';
const PROXY_SCRIPT = 'scripts/check-reverse-proxy-examples.sh';
const ROOT_DOCKERFILE = 'Dockerfile';

// The stages the pin file must declare, and why each is there. A new pin is a
// new entry here, so the reader that consumes it is named in review.
const STAGES = new Map([
  ['postgres', 'the postgres-smoke service in .github/workflows/regression.yml'],
  ['mysql', 'the mysql-smoke service in .github/workflows/regression.yml'],
  ['nginx', `the nginx image ${PROXY_SCRIPT} loads deploy/nginx.conf.example with`],
  ['caddy', `the Caddy image ${PROXY_SCRIPT} loads deploy/Caddyfile.example with`],
  ['dockerfile-frontend', `the \`# syntax=\` directive on the first line of ${ROOT_DOCKERFILE}`],
]);

const PINNED = /^[a-z0-9][a-z0-9._/-]*:[A-Za-z0-9._-]+@sha256:[0-9a-f]{64}$/;

const failures = [];

function read(relative) {
  const path = join(root, relative);
  if (!existsSync(path)) {
    failures.push(`${relative} is missing`);
    return null;
  }
  return readFileSync(path, 'utf8');
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// ── 1. The pin file ──
const pins = new Map();
const pinSource = read(PIN_FILE);
if (pinSource !== null) {
  pinSource.split('\n').forEach((raw, index) => {
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) return;
    const where = `${PIN_FILE}:${index + 1}`;
    const match = /^FROM (\S+) AS (\S+)$/.exec(line);
    if (!match) {
      failures.push(`${where}: \`${line}\` is not \`FROM <image> AS <stage>\` — the only line this file may hold`);
      return;
    }
    const [, image, stage] = match;
    if (!PINNED.test(image)) {
      failures.push(`${where}: \`${image}\` must be \`<name>:<tag>@sha256:<64 hex>\` — Dependabot needs the tag, the digest is the pin`);
    }
    if (!STAGES.has(stage)) {
      failures.push(`${where}: stage \`${stage}\` has no reader this check knows — add it to STAGES with the line that consumes it`);
    }
    if (pins.has(stage)) {
      failures.push(`${where}: stage \`${stage}\` is pinned twice; a reader would take whichever it meets first`);
    }
    pins.set(stage, image);
  });
  for (const [stage, reader] of STAGES) {
    if (!pins.has(stage)) failures.push(`${PIN_FILE} no longer pins \`${stage}\` (${reader})`);
  }
}

// ── 2 + 3. Dependabot and the workflows, through a YAML parser ──
const { parser, missing } = selectYamlParser();
if (!parser) {
  failures.push(`no YAML parser available (tried ${missing.join(', ')}) — the workflows cannot be read as documents`);
} else {
  const parse = (relative) => {
    const parsed = parseYamlFile(parser, join(root, relative));
    if (!parsed.ok) {
      failures.push(`${relative} could not be parsed (${parsed.kind}): ${parsed.diagnostic ?? parsed.message ?? ''}`.trim());
      return null;
    }
    return parsed.document;
  };

  const dependabot = parse('.github/dependabot.yml');
  if (dependabot !== null) {
    const watched = (Array.isArray(dependabot.updates) ? dependabot.updates : []).some(
      (entry) => isObject(entry)
        && entry['package-ecosystem'] === 'docker'
        && [entry.directory, ...(Array.isArray(entry.directories) ? entry.directories : [])]
          .some((directory) => typeof directory === 'string' && directory.replace(/\/+$/, '') === PIN_DIRECTORY),
    );
    if (!watched) {
      failures.push(`.github/dependabot.yml has no \`docker\` entry for \`${PIN_DIRECTORY}\` — the pins there are watched by nothing`);
    }
  }

  const pinned = new Set(pins.values());
  const workflowsDir = join(root, '.github/workflows');
  const workflows = existsSync(workflowsDir)
    ? readdirSync(workflowsDir).filter((name) => /\.ya?ml$/.test(name)).sort()
    : [];
  if (workflows.length === 0) failures.push('.github/workflows holds no workflow — the read broke, not the pins');

  let servicesRead = 0;
  for (const name of workflows) {
    const relative = `.github/workflows/${name}`;
    const document = parse(relative);
    if (!isObject(document?.jobs)) continue;
    for (const [job, definition] of Object.entries(document.jobs)) {
      if (!isObject(definition)) continue;
      const images = [];
      if (isObject(definition.services)) {
        for (const [service, spec] of Object.entries(definition.services)) {
          images.push([`jobs.${job}.services.${service}.image`, isObject(spec) ? spec.image : spec]);
        }
      }
      if (definition.container !== undefined) {
        const spec = definition.container;
        images.push([`jobs.${job}.container.image`, isObject(spec) ? spec.image : spec]);
      }
      for (const [where, image] of images) {
        servicesRead += 1;
        if (typeof image !== 'string' || !pinned.has(image.trim())) {
          const stage = typeof image === 'string' ? image.split(/[:@]/)[0].split('/').pop() : null;
          const source = stage && pins.get(stage);
          failures.push(
            `${relative}: ${where} is \`${image}\`, which ${PIN_FILE} does not pin`
              + (source ? ` — it pins \`${source}\`; copy that reference into the workflow (a Dependabot bump lands there first)` : ' — pin it there, where Dependabot sees it, and repeat it here'),
          );
        }
      }
    }
  }
  // The two smoke services are why this file exists; a parse that found no
  // service at all would make assertion 3 vacuous.
  if (servicesRead < 2) {
    failures.push(`only ${servicesRead} workflow service or container image read — the smoke jobs' services are no longer where this check looks`);
  }
}

// ── 4. The parser directive ──
const rootDockerfile = read(ROOT_DOCKERFILE);
if (rootDockerfile !== null) {
  const first = rootDockerfile.split('\n', 1)[0].trim();
  const directive = /^#\s*syntax\s*=\s*(\S+)$/.exec(first);
  if (!directive) {
    failures.push(`${ROOT_DOCKERFILE}: the first line is no longer a \`# syntax=\` directive — if the frontend pin is gone, drop \`dockerfile-frontend\` from ${PIN_FILE} and STAGES`);
  } else if (pins.has('dockerfile-frontend') && directive[1] !== pins.get('dockerfile-frontend')) {
    failures.push(
      `${ROOT_DOCKERFILE}:1 selects \`${directive[1]}\`, ${PIN_FILE} pins \`${pins.get('dockerfile-frontend')}\` — `
        + 'copy the pinned reference into the directive (a Dependabot bump lands in the pin file first)',
    );
  }
}

// ── 5. The reverse-proxy script reads, it does not pin ──
const proxySource = read(PROXY_SCRIPT);
if (proxySource !== null) {
  const code = shellCodeOnly(proxySource);
  if (/@sha256:/.test(code)) {
    failures.push(`${PROXY_SCRIPT} pins a digest of its own again — read it from ${PIN_FILE}, the copy is the one no updater bumps`);
  }
  if (!code.includes(PIN_FILE)) {
    failures.push(`${PROXY_SCRIPT} no longer reads ${PIN_FILE}`);
  }
  for (const stage of ['nginx', 'caddy']) {
    if (!new RegExp(`\\bci_image ${stage}\\b`).test(code)) {
      failures.push(`${PROXY_SCRIPT} no longer reads the \`${stage}\` pin (\`ci_image ${stage}\`)`);
    }
  }
}

if (failures.length > 0) {
  console.error('❌ CI image pins contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(`✅ CI image pins: ${pins.size} pins in ${PIN_FILE}, watched by Dependabot, and every copy matches`);
