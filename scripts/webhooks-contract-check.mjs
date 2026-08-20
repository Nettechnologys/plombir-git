#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import {
  loadRouteTable,
  productionRustCode,
  productionRustSource,
  routeFailures,
  rustFnBlock,
} from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/webhooks.rs');
const clientPath = path.join(root, 'web/src/lib/api/webhooks.ts');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/webhooks/+page.svelte');

// The production view: comments and `#[cfg(test)]` items alike are blanked, so
// a commented-out handler reads as a deleted one and a test double cannot
// stand in for the handler the server ships.
const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
const client = productionTsSource(readFileSync(clientPath, 'utf8'));
const settingsLayout = productionTsSource(readFileSync(settingsLayoutPath, 'utf8'));
const settingsPage = productionTsSource(readFileSync(settingsPagePath, 'utf8'));
const failures = [];

for (const [method, route] of [
  ['get', '/repos/{owner}/{name}/hooks'],
  ['post', '/repos/{owner}/{name}/hooks'],
  ['get', '/repos/{owner}/{name}/hooks/{id}'],
  ['patch', '/repos/{owner}/{name}/hooks/{id}'],
  ['delete', '/repos/{owner}/{name}/hooks/{id}'],
  ['get', '/repos/{owner}/{name}/hooks/{id}/deliveries'],
  ['post', '/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver'],
]) {
  const pattern = new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`);
  if (!pattern.test(backend)) {
    failures.push(`Backend webhook ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

// ── The admin gate and the repo-scoping of `{id}` ─────────────────────────
//
// A webhook row carries the delivery target and the HMAC key ForgeKeep signs
// deliveries with, and `{id}` is a global `webhooks` primary key. So every
// id-taking door owes two refusals: a non-admin, and a hook that belongs to
// some other repository.
//
// This used to be asserted by grepping the module for
// `resolve_webhook_in_repo(&state.db, &owner, &repo, id)` and counting to five,
// plus a literal `Path((owner, repo, id, delivery_id))`. Two renames later
// neither string existed — scoping had moved to a two-argument helper behind
// the `RepoAdmin` extractor, i.e. it had become *stricter* — and the check
// reported "no scoping" about scoping that was there (card_71260b04bb85).
//
// Rewritten to read each handler on its own instead of the file as a whole: a
// file-wide count of five stays green when one door drops the call and another
// gains a second one. Four assertions, none satisfiable by the others:
//
//   1. The router *declares* `RepoAdmin` for all seven routes. That declaration
//      is what `route_access_sweep_tests::
//      every_route_answers_its_declared_access_level` drives its personas
//      against, so a declaration that does not match behaviour is red there.
//   2. Each handler *takes* one of the gates — a route can keep its declared
//      level while the handler quietly stops asking, and (1) would not notice.
//   3. Each id-taking handler re-anchors the hook to the repository the gate
//      authorized, not to the one the path asked for.
//   4. `redeliver` anchors the delivery to the hook (3) just returned — the
//      second link of the same chain, since `{delivery_id}` is global too.
//
// The gate names and the scoping helper are read out of the modules that define
// them rather than spelled out here. A hard-coded list of symbol names is
// exactly what rotted the first time — and (4) rotted the same way a second
// time by pinning the *shape* of the comparison instead of the chain.

// Derived from executable declarations only. A `pub struct` inside a block
// comment sits at column 0 exactly like a live gate, and so does a
// `#[cfg(test)]` fixture — both minted phantom gate names off the raw file
// (verified: `/* pub struct GhostGate { … } */` and a `#[cfg(test)] pub struct
// FixtureGate` were both listed). The assertion below is satisfied by *any*
// name in this list appearing in a handler signature, so one phantom entry
// weakens it without a word (card_64b6ede78939).
const repoAccess = productionRustCode(
  readFileSync(path.join(root, 'crates/rg-http/src/api/repo_access.rs'), 'utf8'),
);

// Extractors are the braced/generic `pub struct`s; the unit structs next to
// them (`RepoContents`, `Packages`) are scope markers, not gates.
const gates = [
  ...[...repoAccess.matchAll(/^pub struct (\w+)(?:<[^>]*>)?\s*\{/gm)].map((m) => m[1]),
  ...[...repoAccess.matchAll(/^pub(?:\(crate\))? async fn (require_\w+)/gm)].map((m) => m[1]),
];

// The module-private `async fn`s of webhooks.rs — that is what a handler calls
// to re-anchor a hook to a repository.
const scopingHelpers = [...backend.matchAll(/^async fn (\w+)/gm)].map((m) => m[1]);

if (gates.length === 0 || scopingHelpers.length === 0) {
  failures.push(
    'This check can no longer read the gates in api/repo_access.rs or the private helpers in ' +
      'api/webhooks.rs that it derives its assertions from, so its verdicts below mean nothing. ' +
      'Fix the parsing, not the handlers.',
  );
}

failures.push(
  ...routeFailures(
    loadRouteTable(path.join(root, 'crates/rg-http/src/routes.rs')),
    [
      ['GET', '/repos/{owner}/{name}/hooks', 'list_webhooks'],
      ['POST', '/repos/{owner}/{name}/hooks', 'create_webhook'],
      ['GET', '/repos/{owner}/{name}/hooks/{id}', 'get_webhook'],
      ['PATCH', '/repos/{owner}/{name}/hooks/{id}', 'update_webhook'],
      ['DELETE', '/repos/{owner}/{name}/hooks/{id}', 'delete_webhook'],
      ['GET', '/repos/{owner}/{name}/hooks/{id}/deliveries', 'list_deliveries'],
      ['POST', '/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver', 'redeliver'],
    ].map(([method, routePath, handler]) => ({
      method,
      path: routePath,
      handler: `api::webhooks::${handler}`,
      access: 'RepoAdmin',
    })),
  ),
);

/** `Path((a, b, c)): Path<(String, String, i64)>` → its bindings and its types. */
function pathTuple(params) {
  const match = /Path\(\(([^)]*)\)\)\s*:\s*Path<\(([^)]*)\)>/.exec(params);
  if (!match) {
    return null;
  }
  const split = (list) => list.split(',').map((part) => part.trim()).filter(Boolean);
  return { bindings: split(match[1]), types: split(match[2]) };
}

// `hookIdAt` / `deliveryIdAt` are indexes into the Path tuple, so the bindings
// are read from the signature instead of being assumed to be called `id`.
for (const [handler, types, hookIdAt, deliveryIdAt] of [
  ['get_webhook', ['String', 'String', 'i64'], 2, null],
  ['update_webhook', ['String', 'String', 'i64'], 2, null],
  ['delete_webhook', ['String', 'String', 'i64'], 2, null],
  ['list_deliveries', ['String', 'String', 'i64'], 2, null],
  ['redeliver', ['String', 'String', 'i64', 'i64'], 2, 3],
]) {
  const fn = rustFnBlock(backend, handler);
  if (fn === null) {
    failures.push(`api/webhooks.rs no longer defines a \`pub async fn ${handler}\` this check can read`);
    continue;
  }
  if (gates.length === 0 || scopingHelpers.length === 0) {
    continue;
  }

  if (!gates.some((gate) => new RegExp(`\\b${gate}\\b`).test(fn.params))) {
    failures.push(
      `Webhook handler ${handler} must enforce repository admin access: it takes none of the ` +
        `api::repo_access gates (${gates.join(', ')})`,
    );
  }

  const tuple = pathTuple(fn.params);
  if (tuple === null || tuple.types.join(', ') !== types.join(', ')) {
    failures.push(
      `Webhook handler ${handler} must destructure Path<(${types.join(', ')})>` +
        (tuple === null ? ' (no Path extractor found)' : ` (found: (${tuple.types.join(', ')}))`),
    );
    continue;
  }

  // The repository is taken from the gate, never from the path: `{owner}/{name}`
  // is what the caller asked for, the gate's repo is what they were authorized
  // for. Falls back to any `<x>.id` when the gate binding cannot be read, so an
  // unfamiliar spelling weakens the message rather than the assertion.
  const gateBinding = /(\w+)\s*\{\s*(?:\w+\s*:\s*)?(\w+)[^}]*\}\s*:\s*\1\b/.exec(fn.params);
  const repoId = gateBinding ? `${gateBinding[2]}\\.id` : '\\w+\\.id';
  const hookId = tuple.bindings[hookIdAt];

  /** `<helper>(&state.db, <owner>, <id>)` — an anchor call, whatever it is named. */
  const anchorCall = (helper, ownerId, id) =>
    new RegExp(`\\b${helper}\\(\\s*&state\\.db\\s*,\\s*${ownerId}\\s*,\\s*${id}\\s*\\)`);

  const hookAnchor = scopingHelpers
    .map((helper) => anchorCall(helper, repoId, hookId))
    .find((call) => call.test(fn.body));
  if (hookAnchor === undefined) {
    failures.push(
      `Webhook handler ${handler} must re-anchor hook id \`${hookId}\` to the repository the gate ` +
        `authorized — none of ${scopingHelpers.map((helper) => `${helper}(&state.db, <repo>.id, ${hookId})`).join(', ')} ` +
        'is called. `{id}` is a global webhooks primary key, so admin of one repository must not reach ' +
        "another one's rows",
    );
  }

  if (deliveryIdAt === null) {
    continue;
  }

  const deliveryId = tuple.bindings[deliveryIdAt];
  if (!new RegExp(`\\b${deliveryId}\\b`).test(fn.body)) {
    failures.push(`Webhook handler ${handler} destructures delivery id \`${deliveryId}\` but never uses it.`);
  }

  // `{delivery_id}` is a global `webhook_deliveries` primary key, so the second
  // link of the chain: the delivery has to hang off the hook the anchor above
  // just returned, not off any row in scope.
  //
  // The row is found by reading what that anchor call was bound to, so the
  // assertion names no symbol of its own. Either spelling counts — a named
  // helper (which is what `global_id_anchor_guard::ANCHORED` requires, and what
  // this used to demand *not* be used) or the inline comparison it wraps. This
  // assert has rotted twice by pinning one form: it required a literal
  // `.webhook_id == <x>.id` right up until the comparison moved into
  // `delivery_in_webhook`, i.e. it went red on the refactor that made the
  // guarantee stronger (card_4548d995d90c).
  const hookRow = hookAnchor
    ? new RegExp(`let\\s+(\\w+)\\s*=\\s*(?:match\\s+)?${hookAnchor.source}`).exec(fn.body)?.[1]
    : undefined;
  const hookRowId = hookRow ? `${hookRow}\\.id` : '\\w+\\.id';

  const deliveryAnchored =
    scopingHelpers.some((helper) => anchorCall(helper, hookRowId, deliveryId).test(fn.body)) ||
    new RegExp(`\\.webhook_id\\s*==\\s*${hookRowId}`).test(fn.body);
  if (!deliveryAnchored) {
    const hook = hookRow ?? '<hook>';
    failures.push(
      `Webhook handler ${handler} must verify delivery id \`${deliveryId}\` belongs to the hook it ` +
        `just anchored (\`${hook}\`) before redelivering: neither ` +
        `\`<helper>(&state.db, ${hook}.id, ${deliveryId})\` nor an inline \`.webhook_id == ${hook}.id\` ` +
        'is present. `{delivery_id}` is a global webhook_deliveries primary key, so admin of one ' +
        "repository must not replay another one's deliveries",
    );
  }
}

for (const [name, method, pathPattern] of [
  ['list', undefined, /\/hooks`/],
  ['create', 'POST', /\/hooks`[\s\S]*method:\s*'POST'/],
  ['get', undefined, /\/hooks\/\$\{id\}`/],
  ['update', 'PATCH', /\/hooks\/\$\{id\}`[\s\S]*method:\s*'PATCH'/],
  ['remove', 'DELETE', /\/hooks\/\$\{id\}`[\s\S]*method:\s*'DELETE'/],
  ['deliveries', undefined, /\/hooks\/\$\{id\}\/deliveries`/],
  ['redeliver', 'POST', /\/hooks\/\$\{id\}\/deliveries\/\$\{deliveryId\}\/redeliver`[\s\S]*method:\s*'POST'/],
]) {
  if (!new RegExp(`${name}\\s*:\\s*\\(`).test(client)) {
    failures.push(`API client must expose webhooks.${name}`);
  }
  if (!pathPattern.test(client)) {
    failures.push(`API client webhooks.${name} must call the backend ${method || 'GET'} route`);
  }
}

if (!/settings\/webhooks/.test(settingsLayout)) {
  failures.push('Repository settings nav must expose the webhooks page');
}

for (const call of ['webhooks.list(', 'webhooks.create(', 'webhooks.update(', 'webhooks.remove(']) {
  if (!settingsPage.includes(call)) {
    failures.push(`Webhook settings page must call ${call}`);
  }
}

if (!/selectedEvents\.length === 0/.test(settingsPage)) {
  failures.push('Webhook settings page must require at least one backend event.');
}

if (!/content_type:\s*contentType/.test(settingsPage)) {
  failures.push('Webhook settings page must send backend content_type.');
}

const emittedWebhookEvents = [
  'push',
  'issue.opened',
  'issue.closed',
  'issue.comment',
  'pull_request.opened',
  'pull_request.closed',
  'pull_request.merged',
  'release.created',
  'release.deleted',
  'branch.created',
  'branch.deleted',
  'tag.created',
  'tag.deleted',
  'milestone.closed',
];

for (const eventName of emittedWebhookEvents) {
  if (!settingsPage.includes(`'${eventName}'`)) {
    failures.push(`Webhook settings page must expose emitted backend event ${eventName}.`);
  }
}

for (const staleEventName of ['issues', 'pull_request', 'release']) {
  if (new RegExp(`'${staleEventName}'`).test(settingsPage)) {
    failures.push(`Webhook settings page must not expose aggregate event ${staleEventName}; backend dispatch uses concrete event names.`);
  }
}

if (failures.length > 0) {
  console.error('Webhooks frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Webhooks frontend/backend contract ok');
