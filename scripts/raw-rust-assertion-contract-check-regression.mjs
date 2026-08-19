#!/usr/bin/env node

// Mutation stand for raw-rust-assertion-contract-check.mjs.
//
// A green repository only proves that no check greps a `.rs` file raw today.
// It says nothing about whether the ratchet would notice the next one — and a
// detector that has stopped recognising its subject passes exactly the same
// way. Each case below drives the real checker over a private fixture whose
// `scripts/lib/` is a copy of the real one, so the normalizer set is discovered
// the same way it is on `main`, and asserts which side the gate lands on.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const check = join(scriptsDir, 'raw-rust-assertion-contract-check.mjs');

let failed = 0;

/**
 * Run the real check against a fixture holding `files` under `scripts/`.
 *
 * `expect.red` states which side the run must land on; `expect.mentions` and
 * `expect.silent` pin the diagnostic, because a check that goes red for the
 * wrong reason is not evidence about the reason it was written for.
 */
function runCase(name, { files, env = {}, withLib = true, expect }) {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-raw-rust-assert-'));
  try {
    mkdirSync(join(fixture, 'scripts'), { recursive: true });
    if (withLib) cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
    for (const [file, body] of Object.entries(files)) {
      writeFileSync(join(fixture, 'scripts', file), body);
    }

    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: { ...process.env, FORGEKEEP_RAW_RUST_ASSERT_ROOT: fixture, ...env },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const red = result.status !== 0;

    if (red !== expect.red) {
      console.error(
        `❌ ${name}: expected the gate to go ${expect.red ? 'red' : 'green'}, it went ${red ? 'red' : 'green'}:\n${output}`,
      );
      failed += 1;
      return;
    }
    for (const needle of expect.mentions ?? []) {
      if (!output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic never named ${needle}:\n${output}`);
        failed += 1;
        return;
      }
    }
    for (const needle of expect.silent ?? []) {
      if (output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic named ${needle}, which is not the defect:\n${output}`);
        failed += 1;
        return;
      }
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const PATHS = `import { readFileSync } from 'node:fs';
import path from 'node:path';

const backendPath = path.join('crates/rg-demo/src/api/demo.rs');
`;

// The defect the fourteen hand-fixes kept removing: raw bytes, textual assert.
runCase('a raw .includes() over a .rs file is rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const backend = readFileSync(backendPath, 'utf8');
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7', '`backend`'] },
});

// The same assertion, routed through the production view — the shape the
// fourteen were rewritten into. This is the half that proves the gate is not
// simply red about every check that touches Rust.
runCase('the same assertion through productionRustSource is accepted', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/rust-source.mjs';

const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_RUST_ASSERT_MIN: '1' },
  expect: { red: false },
});

// Raw bytes handed straight to a reader are legal: the reader anchors in the
// production view itself, which is the whole point of `rust-source.mjs`.
runCase('raw bytes handed to a reader are accepted', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { rustFnBlock } from './lib/rust-source.mjs';

const backend = readFileSync(backendPath, 'utf8');
const fn = rustFnBlock(backend, 'demo');
if (fn === null || !fn.body.includes('RepoRead')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_RUST_ASSERT_MIN: '1' },
  expect: { red: false },
});

// `requireBlock` matches whatever view its caller hands it, so it is not a
// normalizer — which is exactly the distinction the discovered set has to make.
runCase('raw bytes handed to requireBlock are rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { requireBlock } from './lib/rust-source.mjs';

const failures = [];
const backend = readFileSync(backendPath, 'utf8');
requireBlock(backend, /pub async fn demo[\\s\\S]*?\\n\\}/, 'demo handler', failures);
`,
  },
  expect: { red: true, mentions: ['requireBlock'] },
});

runCase('a regex .test() over raw bytes is rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const backend = readFileSync(backendPath, 'utf8');
if (!/RepoWrite/.test(backend)) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7'] },
});

// A local helper that does nothing but hand back bytes is the same read.
runCase('a local raw-read helper does not launder the bytes', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const read = (relative) => readFileSync(relative, 'utf8');
const backend = read('crates/rg-demo/src/api/demo.rs');
if (!backend.includes('RepoWrite')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:5'] },
});

// `let source = ''; try { source = readFileSync(…) }` binds the bytes without a
// declaration keyword next to the read. The taint has to survive that shape.
runCase('a bare re-assignment does not launder the bytes', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
let backend = '';
try {
  backend = readFileSync(backendPath, 'utf8');
} catch {
  process.exit(1);
}
if (!backend.includes('RepoWrite')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:12'] },
});

// The shape that hid the fifteenth offender from four hand sweeps: one object
// of paths, read into one object of texts. Nothing at the read says which entry
// is Rust, so the taint has to travel by key — and only by the Rust keys.
runCase('a map of files read at once taints only its Rust entries', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const files = {
  client: 'web/src/lib/api/demo.ts',
  backend: 'crates/rg-demo/src/api/demo.rs',
};
const source = Object.fromEntries(
  Object.entries(files).map(([key, file]) => [key, readFileSync(file, 'utf8')]),
);
if (!source.client.includes('export async function demo(')) process.exit(1);
if (!/pub struct CardFull/.test(source.backend)) process.exit(1);
`,
  },
  expect: {
    red: true,
    mentions: ['source.backend', 'demo-contract-check.mjs:11'],
    silent: ['source.client'],
  },
});

// A commented-out offender is code nothing runs. Reporting it would be the very
// mistake this gate exists to punish, one level up.
runCase('a commented-out raw assertion is not reported', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/rust-source.mjs';

const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
// if (!backend.includes('path = "/demo"')) process.exit(1);
/* backend.match(/RepoWrite/); */
const message = 'backend.includes() is the shape this gate rejects';
if (!backend.includes('path = "/demo"') || message === '') process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_RUST_ASSERT_MIN: '1' },
  expect: { red: false },
});

// The subject is Rust. A frontend file read raw and grepped is not this defect,
// and reporting it would make the gate unusable.
runCase('a raw assertion over a TypeScript file is not reported', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const client = readFileSync('web/src/lib/api/demo.ts', 'utf8');
if (!client.includes('export async function demo(')) process.exit(1);
`,
  },
  expect: { red: false },
});

// The anti-vacuous half: a detector that stops recognising reads has to say so
// rather than report a clean corpus it can no longer see.
runCase('a corpus below the recognised-read floor is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const client = readFileSync('web/src/lib/api/demo.ts', 'utf8');
if (!client.includes('export async function demo(')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_RUST_ASSERT_MIN: '1' },
  expect: { red: true, mentions: ['recognised'] },
});

// No library means no normalizer set, and every read would look raw. That must
// be red: a check that cannot read its subject has not passed.
runCase('a missing scripts/lib is rejected rather than passed over', {
  files: {
    'demo-contract-check.mjs': "const x = 1;\n",
  },
  withLib: false,
  expect: { red: true, mentions: ['normalizer set'] },
});

if (failed > 0) {
  console.error(`❌ raw-rust-assertion mutation stand: ${failed} case(s) failed`);
  process.exit(1);
}
console.log('✅ raw-rust-assertion mutation stand: the ratchet bites on raw reads and stays quiet on normalized ones');
