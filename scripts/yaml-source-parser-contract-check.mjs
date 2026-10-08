#!/usr/bin/env node

// Fixture for the YAML production view behind every textual `.yml` assertion in
// `scripts/`.
//
// The gate above it is only as good as this view: a check rewritten onto
// `productionYamlSource` still passes vacuously if the view hands back the raw
// bytes, and that is indistinguishable from a correct run by the check's own
// output. So every form that has produced — or would produce — a wrong verdict
// is pinned here, in both directions:
//
//   - a commented-out step must read as deleted (the false green this exists
//     for: `# run: node scripts/run-contract-checks.mjs` left four gates green,
//     card_fad8ad0ef007);
//   - a live step, a `#` that is not a comment opener, and a `#` inside a
//     quoted scalar must all survive (the false reds an over-reaching view
//     would invent);
//   - a `#` line inside a `run: |` body must survive, because block-scalar
//     content is document data a parser keeps, not a YAML comment.

import { spawnSync } from 'node:child_process';
import { readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parseYamlFile, selectYamlParser } from './lib/yaml-parser.mjs';
import { productionYamlSource, yamlAnnotatedLines } from './lib/yaml-source.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const failures = [];

function expect(condition, message) {
  if (!condition) failures.push(message);
}

const WORKFLOW = `# The contract checks are the cheapest gate in this workflow.
jobs:
  contract-checks:
    steps:
      - name: Run contract checks
        run: node scripts/run-contract-checks.mjs
      # - name: Run the retired checks
      #   run: node scripts/run-retired-checks.mjs
      - name: Install
        run: |
          # apt-get install commented-out-package
          sudo apt-get install -y ripgrep
      - name: Tag
        run: echo "prom/prometheus:v3#1"
      - name: 'a # not a comment'   # but this one is
        run: echo hello
`;

const view = productionYamlSource(WORKFLOW);

expect(view.length === WORKFLOW.length, 'productionYamlSource must stay byte-aligned with its input');
expect(view.split('\n').length === WORKFLOW.split('\n').length, 'productionYamlSource must preserve every newline');

// The defect itself: the live step survives, the commented-out one does not.
expect(
  /^\s*run: node scripts\/run-contract-checks\.mjs$/m.test(view),
  'the live step must survive the production view',
);
expect(
  !/run-retired-checks/.test(view),
  'a commented-out step must read as deleted — that is the false green this view exists to remove',
);
expect(
  !/Run the retired checks/.test(view),
  'a commented-out step name must read as deleted too',
);

// Block-scalar bodies are document data, not YAML comments. Blanking them would
// hide a shell body from every assertion written about it — the false-red twin
// of the defect above.
expect(
  /commented-out-package/.test(view),
  'a `#` line inside a `run: |` body is shell, not YAML — the view must leave block-scalar content alone',
);
expect(
  /sudo apt-get install -y ripgrep/.test(view),
  'a live line inside a block scalar must survive',
);

// A `#` is a comment opener only at line start or after whitespace.
expect(
  /prom\/prometheus:v3#1/.test(view),
  'a `#` with no whitespace before it is part of the value, not a comment opener',
);

// A `#` inside a quoted scalar is a value. Reading it as a comment would blank
// a live value and invent a red; missing the comment that follows the closing
// quote would keep the raw-bytes hole open on the same line.
expect(
  /'a # not a comment'/.test(view),
  'a `#` inside a quoted scalar must survive',
);
expect(
  !/but this one is/.test(view),
  'the comment after a quoted scalar must still be blanked',
);

// An apostrophe inside a plain scalar must not open a quoted scalar and swallow
// the comment that follows it.
const PLAIN = "note: it's fine # but the comment is still a comment\n";
expect(
  !/but the comment is still a comment/.test(productionYamlSource(PLAIN)),
  "an apostrophe inside a plain scalar must not hide the line's comment",
);
expect(
  /it's fine/.test(productionYamlSource(PLAIN)),
  'the plain scalar itself must survive',
);

// A plain scalar containing `>` is not a block-scalar header. Reading it as one
// would take every following line out of the view.
const ANGLE = `expr: rate(x[5m]) > 0.5
next: value # dropped
`;
expect(
  !/dropped/.test(productionYamlSource(ANGLE)),
  'a plain scalar containing `>` must not be read as a block-scalar header',
);

// The comment-bearing reader, for assertions that are *about* a marker comment.
const COMPOSE = `services:
  plombir-git:
    ports:
      - "8080:8080" # HTTP
      # - "9090:9090" # HTTP
  sidecar:
    ports:
      - "8080:8080"
`;
const marked = yamlAnnotatedLines(COMPOSE).filter(({ comment }) => comment === 'HTTP');
expect(marked.length === 1, `exactly one live line must carry the "# HTTP" marker, found ${marked.length}`);
expect(
  marked.length === 1 && /^\s*- "8080:8080"\s*$/.test(marked[0].code),
  'the code half of a marked line must hold the mapping without its comment',
);
expect(
  yamlAnnotatedLines(COMPOSE).length === COMPOSE.split('\n').length,
  'yamlAnnotatedLines must return one entry per line',
);
expect(
  yamlAnnotatedLines(COMPOSE).every(({ comment }) => comment === null || !comment.startsWith('#')),
  'a comment must be reported without its `#`',
);

// The property that makes the hand-written cases above more than a wish list:
// over EVERY `.yml` this repository ships, the view must parse into exactly the
// document the file parses into. A lexer that swallowed a block scalar, closed
// a quoted scalar early or blanked a `#` that was not a comment would change
// the document, and this says so on the real corpus rather than on a fixture
// written by the same hand that wrote the lexer.
const listed = spawnSync('git', ['ls-files', '*.yml', '*.yaml'], { cwd: root, encoding: 'utf8' });
if (listed.error || listed.status !== 0) {
  console.error(
    '❌ yaml production view: `git ls-files` could not enumerate the repository YAML — '
      + 'the corpus half of this check cannot run, and a check that cannot read its subject has not passed.',
  );
  process.exit(1);
}

const corpus = listed.stdout.split('\n').map((name) => name.trim()).filter(Boolean);
if (corpus.length === 0) {
  console.error('❌ yaml production view: the repository lists no *.yml/*.yaml — the corpus glob is broken, not the tree.');
  process.exit(1);
}

const { parser, missing } = selectYamlParser();
if (!parser) {
  console.error(
    `❌ yaml production view: no YAML parser available (tried ${missing.join(', ')}) — `
      + 'install either, or this check cannot prove the view is the same document as the file.',
  );
  process.exit(1);
}

const scratch = scratchDir(join(tmpdir(), 'plombir-git-yaml-view-'));
try {
  const viewPath = join(scratch, 'view.yml');
  for (const name of corpus) {
    const filePath = join(root, name);
    const source = readFileSync(filePath, 'utf8');
    const production = productionYamlSource(source);
    if (production.length !== source.length) {
      failures.push(`${name}: the production view is not byte-aligned with the file`);
      continue;
    }
    writeFileSync(viewPath, production);

    const before = parseYamlFile(parser, filePath);
    const after = parseYamlFile(parser, viewPath);
    if (!before.ok) {
      failures.push(`${name}: ${parser.name} cannot read the file itself — ${before.diagnostic ?? before.message}`);
      continue;
    }
    if (!after.ok) {
      failures.push(`${name}: the production view no longer parses — ${after.diagnostic ?? after.message}`);
      continue;
    }
    if (JSON.stringify(before.document) !== JSON.stringify(after.document)) {
      failures.push(`${name}: the production view parses into a different document than the file`);
    }
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

if (failures.length > 0) {
  console.error('❌ yaml production view:');
  for (const failure of failures) console.error(`   - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ yaml production view: comments read as deleted, block-scalar bodies and quoted \`#\` survive, `
    + `and all ${corpus.length} repository YAML file(s) parse identically through it`,
);
