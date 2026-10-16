#!/usr/bin/env node

// The server's own image must run an init as PID 1, and nothing that starts the
// image may take it away.
//
// Why this exists: the image used to start `plombir-git serve` straight from
// `CMD`, so the server *was* PID 1. The kernel reparents every orphan in a PID
// namespace to its PID 1, and the server makes orphans by design: a git process
// it kills on a timeout leaves its own children — `pack-objects` under
// `upload-pack`, `git-remote-https` under a mirror fetch — behind. Tokio waits
// only for children it spawned, so each one stayed a zombie for the life of the
// instance, until the container's `pids.max` refused the next fork
// (card_ca2b5f84cb69). Job containers had the same hole and got `--init`
// (c205f9a3); this is the server's half.
//
// What is held:
//   - the runtime stage (the last `FROM`) installs `tini` and declares exactly
//     `ENTRYPOINT ["/usr/bin/tini", "--"]` — a bare `--` so `CMD` and every
//     `docker run <image> <program> …` stay the program;
//   - no shipped compose file overrides `entrypoint:` for a service built from
//     or pulled as this image, which would silently drop the init again.
//
// Truth boundary: this reads the Dockerfile's executable lines (comment lines
// stripped — the ENTRYPOINT is introduced by a comment that names it, and a raw
// `includes` would be satisfied by the explanation alone) and the compose files'
// code halves. Whether `tini` really reaps is Docker's business, measured once
// on the image when this was written; this keeps the image declaring it.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { yamlAnnotatedLines } from './lib/yaml-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.argv[2] || process.env.PLOMBIR_GIT_SERVER_IMAGE_INIT_ROOT || join(scriptsDir, '..'));
const failures = [];

const INIT_ENTRYPOINT = 'ENTRYPOINT ["/usr/bin/tini", "--"]';

const executable = readFileSync(join(root, 'Dockerfile'), 'utf8')
  .split('\n')
  .filter((line) => !line.trimStart().startsWith('#'));
const lastFrom = executable.findLastIndex((line) => /^\s*FROM\b/i.test(line));
if (lastFrom === -1) {
  failures.push('Dockerfile: no FROM line — the runtime stage this check reads is gone');
}
const runtime = executable.slice(Math.max(lastFrom, 0));
const runtimeText = runtime.join('\n');

// Instructions are read with their continuations folded, so `apt-get install`
// spread over many `\`-lines is one instruction.
const instructions = runtimeText.replace(/\\\n/g, ' ').split('\n');
const installsTini = instructions.some(
  (instruction) => /^\s*RUN\b/i.test(instruction)
    && /\bapt-get\s+install\b/.test(instruction)
    && /(?:^|\s)tini(?:\s|$)/.test(instruction),
);
if (!installsTini) {
  failures.push('Dockerfile: the runtime stage does not `apt-get install` tini, so the server is PID 1 again');
}

const entrypoints = instructions.map((line) => line.trim()).filter((line) => /^ENTRYPOINT\b/i.test(line));
if (entrypoints.length === 0) {
  failures.push(
    `Dockerfile: the runtime stage declares no ENTRYPOINT, so \`plombir-git serve\` is PID 1 and every orphan `
      + `of a killed git process stays a zombie; declare \`${INIT_ENTRYPOINT}\``,
  );
}
for (const entrypoint of entrypoints) {
  if (entrypoint !== INIT_ENTRYPOINT) {
    failures.push(`Dockerfile: \`${entrypoint}\` is not \`${INIT_ENTRYPOINT}\``);
  }
}

// A compose `entrypoint:` replaces the image's, init included. The shipped
// files run this image under one service each; observability's compose runs
// other images and is not this image's business.
for (const compose of ['deploy/docker-compose.yml', 'deploy/docker-compose.hostdir.yml', 'deploy/docker-compose.release.yml']) {
  const lines = yamlAnnotatedLines(readFileSync(join(root, compose), 'utf8'));
  lines.forEach(({ code }, index) => {
    if (/^\s*entrypoint\s*:/.test(code)) {
      failures.push(`${compose}:${index + 1}: \`${code.trim()}\` replaces the image's init`);
    }
  });
}

if (failures.length > 0) {
  console.error('❌ server image init contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ server image init contract: tini is PID 1 and execs the command it is given');
