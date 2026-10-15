#!/usr/bin/env node

// Does advisory-ignore-list-contract-check.mjs still bite? Each fixture copies
// the four files it reads, applies one mutation, and expects a red run naming
// that mutation. The unmutated copy must stay green, or a red fixture proves
// nothing.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { scratchDir, removeScratchDir } from './lib/scratch-dir.mjs';

const FILES = ['.cargo/audit.toml', 'deny.toml', 'osv-scanner.toml', '.github/workflows/regression.yml'];
const original = Object.fromEntries(FILES.map((file) => [file, readFileSync(file, 'utf8')]));

function replaceOnce(text, from, to, label) {
  const at = text.indexOf(from);
  if (at < 0 || text.indexOf(from, at + 1) >= 0) {
    console.error(`stand fixture "${label}": anchor ${JSON.stringify(from)} is not unique in the tree — update the stand.`);
    process.exit(1);
  }
  return text.slice(0, at) + to + text.slice(at + from.length);
}

const OSV_STEP = '        run: osv-scanner scan --config=osv-scanner.toml --lockfile=Cargo.lock\n';
const OSV_ENTRY = '[[IgnoredVulns]]\nid = "RUSTSEC-2025-0134"\n';

const fixtures = [
  { label: 'unmutated', mutate: {}, expect: null },
  {
    label: 'osv-scanner.toml drops an accepted id',
    mutate: { 'osv-scanner.toml': (t) => t.slice(0, t.indexOf(OSV_ENTRY)) },
    expect: 'RUSTSEC-2025-0134 is accepted in .cargo/audit.toml, deny.toml but not in osv-scanner.toml.',
  },
  {
    label: 'deny.toml accepts one more id',
    mutate: { 'deny.toml': (t, l) => replaceOnce(t, '    "RUSTSEC-2025-0134",\n', '    "RUSTSEC-2025-0134",\n    "RUSTSEC-2099-0001",\n', l) },
    expect: 'RUSTSEC-2099-0001 is accepted in deny.toml but not in .cargo/audit.toml, osv-scanner.toml.',
  },
  {
    label: 'osv entry without a reason',
    mutate: { 'osv-scanner.toml': (t, l) => replaceOnce(t, `${OSV_ENTRY}reason = """\\`, `${OSV_ENTRY}note = """\\`, l) },
    expect: 'IgnoredVulns[1] (RUSTSEC-2025-0134) carries no reason',
  },
  {
    label: 'scan step continues on error',
    mutate: { '.github/workflows/regression.yml': (t, l) => replaceOnce(t, OSV_STEP, `        continue-on-error: true\n${OSV_STEP}`, l) },
    expect: 'step "osv-scanner (OSV/GHSA advisories)" sets continue-on-error',
  },
  {
    label: 'scan step ignores the config',
    mutate: { '.github/workflows/regression.yml': (t, l) => replaceOnce(t, OSV_STEP, '        run: osv-scanner scan --lockfile=Cargo.lock\n', l) },
    expect: 'does not pass --config=osv-scanner.toml',
  },
  {
    label: 'scan step swallows its exit status',
    mutate: { '.github/workflows/regression.yml': (t, l) => replaceOnce(t, OSV_STEP, '        run: osv-scanner scan --config=osv-scanner.toml --lockfile=Cargo.lock || true\n', l) },
    expect: "can swallow the scanner's exit status",
  },
  {
    label: 'osv-scanner removed from the job',
    mutate: { '.github/workflows/regression.yml': (t, l) => replaceOnce(t, OSV_STEP, '        run: echo skipped\n', l) },
    expect: 'never runs osv-scanner',
  },
];

const failures = [];
for (const fixture of fixtures) {
  const dir = scratchDir(join(tmpdir(), 'advisory-ignore-check-'));
  try {
    for (const file of FILES) {
      const mutate = fixture.mutate[file];
      mkdirSync(dirname(join(dir, file)), { recursive: true });
      writeFileSync(join(dir, file), mutate ? mutate(original[file], fixture.label) : original[file]);
    }
    const result = spawnSync(process.execPath, ['scripts/advisory-ignore-list-contract-check.mjs'], {
      env: { ...process.env, ADVISORY_IGNORE_ROOT: dir },
      encoding: 'utf8',
    });
    if (fixture.expect === null) {
      if (result.status !== 0) failures.push(`${fixture.label}: expected green, got ${result.status}\n${result.stderr}`);
    } else if (result.status !== 1 || !result.stderr.includes(fixture.expect)) {
      failures.push(`${fixture.label}: expected red naming ${JSON.stringify(fixture.expect)}, got ${result.status}\n${result.stdout}${result.stderr}`);
    }
  } finally {
    removeScratchDir(dir);
  }
}

if (failures.length) {
  console.error(`❌ advisory ignore-list check no longer bites:\n${failures.join('\n')}`);
  process.exit(1);
}
console.log(`advisory ignore-list check rejects all ${fixtures.length - 1} mutations and passes the real tree`);
