#!/usr/bin/env node

// Guards the three assumptions that make the markdown sanitizer's design safe.
// Each of them has already failed once:
//
//   1. The suite covering it must actually be RUN by CI. The previous guard sat
//      in package.json unreferenced by any workflow (card_21a23bc24a0f).
//   2. It must run against a DOM. The guard that replaced it loaded the module
//      in bare Node, so it asserted a regex fallback no browser ever executed
//      while the real code path went untested (card_dd1b9ed2ad70).
//   3. There must be exactly ONE sanitizer implementation. The deleted regex
//      fallback let `javascript&#58;alert(1)` keep its href, because it
//      validated attribute text before HTML entities were decoded
//      (card_2fc2a934926a). Two implementations of one security boundary means
//      the weaker one is the one that eventually ships.
//
// Assumption 3 is only sound while the app is browser-only, which is why the
// `ssr`/`prerender` flags are checked here too: turning on server rendering is
// allowed, but it must not silently leave `renderMarkdown` without a DOM.

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionTsSource } from './lib/ts-source.mjs';
import { parseWorkflowFile, selectWorkflowParser, workflowJobRuns } from './lib/workflow.mjs';

const root = process.cwd();

const read = (relative) => readFileSync(path.join(root, relative), 'utf8');

const markdown = productionTsSource(read('web/src/lib/utils/markdown.ts'));
const tests = productionTsSource(read('web/src/lib/utils/markdown.test.ts'));
const vitestConfig = productionTsSource(read('web/vitest.config.ts'));
const packageJson = JSON.parse(read('web/package.json'));
const layout = productionTsSource(read('web/src/routes/+layout.ts'));

const failures = [];

function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

function reject(source, pattern, message) {
  if (pattern.test(source)) failures.push(message);
}

// 1. The suite is wired all the way into CI.
if (typeof packageJson.scripts?.test !== 'string') {
  failures.push('web/package.json must define a `test` script');
}

// Asked of the parsed job graph, not of the file's bytes. `# run: npm test`
// satisfies a grep for the line and executes nothing, which is the same false
// green assumption 1 already survived once — and the one that let a single `#`
// in front of `run: node scripts/run-contract-checks.mjs` leave four gates,
// this one included, reporting a healthy workflow (card_fad8ad0ef007).
const { parser, missing } = selectWorkflowParser();
if (!parser) {
  console.error(
    `No YAML parser available (tried ${missing.join(', ')}) — install either, or this check cannot `
      + 'tell a workflow that runs the sanitizer suite from one that only mentions it.',
  );
  process.exit(1);
}

const workflowPath = path.join(root, '.github/workflows/regression.yml');
const workflow = parseWorkflowFile(parser, workflowPath);
if (!workflow.ok) {
  console.error(
    `.github/workflows/regression.yml did not parse under ${parser.name} — `
      + `${workflow.diagnostic ?? workflow.message}`,
  );
  process.exit(1);
}

const frontendJob = workflow.jobs?.frontend;
if (!frontendJob) {
  failures.push(
    '.github/workflows/regression.yml declares no `frontend` job — the sanitizer suite is run by nothing',
  );
} else {
  const inspected = workflowJobRuns(frontendJob);
  if (!inspected.ok) {
    failures.push(
      '.github/workflows/regression.yml `frontend` job has a step whose `run` is not a shell command — '
        + 'the job cannot be read, so nothing here proves the suite runs',
    );
  } else if (!inspected.runs.some((run) => run.trim() === 'npm test')) {
    failures.push(
      'The frontend job in .github/workflows/regression.yml must run `npm test` — '
        + 'a guard no workflow invokes is a comment, not a gate',
    );
  }
}

// 2. It runs against a DOM, and the module refuses to work without one.
expect(
  vitestConfig,
  /environment:\s*'jsdom'/,
  "web/vitest.config.ts must set environment: 'jsdom' — without a DOM the sanitizer tests cannot exercise the browser code path",
);
expect(
  markdown,
  /typeof DOMParser === 'undefined'/,
  'sanitizeHtml must check for a DOM before sanitizing',
);
expect(
  markdown,
  /throw new Error\(\s*\n?\s*'sanitizeHtml requires a DOM/,
  'sanitizeHtml must throw when no DOM is available, so a DOM-less runtime fails loudly instead of taking a weaker path',
);
expect(
  tests,
  /toThrow\(\/requires a DOM\//,
  'markdown.test.ts must assert that sanitizeHtml throws without a DOM — that assertion is what proves which implementation the suite exercises',
);

// 3. Exactly one implementation, and no regex-based attribute filtering.
reject(
  markdown,
  /stripDangerousAttributes/,
  'The regex sanitizer fallback must stay deleted — it shipped a `javascript&#58;` bypass (card_2fc2a934926a)',
);
reject(
  markdown,
  /typeof DOMParser !== 'undefined'/,
  'sanitizeHtml must not branch on DOM availability to pick between two sanitizer implementations (card_dd1b9ed2ad70)',
);
expect(
  markdown,
  /MAX_SANITIZE_PASSES/,
  'sanitizeHtml must re-sanitize to a fixpoint; a single pass cannot detect markup that reparses into something else (mXSS)',
);

// The browser-only assumption behind assumption 3.
expect(
  layout,
  /export const ssr = false/,
  'web/src/routes/+layout.ts no longer sets `ssr = false`: server rendering would call sanitizeHtml without a DOM. Give it a real DOM before enabling SSR — do not add a second sanitizer.',
);
expect(
  layout,
  /export const prerender = false/,
  'web/src/routes/+layout.ts no longer sets `prerender = false`: prerendering would call sanitizeHtml without a DOM. Give it a real DOM before enabling prerendering — do not add a second sanitizer.',
);

if (failures.length > 0) {
  console.error('Markdown sanitizer contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Markdown sanitizer contract ok');
