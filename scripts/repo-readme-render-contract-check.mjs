#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const repoPage = readFileSync(repoPagePath, 'utf8');
const failures = [];

if (!/function\s+escapeHtml\s*\(\s*value:\s*string\s*\)/.test(repoPage)) {
  failures.push('Repository README renderer must escape backend blob content before HTML insertion');
}

if (!/function\s+safeMarkdownHref\s*\(\s*value:\s*string\s*\)/.test(repoPage)) {
  failures.push('Repository README renderer must validate markdown link hrefs before HTML insertion');
}

if (!/function\s+renderInlineMarkdown\s*\(\s*line:\s*string\s*\)/.test(repoPage)) {
  failures.push('Repository README renderer must centralize inline markdown rendering');
}

const renderBlock = repoPage.match(/function\s+renderInlineMarkdown\s*\(\s*line:\s*string\s*\)\s*:\s*string\s*\{[\s\S]*?\n\s*\}/)?.[0] || '';

if (!/escapeHtml\(line\)/.test(renderBlock)) {
  failures.push('renderInlineMarkdown must start from escaped README text');
}

if (!/safeMarkdownHref\(href\)/.test(renderBlock)) {
  failures.push('renderInlineMarkdown must sanitize markdown link hrefs');
}

if (/\{@html\s+line\s*(?:\.|\})/.test(repoPage)) {
  failures.push('Repository page must not inject raw README lines with {@html line...}');
}

if (!/\{@html\s+renderInlineMarkdown\(line\)\}/.test(repoPage)) {
  failures.push('Repository page must render README lines through renderInlineMarkdown');
}

if (failures.length > 0) {
  console.error('Repository README render frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Repository README render frontend/backend contract ok');
