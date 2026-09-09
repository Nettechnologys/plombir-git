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

// The walk itself, which used to be the documented hole (card_2a23d37a583c).
// The extension is stated in the walker's ARGUMENTS, and nothing anywhere
// spells a `.rs` path — so the reader counted no reads at all and the run
// printed `0 .rs read(s)` and exited 0. The floor could not object: a new
// walk-shaped check adds nothing to the recognised corpus, so the corpus never
// shrinks.
runCase('a raw read of a path taken from a directory walk is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

function sourceFiles(dir, extensions) {
  return readdirSync(dir).filter((name) => extensions.some((ext) => name.endsWith(ext))).map((name) => join(dir, name));
}

for (const file of sourceFiles('crates', ['.rs'])) {
  const source = readFileSync(file, 'utf8');
  if (source.includes('pub async fn demo')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: true, mentions: ['`source`', 'Rust source file'] },
});

// The shape the card was reproduced with: the walk is a bare `readdirSync`, and
// the only thing that says `.rs` is a filter in the LOOP BODY. Neither the
// binding nor the expression it iterates carries a literal, so this is the
// furthest the extension can travel from the read and still be findable.
runCase('a walk filtered inside the loop body is recognised', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

for (const name of readdirSync('crates/rg-demo/src')) {
  if (!name.endsWith('.rs')) continue;
  const backend = readFileSync(join('crates/rg-demo/src', name), 'utf8');
  if (!backend.includes('pub async fn demo')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: true, mentions: ['`backend`', 'Rust source file'] },
});

// The accepting half: the same walk, read through a production view. Without
// this the walk row would only prove the gate is red about every check that
// enumerates a directory.
runCase('a walked read through the production view is accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { productionRustSource } from './lib/rust-source.mjs';

for (const name of readdirSync('crates/rg-demo/src')) {
  if (!name.endsWith('.rs')) continue;
  const backend = productionRustSource(readFileSync(join('crates/rg-demo/src', name), 'utf8'));
  if (!backend.includes('pub async fn demo')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// A walk that names no guarded extension anywhere is not a walk over guarded
// files. `listScripts(dir)` collecting `.mjs` must not make every read of its
// result look like a read of Rust — the widening has to stop where the evidence
// does, or the reader manufactures offenders out of its own subject list.
runCase('a walk over an unguarded extension is not a guarded read', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { productionRustSource } from './lib/rust-source.mjs';

const guarded = productionRustSource(readFileSync('crates/rg-demo/src/api/demo.rs', 'utf8'));
if (!guarded.includes('pub async fn demo')) process.exit(1);

for (const name of readdirSync('scripts')) {
  if (!name.endsWith('.mjs')) continue;
  const script = readFileSync(join('scripts', name), 'utf8');
  if (script.includes('process.exit(0)')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// The negative sweep the walk widening first reddened.
// `released-port-contract-check.mjs` hunts a listener dropped after its address
// was read, and the fixtures it hunts live inside `#[cfg(test)]` items, so the
// production view hides exactly what it came for. It says which view it means
// by name — and the name is the claim a reviewer checks, because the reader
// cannot tell a sweep that REPORTS what it finds from one that REQUIRES a
// construct to be present.
runCase('a test-inclusive view named as such is accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { testInclusiveRustCode } from './lib/rust-source.mjs';

for (const name of readdirSync('crates/rg-demo/src')) {
  if (!name.endsWith('.rs')) continue;
  const source = testInclusiveRustCode(readFileSync(join('crates/rg-demo/src', name), 'utf8'));
  if (source.includes('TcpListener::bind')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// Two walks, one variable name. Recognising walks made every file that sweeps
// two trees look like one: `released-port-contract-check.mjs` walks `crates/`
// for `.rs` and then `scripts/` for `.sh` with a loop variable called `file`
// both times, and a whole-file name match reported the shell read as a raw read
// of Rust. A name means one value inside the loop that declares it.
runCase('a same-named loop variable in a second walk is a different path', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { testInclusiveRustCode } from './lib/rust-source.mjs';

for (const file of readdirSync('crates/rg-demo/src')) {
  if (!file.endsWith('.rs')) continue;
  const source = testInclusiveRustCode(readFileSync(join('crates/rg-demo/src', file), 'utf8'));
  if (source.includes('TcpListener::bind')) process.exit(1);
}

for (const file of readdirSync('scripts')) {
  if (!file.endsWith('.sh')) continue;
  const source = readFileSync(join('scripts', file), 'utf8');
  if (source.includes('getsockname')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// The other half of that rule, so it cannot be satisfied by going quiet: the
// second walk over guarded files, under the same variable name, is still read.
runCase('a second walk over guarded files is still read', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { productionRustSource } from './lib/rust-source.mjs';

for (const file of readdirSync('crates/rg-demo/src')) {
  if (!file.endsWith('.rs')) continue;
  const source = productionRustSource(readFileSync(join('crates/rg-demo/src', file), 'utf8'));
  if (!source.includes('pub async fn demo')) process.exit(1);
}

for (const file of readdirSync('crates/rg-other/src')) {
  if (!file.endsWith('.rs')) continue;
  const source = readFileSync(join('crates/rg-other/src', file), 'utf8');
  if (!source.includes('pub async fn other')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: true, mentions: ['`source`', 'Rust source file'] },
});

// The other half of the walk shape: one that ACCUMULATES. The paths never pass
// through a return value — a closure pushes them into an array declared outside
// it — so nothing at the read, and nothing at the array, spells a path.
// `loadUtoipaPaths` in `scripts/lib/rust-source.mjs` is built exactly that way
// and was the last Rust read in the tree the reader could not see.
runCase('a raw read of a path an accumulator walk collected is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const files = [];
const walk = (dir) => {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) walk(join(dir, entry.name));
    else if (entry.name.endsWith('.rs')) files.push(join(dir, entry.name));
  }
};
walk('crates');

for (const file of files) {
  const backend = readFileSync(file, 'utf8');
  if (!backend.includes('pub async fn demo')) process.exit(1);
}
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: true, mentions: ['`backend`', 'Rust source file'] },
});

// A parameter is not the local it shares a name with. `scripts/lib/
// rust-source.mjs` gives a dozen view builders a parameter called `source`, and
// once the walk inside `loadUtoipaPaths` became visible every one of them was
// reported as raw bytes it had never seen.
runCase('a helper parameter sharing a tainted name is not the tainted value', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { productionRustSource } from './lib/rust-source.mjs';

function lineAt(source, index) {
  return source.slice(0, index).split('\n').length;
}

const files = readdirSync('crates/rg-demo/src').filter((entry) => entry.endsWith('.rs'));
const source = readFileSync(join('crates/rg-demo/src', files[0]), 'utf8');
const view = productionRustSource(source);
if (lineAt(view, view.indexOf('pub async fn demo')) < 1) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// `for … of` and `.map` say the same thing about the same array, and the
// reader knew only the first. `collaborators-contract-check.mjs` read its `.ts`
// client inside a `.map`, so the read was not counted, the floor could not
// notice a corpus that never joined, and seven regexes ran over raw bytes in
// silence (card_690786ca30ab).
runCase('a raw read bound by a .map callback parameter is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';
import path from 'node:path';

const clientPaths = [path.join('web/src/lib/api/demo.ts')];
const clients = clientPaths.map((file) => [file, readFileSync(file, 'utf8')]);

const failures = [];
const expect = (text, pattern, message) => {
  if (!pattern.test(text)) failures.push(message);
};

for (const [file, source] of clients) {
  if (!file.endsWith('.ts')) continue;
  expect(source, /export async function demo\\(/, path.relative('.', file) + ' must expose demo');
}
if (failures.length > 0) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_TYPESCRIPT: '1' },
  expect: { red: true, mentions: ['`source`', 'expect'], silent: ['`file`'] },
});

// The other half of the pair, and the reason the taint is carried by SLOT
// rather than by the tuple: slot 0 is the path the failure messages quote. A
// reader that tainted both would report `path.relative('.', file)` as an
// assertion over source bytes, which is a red nobody can clear.
runCase('the same .map read through productionTsSource is accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionTsSource } from './lib/ts-source.mjs';

const clientPaths = [path.join('web/src/lib/api/demo.ts')];
const clients = clientPaths.map((file) => [file, productionTsSource(readFileSync(file, 'utf8'))]);

const failures = [];
const expect = (text, pattern, message) => {
  if (!pattern.test(text)) failures.push(message);
};

for (const [file, source] of clients) {
  if (!file.endsWith('.ts')) continue;
  expect(source, /export async function demo\\(/, path.relative('.', file) + ' must expose demo');
}
if (failures.length > 0) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_TYPESCRIPT: '1' },
  expect: { red: false },
});

// The callback hands the bytes back whole, so there is no slot to wait for and
// the binding the chain lands in holds every guarded file in the directory.
// `openapi-route-coverage-contract-check.mjs` reads all of `rg-http` in exactly
// this shape — through `productionRustCode`, which is why it is green, and why
// nothing would have objected had it not been. The receiver is a filtered walk
// rather than a name, which is the half a reader that stopped at the first `)`
// would have called nothing at all.
runCase('a raw read returned whole from a .map over a filtered walk is rejected', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const SRC = 'crates/rg-demo/src';
const blob = readdirSync(SRC)
  .filter((name) => String(name).endsWith('.rs'))
  .map((name) => readFileSync(join(SRC, String(name)), 'utf8'))
  .join('\\n');
if (!blob.includes('pub async fn demo')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: true, mentions: ['`blob`', 'Rust source file'] },
});

// The same chain through the production view. This is the shape the tree
// actually ships, and it must stay green while still being COUNTED — a read the
// reader waves through without counting is the blind spot, not the fix.
runCase('the same walked .map through productionRustCode is accepted', {
  files: {
    'demo-contract-check.mjs': `import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import { productionRustCode } from './lib/rust-source.mjs';

const SRC = 'crates/rg-demo/src';
const blob = readdirSync(SRC)
  .filter((name) => String(name).endsWith('.rs'))
  .map((name) => productionRustCode(readFileSync(join(SRC, String(name)), 'utf8')))
  .join('\\n');
if (!blob.includes('pub async fn demo')) process.exit(1);
`,
  },
  // Four: the three `.rs` reads `scripts/lib/` contributes to every fixture, plus
  // the one this file adds. The number is what gives the case teeth — against a
  // reader that cannot see a `.map` element the read is not counted at all, and
  // a fixture that merely stayed green would not have told the two apart.
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '4' },
  expect: { red: false },
});

// The normalizer set is matched against CALL SITES, so a name in it launders on
// the strength of its spelling alone. Three cases hold that spelling to what it
// actually says, because the set of the live tree used to hold `text`, `code`,
// `src`, `rest`, `row`, `rows`, `routes` and `structure` — every one an
// ordinary JavaScript method or an ordinary local variable, and none of them a
// production view (card_90de966c222d).
//
// First: the qualifier at the call the read sits inside. Reading only the last
// segment of `response.text` laundered a raw read on the word `text`. The
// fixture spells `response` as a plain object because the shape under test is
// the spelling, not the transport — a check that pings a live endpoint and
// writes `response.text(…)` is the same three tokens.
runCase('a read wrapped in a qualified call sharing a normalizer name is rejected', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const response = { text: (bytes) => bytes };

const backend = response.text(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`backend`', 'Rust source file'] },
});

// Second: the same qualifier inside a local helper's body. A helper that reads
// a guarded file and mentions `response.text()` anywhere in the same function
// used to read as a helper that normalizes, so it never joined the raw readers
// and the bytes it handed back arrived at their assertion unwatched. That is
// the shape of every smoke check in this tree that also touches a source file.
runCase('a helper is not laundered by a qualified call sharing a normalizer name', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const response = { text: () => 'ok' };

function loadBackend(file) {
  if (response.text() !== 'ok') process.exit(1);
  return readFileSync(file, 'utf8');
}

const backend = loadBackend(backendPath);
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`backend`', 'Rust source file'] },
});

// Third: the other half of the same defect, and the half a qualifier lock alone
// does not reach. `code` reached the set as a plain local variable of
// `scripts/lib/rust-source.mjs` (`const code = productionRustCode(source)`),
// because "its initializer calls a known normalizer" was the only test applied.
// A value is not something a call site can hand bytes to, so the call here is
// BARE and still must not launder: only admitting callable bindings closes it.
runCase('a bare call to a local function named after a lib variable does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const code = (bytes) => bytes;

const backend = code(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`backend`'] },
});

// One word between the `=` and the read, and the binding stopped being watched.
// The reader followed the bytes through any number of call wrappers, but not
// through the `await` that a call to an async helper is spelled with — so the
// name never carried the taint and the assertion over it was never examined.
// Worse than blindness: the read still COUNTED towards the recognised-read
// floor, so the corpus looked seen and the gate went green having checked
// nothing. Every smoke check in this tree is async, so this is one keyword away
// from live.
runCase('an async reader reached through await does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
async function loadBackend(file) {
  return readFileSync(file, 'utf8');
}

const backend = await loadBackend(backendPath);
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`backend`', 'Rust source file'] },
});

// The same keyword one level in: the wrapper is awaited and the raw read is its
// argument. The wrapper chain is what the reader walks out through, so an
// `await` between two of its links has to be as invisible as whitespace.
runCase('an awaited wrapper around a raw read does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const wrap = async (bytes) => bytes;

const backend = await wrap(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`backend`'] },
});

// The other side of the same edit: `await` must not become a laundering word of
// its own. A read that goes through the production view is still accepted when
// the view is reached through an awaited helper.
runCase('an awaited helper that normalizes is still accepted', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/rust-source.mjs';

async function loadBackend(file) {
  return productionRustSource(readFileSync(file, 'utf8'));
}

const backend = await loadBackend(backendPath);
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// One harmless word between the bytes and the grep, and the accusation was
// dropped while the read still counted. `.trim()` is not a view of the program
// — it returns the same unparsed bytes with the ends clipped — but step 5 only
// ever looked for an assertion spelled DIRECTLY on the tainted name, so the
// chain hid the claim and the floor stayed satisfied. `X.trim().replace(…)` /
// `X.toLowerCase().includes(…)` is the native idiom of this tree, so this is
// one word away from live.
runCase('a passthrough method between the raw bytes and the grep does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const backend = readFileSync(backendPath, 'utf8');
if (!backend.trim().includes('path = "/demo"')) process.exit(1);
if (!backend.toLowerCase().includes('demo')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7', 'demo-contract-check.mjs:8', '`backend`'] },
});

// The array half of the same word. `paths.map(read)` taints the ARRAY, and
// `.join('\n')` is how a check spells "all of them at once" before greping the
// lot — a whole directory of guarded files welded into one string, asserted
// over raw, with nothing on the way that this reader used to look through.
runCase('a tainted array joined before the grep does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
const files = [backendPath];
const blobs = files.map((f) => readFileSync(f, 'utf8'));
if (!blobs.join('\\n').includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['`blobs`'] },
});

// The other side of the same edit: a passthrough chain must not become an
// accusation of its own. Clipping the whitespace off a value that already went
// through the production view says nothing about the bytes it was built from.
runCase('a passthrough chain over a normalized view is still accepted', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/rust-source.mjs';

const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
if (!backend.trim().toLowerCase().includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
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

// A name is not a behaviour, and this is the family that proves it. The set of
// laundering names used to be exactly that — names — closed over `scripts/lib/`
// and then matched against every call site in `scripts/`, so a check could
// write the word and be believed. All four cases below carry the SAME helper
// body, `return text;`, and differ only in what the name resolves to
// (card_11ee38c864dd; the Rust half of the ratchet paid for the identical hole
// one card earlier, card_e0f4ada65cee).
runCase('a local declaration under a view name does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
function productionRustCode(text) { return text; }

const backend = productionRustCode(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:8', '`productionRustCode()`'] },
});

// The same word with no declaration at all behind it. Nothing in `scripts/lib/`
// is called `nowhere.mjs`, and nothing used to ask: the name alone was the
// whole credential.
runCase('a view name imported from a module that does not exist does not launder', {
  files: {
    'demo-contract-check.mjs': `${PATHS}import { productionRustCode } from './lib/nowhere.mjs';

const backend = productionRustCode(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7', '`productionRustCode()`'] },
});

// The second axis of the same defect: a DERIVED name. `rustFnBlock` earns its
// place inside `rust-source.mjs` by calling `productionRustCode` there, and
// that used to make the word launder anywhere in `scripts/`, including over a
// same-named local helper that parses nothing. JavaScript resolves the bare
// call to the declaration this file makes, and so does the reader now.
runCase('a local declaration shadows a derived normalizer name', {
  files: {
    'demo-contract-check.mjs': `${PATHS}
function rustFnBlock(source) { return { body: source }; }

const fn = rustFnBlock(readFileSync(backendPath, 'utf8'), 'demo');
if (!fn.body.includes('RepoRead')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:8', '`rustFnBlock()`'] },
});

// Living in `scripts/lib/` is not the credential either: the seed names are the
// declarations `lib/rust-source.mjs` makes, and a second module writing one over
// a body that parses nothing is the impostor one directory closer to home.
runCase('a seed name declared by another lib module is not the view', {
  files: {
    'lib/demo-view.mjs': `export function productionRustCode(text) {
  return text;
}
`,
    'demo-contract-check.mjs': `${PATHS}import { productionRustCode } from './lib/demo-view.mjs';

const backend = productionRustCode(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  expect: { red: true, mentions: ['demo-contract-check.mjs:7', '`productionRustCode()`'] },
});

// The counter-danger, and the reason the rule is resolution rather than a
// filename. A shared module that delegates to the view IS the view, and a
// ratchet that reddened on the honest wrapper is one somebody deletes. The
// trust travels down the delegation, not down the name.
runCase('a wrapper inside scripts/lib that delegates to the view is the view', {
  files: {
    'lib/demo-view.mjs': `import { productionRustSource } from './rust-source.mjs';

export function demoView(source) {
  return productionRustSource(source);
}
`,
    'demo-contract-check.mjs': `${PATHS}import { demoView } from './lib/demo-view.mjs';

const backend = demoView(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

// A re-export is a delegation with the body left out. Both spellings have to
// carry the view on, or the first module that tidies its exports into a barrel
// turns the whole corpus red for a change that moved nothing.
runCase('a named re-export inside scripts/lib carries the view on', {
  files: {
    'lib/demo-view.mjs': "export { productionRustSource } from './rust-source.mjs';\n",
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/demo-view.mjs';

const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
  expect: { red: false },
});

runCase('a star re-export inside scripts/lib carries the view on', {
  files: {
    'lib/demo-view.mjs': "export * from './rust-source.mjs';\n",
    'demo-contract-check.mjs': `${PATHS}import { productionRustSource } from './lib/demo-view.mjs';

const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
if (!backend.includes('path = "/demo"')) process.exit(1);
`,
  },
  env: { FORGEKEEP_RAW_SOURCE_ASSERT_MIN_RUST: '1' },
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
