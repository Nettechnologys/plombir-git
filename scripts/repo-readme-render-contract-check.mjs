#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { productionTsSource } from './lib/ts-source.mjs';

// The repository page renders the README from a blob the backend hands over
// verbatim, so it is untrusted HTML input. It used to be rendered line by line
// by a page-local helper that escaped the text itself — safe, but not Markdown:
// fenced code came out as rules and paragraphs, and **bold** or `code` inside a
// list item stayed raw. It now goes through the shared renderMarkdown, which
// sanitizes what it renders, the same path the file view, issues and pull
// requests use. This contract holds the page to that path.

const root = process.cwd();
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const repoPage = productionTsSource(readFileSync(repoPagePath, 'utf8'));

const failures = [];

if (!/import\s*\{[^}]*\brenderMarkdown\b[^}]*\}\s*from\s*['"]\$lib\/utils\/markdown['"]/.test(repoPage)) {
  failures.push('Repository page must render the README with the shared renderMarkdown from $lib/utils/markdown');
}

if (!/renderMarkdown\(\s*readmeContent\s*\)/.test(repoPage)) {
  failures.push('Repository page must pass the README content through renderMarkdown');
}

const htmlSinks = [...repoPage.matchAll(/\{@html\s+([^}]+)\}/g)].map((match) => match[1].trim());
for (const sink of htmlSinks) {
  if (sink !== 'renderedReadme') {
    failures.push(`Repository page injects {@html ${sink}}; only the sanitized renderedReadme may reach {@html}`);
  }
}
if (!htmlSinks.includes('renderedReadme')) {
  failures.push('Repository page must render the README through {@html renderedReadme}');
}

if (!/renderedReadme\s*=\s*\$derived\([^;]*renderMarkdown\(\s*readmeContent\s*\)/.test(repoPage)) {
  failures.push('renderedReadme must be derived from renderMarkdown(readmeContent)');
}

if (failures.length > 0) {
  console.error('Repository README render frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Repository README render frontend/backend contract ok');
