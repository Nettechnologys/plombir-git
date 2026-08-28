#!/usr/bin/env node

// Copied-tree mutation proof for card_b8608f60b29d.
//
// `docs/ui-inventory.json` is derived from `routes.rs` and test sources. A
// fixture that merely checks the generated artefact can therefore shrink in
// lockstep with a renamed route and stay green. Every mutation below changes
// one live registration's method while leaving its independently-spelled test
// request alone; the oracle contract must reject all ten.

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const copied = [
  'scripts',
  'crates',
  'web/src',
  'docs/ui-access-sweep.json',
];

function baseline(fixture) {
  for (const path of copied) {
    const target = join(fixture, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, path), target, { recursive: true });
  }
}

function patch(fixture, path, from, to) {
  const target = join(fixture, path);
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) {
    throw new Error(`mutation cannot find ${JSON.stringify(from)} in ${path}`);
  }
  writeFileSync(target, source.replace(from, to));
}

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

function mutateMethod(fixture, mutation) {
  const target = join(fixture, 'crates/rg-http/src/routes.rs');
  const source = readFileSync(target, 'utf8');
  const pattern = new RegExp(
    `\\.${mutation.method.toLowerCase()}(\\(\\s*[A-Za-z_][A-Za-z0-9_:]*,\\s*"${escapeRegExp(mutation.registeredRoute)}")`,
    'g',
  );
  const matches = [...source.matchAll(pattern)];
  const match = matches[mutation.occurrence ?? 0];
  if (!match) {
    throw new Error(
      `mutation cannot find occurrence ${mutation.occurrence ?? 0} of `
        + `${mutation.method} ${mutation.registeredRoute} in routes.rs`,
    );
  }
  const replacementMethod = mutation.method === 'PUT' ? 'get' : 'put';
  const replacement = `.${replacementMethod}${match[1]}`;
  const at = match.index;
  writeFileSync(target, `${source.slice(0, at)}${replacement}${source.slice(at + match[0].length)}`);
}

function run(fixture) {
  const result = spawnSync(
    process.execPath,
    [join(fixture, 'scripts/ui-inventory-oracle-contract-check.mjs')],
    { cwd: fixture, encoding: 'utf8', timeout: 60_000 },
  );
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const mutations = [
  {
    method: 'POST',
    registeredRoute: '/{owner}/{repo}/git-upload-pack',
    occurrence: 0,
    expected: 'POST /git/{owner}/{repo}/git-upload-pack',
  },
  {
    method: 'POST',
    registeredRoute: '/{owner}/{repo}/git-receive-pack',
    occurrence: 0,
    expected: 'POST /git/{owner}/{repo}/git-receive-pack',
  },
  {
    method: 'POST',
    registeredRoute: '/{owner}/{repo}/git-upload-pack',
    occurrence: 1,
    expected: 'POST /{owner}/{repo}/git-upload-pack',
  },
  {
    method: 'POST',
    registeredRoute: '/{owner}/{repo}/git-receive-pack',
    occurrence: 1,
    expected: 'POST /{owner}/{repo}/git-receive-pack',
  },
  {
    method: 'GET',
    registeredRoute: '/repos/{owner}/{name}/issues/comments/{comment_id}/assets',
    expected: 'GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets',
  },
  {
    method: 'DELETE',
    registeredRoute: '/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}',
    expected: 'DELETE /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}',
  },
  {
    method: 'GET',
    registeredRoute: '/repos/{owner}/{name}/pulls/{number}/assets',
    expected: 'GET /api/v1/repos/{owner}/{name}/pulls/{number}/assets',
  },
  {
    method: 'DELETE',
    registeredRoute: '/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}',
    expected: 'DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}',
  },
  {
    method: 'GET',
    registeredRoute: '/repos/{owner}/{name}/pulls/comments/{comment_id}/assets',
    expected: 'GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets',
  },
  {
    method: 'PATCH',
    registeredRoute: '/repos/{owner}/{name}/boards/{id}/columns/{col_id}',
    expected: 'PATCH /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}',
  },
  {
    expected: 'a templated child URL must not end inside `}` and cover its parent route',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-inventory.mjs',
      'const PLAIN_SEGMENT = "[^/{}',
      'const PLAIN_SEGMENT = "[^/',
    ),
  },
  {
    expected: 'an empty Rust format placeholder must cover one complete route segment',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-inventory.mjs',
      'const TEMPLATE_SEGMENT = "(?:\\\\{[^}/\\\"\'\\\\x60\\\\s?]*',
      'const TEMPLATE_SEGMENT = "(?:\\\\{[^}/\\\"\'\\\\x60\\\\s?]+',
    ),
  },
  {
    expected: 'a concrete multi-segment tail must cover an Axum catch-all route',
    apply: (fixture) => patch(
      fixture,
      'scripts/ui-inventory.mjs',
      "if (segment.startsWith('{*')) return WILDCARD_TAIL;",
      "if (segment.startsWith('{*')) return ORDINARY_SEGMENT;",
    ),
  },
];

let fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ui-inventory-oracle.'));
try {
  baseline(fixture);
  const clean = run(fixture);
  if (clean.status !== 0) {
    console.error(`❌ UI inventory oracle baseline fixture is red, so mutations prove nothing:\n${clean.output}`);
    process.exit(1);
  }

  for (const mutation of mutations) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ui-inventory-oracle.'));
    baseline(fixture);
    if (mutation.apply) mutation.apply(fixture);
    else mutateMethod(fixture, mutation);
    const result = run(fixture);
    if (result.status === 0 || !result.output.includes(mutation.expected)) {
      console.error(
        `❌ mutation did not go red by route: ${mutation.expected}\n`
          + `status=${result.status}\n${result.output}`,
      );
      process.exit(1);
    }
    console.log(`✅ mutation rejected: ${mutation.expected}`);
  }
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
