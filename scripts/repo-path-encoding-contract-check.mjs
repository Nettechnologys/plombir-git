#!/usr/bin/env node

// Browser API paths must not splice decoded route parameters into a URL.
import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { productionTsSource } from './lib/ts-source.mjs';

const dir = resolve(process.env.REPO_PATH_CLIENT_DIR || 'web/src/lib/api');
const failures = [];
for (const file of readdirSync(dir).filter((name) => name.endsWith('.ts') && !name.endsWith('.test.ts'))) {
  const source = productionTsSource(readFileSync(resolve(dir, file), 'utf8'));
  for (const match of source.matchAll(/`(?:\\[\s\S]|[^`])*`/g)) {
    const literal = match[0];
    const rawOwnerOrRepo = /\$\{\s*(?:owner|repo)\s*\}/.test(literal);
    const rawRepoName = literal.includes('/repos/') && /\$\{\s*name\s*\}/.test(literal);
    const rawOrgName = /\/(?:admin\/)?orgs\//.test(literal) && /\$\{\s*name\s*\}/.test(literal);
    if (!rawOwnerOrRepo && !rawRepoName && !rawOrgName) continue;
    const line = source.slice(0, match.index).split('\n').length;
    failures.push(`${file}:${line}: raw owner/repo/organization segment in API path`);
  }
}

if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
console.log('repository API path segments are encoded');
