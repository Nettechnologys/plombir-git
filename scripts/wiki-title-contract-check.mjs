#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { parseUtoipaPaths } from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const splitClientPath = path.join(root, 'web/src/lib/api/wiki.ts');
const wikiPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/wiki/[title]/+page.svelte');
const wikiHistoryPath = path.join(root, 'web/src/routes/[owner]/[repo]/wiki/[title]/history/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/wiki.rs');

const splitClient = productionTsSource(readFileSync(splitClientPath, 'utf8'));
const wikiPage = productionTsSource(readFileSync(wikiPagePath, 'utf8'));
const wikiHistory = productionTsSource(readFileSync(wikiHistoryPath, 'utf8'));
const backend = readFileSync(backendPath, 'utf8');
const failures = [];

function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

// Read the annotations once, then assert method and path *inside one row*.
//
// The bridge this replaces — `` `${method},[\s\S]*path = "${route}"` `` over the
// whole file — asserted neither. `[\s\S]*` walks out of the annotation it
// started in, so any `get,` above any `path = "…"` below satisfied it: the five
// routes could be spread over five unrelated annotations, or sit inside a
// comment, and the check still read as "this method serves this path"
// (card_64b6ede78939).
const annotations = parseUtoipaPaths(
  backend,
  'api::wiki',
  path.relative(root, backendPath).split(path.sep).join('/'),
);

for (const [method, route] of [
  ['get', '/repos/{owner}/{name}/wiki/{title}'],
  ['patch', '/repos/{owner}/{name}/wiki/{title}'],
  ['delete', '/repos/{owner}/{name}/wiki/{title}'],
  ['get', '/repos/{owner}/{name}/wiki/{title}/history'],
  ['get', '/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}'],
]) {
  const declared = annotations.some(
    (row) => row.method === method.toUpperCase() && row.path === route,
  );
  if (!declared) {
    failures.push(`Backend wiki ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

for (const [label, source] of [
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
