#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const files = execFileSync('rg', ['--files', 'web/src'], { cwd: root, encoding: 'utf8' })
  .split('\n')
  .filter(Boolean);
const svelteFiles = files.filter((file) => file.endsWith('.svelte'));
const routeFiles = files.filter((file) => file.startsWith('web/src/routes/') && file.endsWith('/+page.svelte'));
const repoHeaderPath = 'web/src/lib/components/RepoHeader.svelte';
const repoHeader = readFileSync(resolve(root, repoHeaderPath), 'utf8');

const failures = [];
const literalDynamicHref = /\bhref="[^"]*\{[^"]*"/g;
const literalHref = /\bhref="([^"{]+)"/g;
const activeTabProp = /<RepoHeader\b[^>]*\bactiveTab="([^"]+)"/g;

function escapeRegex(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function routeFileToRegex(file) {
  const routeDir = dirname(file).replace(/^web\/src\/routes/, '');
  if (!routeDir || routeDir === '.') return /^\/$/;

  const parts = routeDir.split('/').filter(Boolean);
  const pattern = parts
    .map((part) => {
      if (/^\[\.\.\.[^\]]+\]$/.test(part)) return '.+';
      if (/^\[[^\]]+\]$/.test(part)) return '[^/]+';
      return escapeRegex(part);
    })
    .join('/');

  return new RegExp(`^/${pattern}$`);
}

const routePatterns = routeFiles.map(routeFileToRegex);
const repoHeaderTabIds = new Set(
  [...repoHeader.matchAll(/\{\s*id:\s*'([^']+)'/g)].map((match) => match[1]),
);

function isInternalHref(href) {
  return href.startsWith('/')
    && !href.startsWith('//')
    && !href.includes('{')
    && !href.includes('*');
}

function normalizePath(href) {
  return href.split(/[?#]/, 1)[0].replace(/\/+$/, '') || '/';
}

function hasRoute(path) {
  return routePatterns.some((pattern) => pattern.test(path));
}

for (const file of svelteFiles) {
  const source = readFileSync(resolve(root, file), 'utf8');
  for (const match of source.matchAll(literalDynamicHref)) {
    failures.push(`${relative(root, file)}: static href contains Svelte placeholders: ${match[0]}`);
  }
  for (const match of source.matchAll(literalHref)) {
    const href = match[1];
    if (!isInternalHref(href)) continue;
    const path = normalizePath(href);
    if (!hasRoute(path)) {
      failures.push(`${relative(root, file)}: literal internal href has no Svelte route: ${href}`);
    }
  }
  for (const match of source.matchAll(activeTabProp)) {
    const activeTab = match[1];
    if (!repoHeaderTabIds.has(activeTab)) {
      failures.push(`${relative(root, file)}: RepoHeader activeTab="${activeTab}" does not match a RepoHeader tab id`);
    }
  }
}

if (failures.length > 0) {
  console.error('Frontend href contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Frontend href contract ok');
