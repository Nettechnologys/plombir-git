#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import {
  loadRouteTable,
  parseUtoipaPaths,
  productionRustSource,
  routeFailures,
  rustFnBlock,
  rustStructBody,
} from './lib/rust-source.mjs';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/imports.rs');
const entityPath = path.join(root, 'crates/rg-db/src/entities/import_task.rs');
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');
const clientPath = path.join(root, 'web/src/lib/api/imports.ts');
const navbarPath = path.join(root, 'web/src/lib/components/Navbar.svelte');
const pagePath = path.join(root, 'web/src/routes/imports/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const entity = readFileSync(entityPath, 'utf8');
const routes = loadRouteTable(routerPath);
const client = readFileSync(clientPath, 'utf8');
const navbar = readFileSync(navbarPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const failures = [];

// Two of these four routes share a path and differ only by method, which the
// bridge they replace could not tell apart: `` `${method},[\s\S]*path = "${route}"` ``
// matched a method from one annotation against a path from another, and matched
// both inside a comment (card_64b6ede78939). Parse once, assert within a row.
const annotations = parseUtoipaPaths(
  backend,
  'api::imports',
  path.relative(root, backendPath).split(path.sep).join('/'),
);

for (const [method, route] of [
  ['post', '/imports'],
  ['get', '/imports'],
  ['get', '/imports/{id}'],
  ['delete', '/imports/{id}'],
]) {
  const declared = annotations.some(
    (row) => row.method === method.toUpperCase() && row.path === route,
  );
  if (!declared) {
    failures.push(`Backend import ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

// Imports are per-user tasks carrying source credentials — every route is
// authenticated, none of them is `Public`.
failures.push(
  ...routeFailures(routes, [
    { method: 'POST', path: '/imports', handler: 'api::imports::start_import', access: 'User' },
    { method: 'GET', path: '/imports', handler: 'api::imports::list_imports', access: 'User' },
    { method: 'GET', path: '/imports/{id}', handler: 'api::imports::get_import_status', access: 'User' },
    { method: 'DELETE', path: '/imports/{id}', handler: 'api::imports::delete_import', access: 'User' },
  ]),
);

// ── The user gate and the per-user scoping of `{id}` ──────────────────────
//
// `{id}` is a global `import_tasks` primary key and the only gate on these two
// routes is `AuthUser`, so each owes a second refusal beyond "is authenticated":
// a task somebody else started. An import task carries the source-forge auth
// token it was started with, so that is a credential read, not a metadata read.
//
// Authentication is the extractor, not a hand-rolled header read — these used
// to spell out `headers: HeaderMap` + `extract_bearer_claims`, which is how the
// handlers were written before the extractor migration, and the check went on
// failing long after the handlers were correct (sol_a8174e6f3b09).
//
// That fix pinned the *next* form instead of the behaviour, and it rotted the
// same way — silently, and in the dangerous direction:
//
//   /pub async fn get_import_status\([\s\S]*task\.user_id == user_id/
//
// `[\s\S]*` spans the whole module, so when the comparison moved out of both
// handlers into the private `import_task_of_user` — which sits *below* them —
// the regex kept matching by reaching past the handler it names into the
// helper. Both asserts went VACUOUS: deleting the scoping call from
// `get_import_status` outright, turning it into a cross-account read, still
// passed. Handler-scoped from here, so a file-wide match cannot stand in for
// the door being asserted (card_4548d995d90c).
//
// The production view: comments and `#[cfg(test)]` items alike are blanked, so
// a commented-out handler reads as a deleted one and a test double cannot
// stand in for the handler the server ships.
const backendCode = productionRustSource(backend);

// The module-private `async fn`s of imports.rs — that is what a handler calls
// to re-anchor a task to an account.
const anchors = [...backendCode.matchAll(/^async fn (\w+)/gm)].map((m) => m[1]);
if (anchors.length === 0) {
  failures.push(
    'This check can no longer read the private helpers of api/imports.rs that it derives the ' +
      'scoping assertions from, so their verdicts below mean nothing. Fix the parsing, not the ' +
      'handlers.',
  );
}

for (const [handler, route] of [
  ['get_import_status', 'GET /imports/{id}'],
  ['delete_import', 'DELETE /imports/{id}'],
]) {
  const fn = rustFnBlock(backendCode, handler);
  if (fn === null) {
    failures.push(`api/imports.rs no longer defines a \`pub async fn ${handler}\` this check can read`);
    continue;
  }

  // Both bindings are read out of the signature rather than assumed, so
  // renaming either is not a contract change.
  const actor = /AuthUser\((\w+)\)\s*:\s*AuthUser/.exec(fn.params);
  if (actor === null) {
    failures.push(`${route} must authenticate: handler ${handler} does not take the AuthUser extractor`);
  }
  const taskId = /Path\((\w+)\)\s*:\s*Path<i64>/.exec(fn.params);
  if (taskId === null) {
    failures.push(`${route} must destructure Path<i64>: handler ${handler} does not`);
  }
  if (actor === null || taskId === null || anchors.length === 0) {
    continue;
  }

  // Either spelling counts — the named helper `global_id_anchor_guard::ANCHORED`
  // requires, or the inline comparison it wraps. What is asserted is that *this*
  // handler ties *its* task id to *its* authenticated actor, which is what both
  // spellings mean and neither symbol name does.
  const scoped =
    anchors.some((helper) =>
      new RegExp(`\\b${helper}\\(\\s*&state(?:\\.db)?\\s*,\\s*${actor[1]}\\s*,\\s*${taskId[1]}\\s*\\)`).test(fn.body),
    ) || new RegExp(`\\.user_id\\s*==\\s*${actor[1]}\\b`).test(fn.body);
  if (!scoped) {
    failures.push(
      `${route} must anchor task id \`${taskId[1]}\` to the authenticated account \`${actor[1]}\`: ` +
        `neither \`<helper>(&state, ${actor[1]}, ${taskId[1]})\` nor an inline ` +
        `\`task.user_id == ${actor[1]}\` is present in ${handler}. \`{id}\` is a global import_tasks ` +
        'primary key, and the row carries the source-forge auth token the import was started with',
    );
  }
}

// card_e736b5186281: the target namespace arrives in the *body*, so no
// path-based extractor can gate it and the route table's `User` cannot state
// it. `NamespaceWrite` is the gate; without it any authenticated user may
// import into somebody else's `owner/name`. Scoped to the handler for the same
// reason as the two above — file-wide, this matched any module that mentions
// the gate anywhere.
const startImport = rustFnBlock(backendCode, 'start_import');
if (startImport === null) {
  failures.push('api/imports.rs no longer defines a `pub async fn start_import` this check can read');
} else if (!/NamespaceWrite\s*\{[\s\S]*\}\s*:\s*NamespaceWrite<StartImportRequest>/.test(startImport.params)) {
  failures.push('POST /imports must take the NamespaceWrite gate over its target_owner');
}

for (const [name, pattern] of [
  ['list', /list:\s*\(\)\s*=>\s*\n\s*request<ImportTask\[]>\('\/imports'\)/],
  ['start', /start:\s*\(payload:[\s\S]*request<ImportTask>\('\/imports'[\s\S]*method:\s*'POST'/],
  ['get', /get:\s*\(id: number\)\s*=>\s*\n\s*request<ImportTask>\(`\/imports\/\$\{id\}`\)/],
  ['remove', /remove:\s*\(id: number\)\s*=>\s*\n\s*request<void>\(`\/imports\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'\s*\}\)/],
]) {
  if (!pattern.test(client)) {
    failures.push(`API client must expose imports.${name} with the backend route`);
  }
}

for (const field of [
  'platform',
  'source_url',
  'target_owner',
  'target_name',
  'auth_token',
  'import_repo',
  'import_issues',
  'import_pull_requests',
  'import_wiki',
  'import_releases',
  'import_labels',
  'import_milestones',
]) {
  if (!client.includes(field) || !page.includes(field)) {
    failures.push(`Import frontend must preserve backend field ${field}`);
  }
}

// ── Every option the form offers must have a consumer ─────────────────────
//
// `import_wiki` travelled from the checkbox to its own database column and
// stopped there: `start_import` wrote it, and not one runner ever read it
// (card_20cf2efd80c4). The symmetry checks above asserted that field along its
// whole path and passed the entire time, because every hop they know about was
// present — the missing one was the *consumer*, which no frontend/backend
// symmetry can see. The user ticked the box, the task reached `completed`, and
// the status said `wiki_pages_imported: 0`, which is exactly what a source with
// no wiki says.
//
// So the option columns are read out of the entity and each is required to be
// read back as `task.<field>` by the import pipeline. Derived rather than
// listed: an option column added tomorrow is covered the day it is added, and
// the writer in `start_import` (a bare `import_wiki:` parameter, not
// `task.import_wiki`) cannot stand in for a reader.
const servicePath = path.join(root, 'crates/rg-core/src/import/service.rs');
const service = productionRustSource(readFileSync(servicePath, 'utf8'));
const optionColumns = [...productionRustSource(entity).matchAll(/^\s*pub (import_\w+):\s*bool/gm)].map((m) => m[1]);

if (optionColumns.length === 0) {
  failures.push(
    'This check can no longer read the `import_*` option columns off import_task.rs, so the ' +
      'consumer assertions below mean nothing. Fix the parsing, not the entity.',
  );
}

for (const option of optionColumns) {
  if (!new RegExp(`\\btask\\.${option}\\b`).test(service)) {
    failures.push(
      `import_tasks.${option} is written by start_import and read by no runner in ` +
        'import/service.rs — the option would reach the database and have no effect, which a ' +
        'user cannot tell from a source that had nothing to import',
    );
  }
}

for (const field of ['repo_id', 'stage', 'error', 'stats']) {
  if (!client.includes(field)) {
    failures.push(`ImportTask client model must expose backend field ${field}`);
  }
}

for (const field of ['task.error', 'task.stage']) {
  if (!page.includes(field)) {
    failures.push(`Imports page must render ${field}`);
  }
}

if (client.includes('error_message') || page.includes('error_message')) {
  failures.push('Import frontend must render backend ImportTask.error, not non-existent error_message');
}

// Asserted inside the entity's own `Model`, out of the executable view: a
// file-wide `entity.includes('pub stage: Option<String>')` was satisfied by a
// commented-out column and by a field of any neighbouring struct, so the
// frontend could keep depending on a column the table no longer has
// (card_64b6ede78939).
const importTaskFields = rustStructBody(entity, 'Model');
if (importTaskFields === null) {
  failures.push('Backend import task `Model` struct could not be read, so its column contract is unverified');
} else {
  for (const backendField of ['pub repo_id: Option<i64>', 'pub stage: Option<String>', 'pub error: Option<String>', 'pub stats: Option<String>']) {
    if (!importTaskFields.includes(backendField)) {
      failures.push(`Backend import task contract check could not find ${backendField}`);
    }
  }
}

if (!navbar.includes('href="/imports"')) {
  failures.push('Authenticated navbar must expose the imports page');
}

for (const call of ['imports.list(', 'imports.start(', 'imports.remove(']) {
  if (!page.includes(call)) {
    failures.push(`Imports page must call ${call}`);
  }
}

if (!/goto\('\/login'\)/.test(page)) {
  failures.push('Imports page must redirect unauthenticated users before calling user-scoped APIs');
}

if (failures.length > 0) {
  console.error('Imports frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Imports frontend/backend contract ok');
