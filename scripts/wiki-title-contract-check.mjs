#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const splitClientPath = path.join(root, 'web/src/lib/api/wiki.ts');
const wikiPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/wiki/[title]/+page.svelte');
const wikiHistoryPath = path.join(root, 'web/src/routes/[owner]/[repo]/wiki/[title]/history/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/wiki.rs');

const client = readFileSync(clientPath, 'utf8');
const splitClient = readFileSync(splitClientPath, 'utf8');
const wikiPage = readFileSync(wikiPagePath, 'utf8');
const wikiHistory = readFileSync(wikiHistoryPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const failures = [];

function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

for (const [method, route] of [
  ['get', '/repos/{owner}/{name}/wiki/{title}'],
  ['patch', '/repos/{owner}/{name}/wiki/{title}'],
  ['delete', '/repos/{owner}/{name}/wiki/{title}'],
  ['get', '/repos/{owner}/{name}/wiki/{title}/history'],
  ['get', '/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}'],
]) {
  const pattern = new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`);
  expect(backend, pattern, `Backend wiki ${method.toUpperCase()} ${route} annotation is missing or changed`);
}

for (const [label, source] of [
  ['main client', client],
  ['split wiki client', splitClient],
]) {
  expect(
    source,
    /wiki\/\$\{encodeURIComponent\(title\)\}`/,
    `${label} must encode wiki title for get/update/delete route calls`,
  );
  expect(
    source,
    /wiki\/\$\{encodeURIComponent\(title\)\}\/history`/,
    `${label} must encode wiki title for history route calls`,
  );
  expect(
    source,
    /wiki\/\$\{encodeURIComponent\(title\)\}\/revisions\/\$\{revId\}`/,
    `${label} must encode wiki title for revision route calls`,
  );

  if (/wiki\/\$\{title\}(?:`|\/)/.test(source)) {
    failures.push(`${label} must not interpolate raw wiki titles into route path segments`);
  }
}

for (const [label, source] of [
  ['wiki page route', wikiPage],
  ['wiki history route', wikiHistory],
]) {
  if (/decodeURIComponent\(\$page\.params\.title!\)/.test(source)) {
    failures.push(`${label} must not decode SvelteKit title params a second time`);
  }
}

expect(
  wikiHistory,
  /wiki\/\$\{encodeURIComponent\(title\)\}`/,
  'Wiki history breadcrumb must encode the title when linking back to the page',
);

if (failures.length > 0) {
  console.error('Wiki title frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Wiki title frontend/backend contract ok');
