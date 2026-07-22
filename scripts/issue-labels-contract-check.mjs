#!/usr/bin/env node

import { readFileSync } from 'node:fs';

const checks = [];

function read(path) {
  return readFileSync(path, 'utf8');
}

function check(condition, message) {
  checks.push({ ok: Boolean(condition), message });
}

const backendIssueEntity = read('crates/rg-db/src/entities/issue.rs');
const mainClient = read('web/src/lib/api/client.svelte.ts');
const splitClient = read('web/src/lib/api/issues.ts');
const issuesListPage = read('web/src/routes/[owner]/[repo]/issues/+page.svelte');
const issueDetailPage = read('web/src/routes/[owner]/[repo]/issues/[number]/+page.svelte');
const enTranslations = JSON.parse(read('web/src/lib/i18n/translations/en.json'));
const zhTranslations = JSON.parse(read('web/src/lib/i18n/translations/zh-CN.json'));

check(
  /pub labels:\s*Option<String>/.test(backendIssueEntity),
  'backend issue entity stores labels as an optional JSON string',
);

for (const [name, source] of [
  ['client.svelte.ts', mainClient],
  ['issues.ts', splitClient],
]) {
  check(
    /function parseIssueLabels/.test(source) && /JSON\.parse\(labels\)/.test(source),
    `${name} parses JSON-string issue labels`,
  );
  check(
    /function normalizeIssue/.test(source) && /labels:\s*parseIssueLabels\(issue\.labels\)/.test(source),
    `${name} normalizes issue labels to arrays`,
  );
  check(
    /data:\s*response\.data\.map\(normalizeIssue\)/.test(source),
    `${name} normalizes paginated issue list data`,
  );
  check(
    /issues\/\$\{number\}`\)\.then\(normalizeIssue\)/.test(source),
    `${name} normalizes issue detail data`,
  );
}

check(
  /issue\.labels\?\.length/.test(issuesListPage) && /\{#each issue\.labels as label\}/.test(issuesListPage),
  'issue list page renders issue.labels as an iterable array',
);
check(
  /t\('issues\.meta',\s*\{\s*number:\s*issue\.number/.test(issuesListPage)
    && enTranslations.issues?.meta?.includes('#{number}')
    && zhTranslations.issues?.meta?.includes('#{number}')
    && !/#\$\{issue\.number\}/.test(issuesListPage)
    && !enTranslations.issues?.meta?.includes('#${number}')
    && !zhTranslations.issues?.meta?.includes('#${number}'),
  'issue list page renders translated issue numbers without a stray dollar sign',
);
check(
  /issue\.labels\?\.length/.test(issueDetailPage) && /\{#each issue\.labels as label\}/.test(issueDetailPage),
  'issue detail page renders issue.labels as an iterable array',
);

const failures = checks.filter((item) => !item.ok);
if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`❌ ${failure.message}`);
  }
  console.error(`\n${failures.length} issue label contract check(s) failed`);
  process.exit(1);
}

console.log(`Issue label frontend/backend contract ok (${checks.length} checks)`);
