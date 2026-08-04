#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { loadRouteTable, routeFailures } from './lib/rust-source.mjs';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/collaborators.rs');
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');
const clientPaths = [
  path.join(root, 'web/src/lib/api/collaborators.ts'),
];
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/collaborators/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const routes = loadRouteTable(routerPath);
const clients = clientPaths.map((file) => [file, readFileSync(file, 'utf8')]);
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const settingsPage = readFileSync(settingsPagePath, 'utf8');
const failures = [];

// The `#[utoipa::path]` URLs are deliberately NOT re-asserted here.
//
// They used to be, one hand-written regex per verb — and that is how the drift
// this file now avoids got in and stayed: the DELETE pin was copied from the
// annotation rather than from the router, so it pinned `{user_id}` while the
// route mounted `{id}`, and the check went green on a spec that documented a
// URL the server does not serve. A per-endpoint copy of a URL is one more
// declaration to keep in sync, not a check.
//
// `openapi-route-coverage-contract-check.mjs` now compares every annotation
// against its route table row mechanically, for all 286 documented handlers.
// Adding a copy back here would weaken that, not strengthen it.

// Changing or revoking someone's access is repo administration — both routes
// must say so, or the sweep would be checking a level the router never claims.
failures.push(
  ...routeFailures(routes, [
    {
      method: 'PATCH',
      path: '/repos/{owner}/{name}/collaborators/{id}',
      handler: 'api::collaborators::update_permission',
      access: 'RepoAdmin',
    },
    {
      method: 'DELETE',
      path: '/repos/{owner}/{name}/collaborators/{id}',
      handler: 'api::collaborators::remove_collaborator',
      access: 'RepoAdmin',
    },
  ]),
);

// One URL, two verbs, two different id spaces: PATCH addresses the
// `repo_collaborators` row, DELETE addresses the collaborator's `users.id`. The
// asymmetry is intentional, which is exactly why neither annotation may leave it
// unsaid — a client generated from the spec reads `description = "id"` as "the
// same id as the sibling operation" and gets a 404 on a live collaborator. The
// spec is the only place a consumer can learn this, so the gate is here.
const idParamDescription = (verb) => {
  const block = backend
    .split('#[utoipa::path(')
    .find((chunk) => new RegExp(`^\\s*${verb},`).test(chunk)
      && /path = "\/repos\/\{owner\}\/\{name\}\/collaborators\/\{id\}"/.test(chunk));
  if (!block) return null;
  const match = /\("id" = i64, Path, description = ([\s\S]*?)\),\n/.exec(block);
  return match ? match[1] : null;
};

for (const [verb, space, sibling] of [
  ['patch', 'repo_collaborators.id', 'users.id'],
  ['delete', 'users.id', 'repo_collaborators row id'],
]) {
  const description = idParamDescription(verb);
  if (description === null) {
    failures.push(`${verb.toUpperCase()} /collaborators/{id} must document its {id} path parameter`);
    continue;
  }
  if (!description.includes(space)) {
    failures.push(
      `${verb.toUpperCase()} /collaborators/{id} must name the id space it takes (${space})`,
    );
  }
  if (!description.includes(sibling)) {
    failures.push(
      `${verb.toUpperCase()} /collaborators/{id} must say how its {id} differs from the sibling verb (${sibling})`,
    );
  }
}

// Any method on the legacy path, not just POST: the point is that the path is gone.
if (routes.some((route) => route.path === '/repos/{owner}/{name}/collaborators/{user_id}/remove')) {
  failures.push('Backend router must not expose legacy POST /collaborators/{user_id}/remove');
}

if (!/Ok\(\(\)\)\s*=>\s*StatusCode::NO_CONTENT\.into_response\(\)/.test(backend)) {
  failures.push('Backend collaborator removal must return an empty 204 response');
}

for (const [file, source] of clients) {
  if (!/updatePermission\s*:\s*\([^)]*\bid\b[^)]*permission[^)]*\)\s*=>/.test(source)) {
    failures.push(`${path.relative(root, file)} must expose collaborators.updatePermission`);
  }

  // Both verbs now spell the segment `${id}` — axum will not mount them with
  // different names — so the method has to sit inside the same match as the
  // URL. With `[\s\S]*` between them, the PATCH template plus the DELETE method
  // further down the file satisfied the DELETE assertion, and either call site
  // could drift with the check none the wiser.
  if (!/\/collaborators\/\$\{id\}`,\s*\{\s*method:\s*'PATCH'/.test(source)) {
    failures.push(`${path.relative(root, file)} must PATCH /collaborators/{id}`);
  }

  if (!/body:\s*JSON\.stringify\(\{\s*permission\s*\}\)/.test(source)) {
    failures.push(`${path.relative(root, file)} must send the backend permission payload`);
  }

  if (!/remove\s*:\s*\([^)]*\bid\b[^)]*\)\s*=>/.test(source)) {
    failures.push(`${path.relative(root, file)} must expose collaborators.remove`);
  }

  if (!/\/collaborators\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'/.test(source)) {
    failures.push(`${path.relative(root, file)} must DELETE /collaborators/{id}`);
  }

  // The removal must still carry the collaborator's *user* id, whatever the URL
  // segment is called — the page holds both keys and only one of them works.
  if (!/collaborators\.remove\([^)]*\.user_id\s*\)/.test(settingsPage)) {
    failures.push('Collaborators settings page must remove by collaborator.user_id, not the row id');
  }

  if (/\/collaborators\/\$\{id\}\/remove`[\s\S]*method:\s*'POST'/.test(source)) {
    failures.push(`${path.relative(root, file)} must not use legacy POST /collaborators/{id}/remove`);
  }
}

if (!/settings\/collaborators/.test(settingsLayout)) {
  failures.push('Repository settings nav must expose the collaborators page');
}

if (!/collaborators\.list\(/.test(settingsPage)) {
  failures.push('Collaborators settings page must load backend collaborators');
}

if (!/collaborators\.add\(/.test(settingsPage)) {
  failures.push('Collaborators settings page must add collaborators through the API client');
}

if (!/collaborators\.updatePermission\(/.test(settingsPage)) {
  failures.push('Collaborators settings page must update collaborator permissions');
}

if (!/collaborators\.remove\(/.test(settingsPage)) {
  failures.push('Collaborators settings page must remove collaborators');
}

if (failures.length > 0) {
  console.error('Collaborators frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Collaborators frontend/backend contract ok');
