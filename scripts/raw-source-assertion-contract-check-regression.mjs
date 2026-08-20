#!/usr/bin/env node

// Mutation stand for raw-source-assertion-contract-check.mjs.
//
// A green repository only proves that no check greps a guarded file raw today.
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
const check = join(scriptsDir, 'raw-source-assertion-contract-check.mjs');

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
      env: { ...process.env, FORGEKEEP_RAW_SOURCE_ASSERT_ROOT: fixture, ...env },
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
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
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
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// The subclass this ratchet was narrowed for: a view that drops comments but
// keeps `#[cfg(test)]` items. It reads like a normalizer and is not one — a
// test double declared at column 0 satisfies an assertion written about the
// handler the server ships (card_04cdbcb8d553).
runCase('a comment-only view of a .rs file is rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { stripRustComments } from './lib/rust-source.mjs';

const backend = stripRustComments(readFileSync(backendPath, 'utf8'));
if (!backend.includes('pub async fn demo')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:8', '`backend`'] },
});

// A Rust path written as an object property whose value spans a comma. The
// property scanner used to stop at the first one, so `path.join(root, '….rs')`
// declared the property path-free and the whole file left the sweep — which is
// how `org-repo-create-contract-check.mjs` asserted over a comment-only view
// while appearing in no offender list.
runCase('a .rs path assembled inside an object property is recognised', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';
import path from 'node:path';

const files = {
  client: path.join('web', 'src/lib/api/demo.ts'),
  backend: path.join('crates', 'rg-demo/src/api/demo.rs'),
};

const backend = readFileSync(files.backend, 'utf8');
if (!backend.includes('pub async fn demo')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:10', '`backend`'] },
});

// Nothing binds the bytes, so no name carries the taint — the assertion rides
// the read expression itself. `observability-contract-check.mjs` sat in this
// blind spot and counted metrics declared inside a `#[cfg(test)]` module.
runCase('an assertion chained onto the read expression is rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { stripRustComments } from './lib/rust-source.mjs';

for (const statement of stripRustComments(readFileSync(backendPath, 'utf8')).split(';')) {
  if (statement.includes('demo_total')) process.exit(0);
}
process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7', '.split('] },
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
// is which language, so the taint has to travel by key — and each language's
// pass has to claim its own keys and no others. The `.rs` entry is reported as
// Rust, the `.ts` entry as TypeScript, and the `.json` entry not at all.
runCase('a map of files read at once taints each entry by its own language', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const files = {
  client: 'web/src/lib/api/demo.ts',
  backend: 'crates/rg-demo/src/api/demo.rs',
  translations: 'web/src/lib/i18n/en.json',
};
const source = Object.fromEntries(
  Object.entries(files).map(([key, file]) => [key, readFileSync(file, 'utf8')]),
);
if (!source.client.includes('export async function demo(')) process.exit(1);
if (!/pub struct CardFull/.test(source.backend)) process.exit(1);
if (!source.translations.includes('demo.title')) process.exit(1);
`,
  },
  expect: {
    red: true,
    mentions: ['source.backend', 'source.client', 'Rust source file', 'TypeScript source file'],
    silent: ['source.translations'],
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
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// The frontend half of the same wire, and the reason this file stopped being
// Rust-only. Commenting out the line that sets `Content-Disposition` in
// `packages.ts` left `package-publish-contract-check.mjs` green over a header
// the client no longer sends, months after the Rust half of that very check had
// been hardened (card_a54b6a2db9f2).
runCase('a raw .includes() over a .ts file is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const client = readFileSync('web/src/lib/api/demo.ts', 'utf8');
if (!client.includes('export async function demo(')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:4', '`client`', 'TypeScript source file'] },
});

// A `.svelte` page is guarded on the same terms: markup commented out with
// `<!-- … -->` is not rendered, so a raw grep over it asserts about a page
// nobody sees.
runCase('a raw regex over a .svelte page is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const page = readFileSync('web/src/routes/demo/+page.svelte', 'utf8');
if (!/href="\\/demo"/.test(page)) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:4', '`page`', 'TypeScript source file'] },
});

// The accepting half, so the TypeScript row is not simply red about every check
// that touches the frontend.
runCase('the same assertions through productionTsSource are accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { productionTsSource, tsInterfaceBody } from './lib/ts-source.mjs';

const client = productionTsSource(readFileSync('web/src/lib/api/demo.ts', 'utf8'));
const page = productionTsSource(readFileSync('web/src/routes/demo/+page.svelte', 'utf8'));
const body = tsInterfaceBody(readFileSync('web/src/lib/api/types.ts', 'utf8'), 'Demo');
if (!client.includes('export async function demo(')) process.exit(1);
if (!/href="\\/demo"/.test(page)) process.exit(1);
if (body === null) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_TYPESCRIPT: '1' },
  expect: { red: false },
});

// The languages do not launder each other. A Rust production view over
// TypeScript bytes blanks `#[cfg(test)]` items and Rust comment syntax — none
// of which is what a `.ts` file is made of — so it must count as raw here.
runCase('a Rust view over TypeScript bytes is still raw', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { productionRustSource } from './lib/rust-source.mjs';

const client = productionRustSource(readFileSync('web/src/lib/api/demo.ts', 'utf8'));
if (!client.includes('export async function demo(')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`client`', 'TypeScript source file'] },
});

// A path written as a bare string literal rather than assembled with
// `path.join(…)`. In the code view a literal is blanked *including its quotes*,
// so the binding scanner used to walk straight past the initializer and hand
// back an empty span: the name carried no path, the read of it was not counted,
// and the whole file left the sweep without appearing anywhere. Nine checks sat
// there, and the Rust half had the same hole.
runCase('a path written as a bare string literal is recognised', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const pagePath = 'web/src/routes/demo/+page.svelte';
const page = readFileSync(pagePath, 'utf8');
if (!page.includes('href="/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:5', '`page`'] },
});

// A check's own `expect(source, pattern, message)` is where all of its
// assertions go, and it launders the bytes exactly as an imported helper would.
// Covering only the library helpers left every check built that way invisible —
// including `password-reset-contract-check.mjs`, which handed the raw bytes of
// `password.rs` to its local `expect` six times over.
runCase('a local assertion helper does not launder the bytes', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const failures = [];
function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

const client = readFileSync('web/src/lib/api/demo.ts', 'utf8');
expect(client, /export async function demo\\(/, 'the client must call demo');
`,
  },
  expect: { red: true, mentions: ['expect()', '`client`'] },
});

// The other side of that rule: a local helper that puts the bytes into the
// production view itself is a normalizer, not a sink, and handing it raw bytes
// is the correct shape rather than the defect.
runCase('a local helper that normalizes is not a sink', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { productionTsSource } from './lib/ts-source.mjs';

function declaresDemo(source) {
  return productionTsSource(source).includes('export async function demo(');
}

if (!declaresDemo(readFileSync('web/src/lib/api/demo.ts', 'utf8'))) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_TYPESCRIPT: '1' },
  expect: { red: false },
});

// The third language. A single `#` in front of a `run:` line is enough, and it
// is what left four gates green over a `regression.yml` whose contract-check
// step no longer existed (card_fad8ad0ef007).
runCase('a raw .includes() over a .yml file is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

const workflow = readFileSync('.github/workflows/regression.yml', 'utf8');
if (!workflow.includes('run: node scripts/run-contract-checks.mjs')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:4', '`workflow`', 'YAML source file'] },
});

// The accepting half, so the YAML row is not simply red about every check that
// reads configuration.
runCase('the same assertion through productionYamlSource is accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { productionYamlSource } from './lib/yaml-source.mjs';

const workflow = productionYamlSource(readFileSync('.github/workflows/regression.yml', 'utf8'));
if (!workflow.includes('run: node scripts/run-contract-checks.mjs')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_YAML: '1' },
  expect: { red: false },
});

// `yamlAnnotatedLines` is the reader for a claim about a marker comment. It
// reaches the production view itself, so handing it raw bytes is the correct
// shape rather than the defect — the same rule `rustFnBlock` gets.
runCase('raw bytes handed to the annotated-line reader are accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { yamlAnnotatedLines } from './lib/yaml-source.mjs';

const marked = yamlAnnotatedLines(readFileSync('deploy/docker-compose.yml', 'utf8'))
  .filter(({ comment }) => comment === 'HTTP');
if (marked.length !== 1) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_YAML: '1' },
  expect: { red: false },
});

// A path that is bound by a `for (const … of …)` and never by an `=`. The
// scanner keyed on assignments walked straight past it, so the loop variable
// carried no path and the file left the sweep in silence — where
// `deploy-config-concurrency-contract-check.mjs` and
// `repo-actions-contract-check.mjs` both sat.
runCase('a path bound by a for-of loop is recognised', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';
import { join } from 'node:path';

for (const file of ['docker-compose.yml', 'docker-compose.hostdir.yml']) {
  const compose = readFileSync(join('deploy', file), 'utf8');
  if (!compose.includes('- \${FORGEKEEP_DEPLOY_ENV_FILE:-.env}')) process.exit(1);
}
`,
  },
  expect: { red: true, mentions: ['`compose`', 'YAML source file'] },
});

// The other side of that widening, and the reason it is not simply "anything a
// loop walks is a path": `sourceFiles(…, ['.rs'])` returns a directory walk,
// and its `'.rs'` is an extension filter. That shape is the documented truth
// boundary (card_2a23d37a583c) — recognising it here would redden
// `released-port-contract-check.mjs`, a deliberate NEGATIVE sweep that keeps
// `#[cfg(test)]` items precisely because test fixtures are what it hunts.
runCase('a loop over a directory walk stays outside the sweep', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

function sourceFiles(dir, extensions) {
  return readdirSync(dir).filter((name) => extensions.some((ext) => name.endsWith(ext))).map((name) => join(dir, name));
}

for (const file of sourceFiles('crates', ['.rs'])) {
  const source = readFileSync(file, 'utf8');
  if (source.includes('TcpListener::bind')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '0' },
  expect: { red: false },
});

// The one family held out of the glob. A stand copies the repository into a
// fixture and edits the bytes there — anchoring on a YAML *comment* is normal,
// `observability-contract-check-regression.mjs` inserts an alert rule before
// `      # Slow requests` — and then judges the real check by its exit code in
// both directions. A drifted anchor throws and a vacuous mutation leaves the
// check green where red was demanded, so raw bytes there cannot buy a false
// green and this ratchet has nothing to protect.
runCase('a raw .yml read inside a mutation stand is not reported', {
  files: {
    'demo-contract-check-regression.mjs': `import { readFileSync, writeFileSync } from 'node:fs';

const alerts = readFileSync('deploy/prometheus/alerts.yml', 'utf8');
const anchor = '      # Slow requests';
if (!alerts.includes(anchor)) throw new Error('fixture anchor disappeared');
writeFileSync('/tmp/fixture-alerts.yml', alerts.replace(anchor, 'mutated'));
`,
  },
  expect: { red: false },
});

// The anti-vacuous half: a detector that stops recognising reads has to say so
// rather than report a clean corpus it can no longer see. The fixture asserts
// nothing raw, so the only thing left that can redden it is the floor — set
// here above the single read the corpus contains.
runCase('a corpus below the recognised-read floor is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';

import { productionTsSource } from './lib/ts-source.mjs';

const client = productionTsSource(readFileSync('web/src/lib/api/demo.ts', 'utf8'));
if (!client.includes('export async function demo(')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_TYPESCRIPT: '2' },
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
  console.error(`❌ raw-source-assertion mutation stand: ${failed} case(s) failed`);
  process.exit(1);
}
console.log('✅ raw-source-assertion mutation stand: the ratchet bites on raw reads in all three guarded languages and stays quiet on normalized ones');
