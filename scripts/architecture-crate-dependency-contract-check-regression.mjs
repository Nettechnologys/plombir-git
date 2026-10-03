#!/usr/bin/env node

// Prove the architecture dependency check notices drift from either side: a
// deleted documented edge and a new Cargo edge must both turn the real check
// red. The stand uses a minimal throwaway workspace so it tests cargo metadata,
// not a second parser that could agree with the check by sharing its bug.

import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = resolve(scriptsDir, 'architecture-crate-dependency-contract-check.mjs');
const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-architecture-deps-'));

function write(path, body) {
  const full = resolve(fixture, path);
  mkdirSync(dirname(full), { recursive: true });
  writeFileSync(full, body);
}

function graph(aTargets = 'rg-b') {
  return `# Fixture\n\n### Crate dependency direction\n\n\`\`\`\nrg-a ──> ${aTargets}\nrg-b ──> none\nrg-c ──> none\n\`\`\`\n`;
}

function run() {
  return spawnSync(process.execPath, [check], {
    cwd: fixture,
    encoding: 'utf8',
    env: { ...process.env, PLOMBIR_GIT_ARCHITECTURE_DEPENDENCY_ROOT: fixture },
  });
}

function output(result) {
  return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

try {
  write('Cargo.toml', '[workspace]\nmembers = ["crates/rg-a", "crates/rg-b", "crates/rg-c"]\nresolver = "2"\n');
  for (const name of ['rg-a', 'rg-b', 'rg-c']) {
    const dependency = name === 'rg-a' ? '\n[dependencies]\nrg-b = { path = "../rg-b" }\n' : '';
    write(
      `crates/${name}/Cargo.toml`,
      `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n${dependency}`,
    );
    write(`crates/${name}/src/lib.rs`, '');
  }
  write('ARCHITECTURE.md', graph());

  const clean = run();
  if (clean.status !== 0) {
    throw new Error(`clean fixture failed:\n${output(clean)}`);
  }

  write('ARCHITECTURE.md', graph('none'));
  const missingDocumentedEdge = run();
  if (missingDocumentedEdge.status === 0 || !output(missingDocumentedEdge).includes('rg-a')) {
    throw new Error(`deleted documented edge was not rejected:\n${output(missingDocumentedEdge)}`);
  }

  write('ARCHITECTURE.md', graph());
  write(
    'crates/rg-a/Cargo.toml',
    '[package]\nname = "rg-a"\nversion = "0.1.0"\nedition = "2021"\n\n'
      + '[dependencies]\nrg-b = { path = "../rg-b" }\nrg-c = { path = "../rg-c" }\n',
  );
  const newCargoEdge = run();
  if (newCargoEdge.status === 0 || !output(newCargoEdge).includes('rg-a')) {
    throw new Error(`new Cargo edge was not rejected:\n${output(newCargoEdge)}`);
  }

  console.log('architecture crate dependency regression: clean + docs drift + Cargo drift proven');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
