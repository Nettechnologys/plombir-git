#!/usr/bin/env node

import { existsSync, readFileSync } from 'node:fs';

const commitsPagePath = 'web/src/routes/[owner]/[repo]/commits/+page.svelte';
const commitDetailPagePath = 'web/src/routes/[owner]/[repo]/commits/[sha]/+page.svelte';

const commitsPage = readFileSync(commitsPagePath, 'utf8');
const commitDetailPage = readFileSync(commitDetailPagePath, 'utf8');

const failures = [];

if (!existsSync(commitDetailPagePath) || !commitDetailPage.includes('$page.params')) {
  failures.push('Commit detail page must exist at /:owner/:repo/commits/:sha');
}

if (/href="\/\{owner\}\/\{repo\}\/commit\/\{commit\.sha\}"/.test(commitsPage)) {
  failures.push('Commits list must not link to absent singular /commit/:sha route');
}

const hasCommitDetailLink =
  /href="\/\{owner\}\/\{repo\}\/commits\/\{commit\.sha\}"/.test(commitsPage) ||
  /href=\{`\/\$\{owner\}\/\$\{repo\}\/commits\/\$\{commit\.sha\}`\}/.test(commitsPage);

if (!hasCommitDetailLink) {
  failures.push('Commits list must link each commit to /:owner/:repo/commits/:sha');
}

if (!commitsPage.includes('let owner = $derived($page.params.owner!)')) {
  failures.push('Commits page must treat owner route param as required before calling repos.log');
}

if (!commitsPage.includes('let repo = $derived($page.params.repo!)')) {
  failures.push('Commits page must treat repo route param as required before calling repos.log');
}

if (/console\.(log|error|warn)\(/.test(commitsPage)) {
  failures.push('Commits page must not leak backend request diagnostics to the browser console');
}

if (failures.length > 0) {
  console.error('Commit links frontend route contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Commit links frontend route contract ok');
