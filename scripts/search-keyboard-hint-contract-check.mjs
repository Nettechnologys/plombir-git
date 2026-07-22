#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const pagePath = path.join(root, 'web/src/routes/search/+page.svelte');
const page = readFileSync(pagePath, 'utf8');
const failures = [];

if (!/function\s+keyboardHintParts\s*\(/.test(page)) {
  failures.push('Search page must split the localized keyboard hint in component code');
}

if (!/<kbd>\{[^}]*shortcut[^}]*\}<\/kbd>/.test(page)) {
  failures.push('Search page must render the Ctrl+K shortcut with a real <kbd> element');
}

if (/\{@html\s+t\('search\.keyboard_hint'/.test(page)) {
  failures.push('Search page must not inject the localized keyboard hint as HTML');
}

if (/\.keyboard-hint\s+kbd/.test(page) && !/<kbd>/.test(page)) {
  failures.push('Search page keyboard hint CSS must be backed by real markup');
}

if (!/let\s+searchError\s*=/.test(page)) {
  failures.push('Search page must keep backend search failures in explicit state');
}

if (!/searchError\s*=\s*err\?\./.test(page)) {
  failures.push('Search page must surface backend search errors instead of swallowing them');
}

if (!/\{:else if searchError\}/.test(page)) {
  failures.push('Search page must render search errors before the empty-results state');
}

if (/\}\s*catch\s*\([^)]*\)\s*\{\s*results\s*=\s*\[\];\s*total\s*=\s*0;\s*\}/s.test(page)) {
  failures.push('Search page must not collapse failed searches into the no-results state');
}

if (!/function\s+normalizeSearchType\s*\(\s*type:\s*string\s*\|\s*null\s*\)/.test(page)) {
  failures.push('Search page must normalize type query parameters before calling the backend');
}

if (!/type\s*===\s*'repo'\)\s*return\s*'repos'/.test(page)) {
  failures.push('Search page must keep old type=repo URLs working by mapping to repos');
}

if (!/type\s*===\s*'issue'\)\s*return\s*'issues'/.test(page)) {
  failures.push('Search page must keep old type=issue URLs working by mapping to issues');
}

if (/\{\s*key:\s*'repo'/.test(page) || /\{\s*key:\s*'issue'/.test(page)) {
  failures.push('Search tabs must emit canonical backend type values: repos/issues');
}

if (!/\{\s*key:\s*'repos'/.test(page) || !/\{\s*key:\s*'issues'/.test(page)) {
  failures.push('Search tabs must include canonical repos and issues filters');
}

if (failures.length > 0) {
  console.error('Search keyboard hint contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Search keyboard hint contract ok');
