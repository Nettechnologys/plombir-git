#!/usr/bin/env node

// Run the README's shell blocks against a fresh local clone, database and
// listeners. Only the clone URL and listen addresses are replaced so the test
// needs no network access or fixed ports. Cargo's target is shared with the
// checkout to avoid compiling every dependency from scratch.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { adoptChild, removeScratchDir, scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const readme = readFileSync(join(root, 'README.md'), 'utf8');
const work = scratchDir(join(tmpdir(), 'plombir-git-readme-'));
const clone = join(work, 'plombir-git');
const listenFile = join(work, 'listen-addresses');
const cloneDir = join(work, 'cloned-repo');
let server;

function blocks(heading) {
  const start = readme.indexOf(`### ${heading}\n`);
  if (start < 0) throw new Error(`README heading missing: ${heading}`);
  const section = readme.slice(start + heading.length + 5).split(/\n(?=##(?: |# ))/, 1)[0];
  return [...section.matchAll(/```bash\n([\s\S]*?)\n```/g)].map((match) => match[1]);
}

function block(heading, index = 0) {
  const found = blocks(heading);
  if (!found[index]) throw new Error(`README bash block missing: ${heading} #${index + 1}`);
  return found[index];
}

function run(script, cwd, env = {}) {
  const result = spawnSync('bash', ['-c', `set -euo pipefail\n${script}`], {
    cwd,
    env: { ...process.env, ...env },
    stdio: 'inherit',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`README block exited ${result.status}`);
}

async function waitForServer() {
  for (let attempt = 0; attempt < 240; attempt++) {
    if (server.exitCode !== null) throw new Error(`server exited ${server.exitCode} before startup`);
    if (existsSync(listenFile)) {
      const line = readFileSync(listenFile, 'utf8').split('\n').find((value) => value.startsWith('http='));
      if (line) {
        const base = `http://${line.slice(5)}`;
        try {
          const response = await fetch(`${base}/`);
          if (response.ok && /<!doctype html/i.test(await response.text())) return base;
        } catch { /* listener not ready yet */ }
      }
    }
    await new Promise((done) => setTimeout(done, 250));
  }
  throw new Error('server did not serve the built web UI');
}

try {
  const publicClone = 'git clone https://github.com/Nettechnologys/plombir-git.git';
  const build = block('Build').replace(publicClone, `git clone '${root}' plombir-git`);
  if (build === block('Build')) throw new Error('README build block no longer clones the public repository');
  run(build, work, { CARGO_TARGET_DIR: join(root, 'target') });
  symlinkSync(join(root, 'target'), join(clone, 'target'), 'dir');

  run(block('Generate an SSH host key'), clone);
  run(block('Run the server', 0), clone);

  const serve = block('Run the server', 1)
    .replace('127.0.0.1:8080', '127.0.0.1:0')
    .replace('127.0.0.1:2222', '127.0.0.1:0');
  if (serve === block('Run the server', 1) || !serve.includes('--config    ./plombir-git.toml')) {
    throw new Error('README serve block no longer has replaceable loopback listeners and config');
  }
  server = spawn('bash', ['-c', `set -euo pipefail\nexec ${serve} \\\n  --listen-address-file '${listenFile}'`], {
    cwd: clone,
    detached: true,
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  adoptChild(server);
  const base = await waitForServer();
  run(block('Create a repository and clone it'), clone, {
    BASE_URL: base,
    QUICKSTART_CLONE_DIR: cloneDir,
  });
  run(`git -C '${cloneDir}' rev-parse --verify HEAD`, clone);
  console.log('README quick start: build, web UI, registration, DB repository and clone passed');
} finally {
  if (server?.pid) {
    try { process.kill(-server.pid, 'SIGTERM'); } catch { /* already exited */ }
    await new Promise((done) => setTimeout(done, 500));
    try { process.kill(-server.pid, 'SIGKILL'); } catch { /* already exited */ }
  }
  removeScratchDir(work);
}
