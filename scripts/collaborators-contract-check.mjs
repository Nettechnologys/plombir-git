#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/collaborators.rs');
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');
const clientPaths = [
  path.join(root, 'web/src/lib/api/client.svelte.ts'),
  path.join(root, 'web/src/lib/api/collaborators.ts'),
];
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/collaborators/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const routerSource = readFileSync(routerPath, 'utf8');
const clients = clientPaths.map((file) => [file, readFileSync(file, 'utf8')]);
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const settingsPage = readFileSync(settingsPagePath, 'utf8');
const failures = [];

if (!/patch,\s*\n\s*path\s*=\s*"\/repos\/\{owner\}\/\{name\}\/collaborators\/\{id\}"/.test(backend)) {
  failures.push('Backend collaborator PATCH route is missing or changed');
}

if (!/delete,\s*\n\s*path\s*=\s*"\/repos\/\{owner\}\/\{name\}\/collaborators\/\{user_id\}"/.test(backend)) {
  failures.push('Backend collaborator DELETE route is missing or changed');
}

if (!/\/repos\/\{owner\}\/\{name\}\/collaborators\/\{id\}"[\s\S]*patch\(api::collaborators::update_permission\)[\s\S]*\.delete\(api::collaborators::remove_collaborator\)/.test(routerSource ?? '')) {
  failures.push('Backend router must expose DELETE /collaborators/{user_id} alongside PATCH permission updates');
}

if (/\/repos\/\{owner\}\/\{name\}\/collaborators\/\{user_id\}\/remove/.test(routerSource ?? '')) {
  failures.push('Backend router must not expose legacy POST /collaborators/{user_id}/remove');
}

if (!/Ok\(\(\)\)\s*=>\s*StatusCode::NO_CONTENT\.into_response\(\)/.test(backend)) {
  failures.push('Backend collaborator removal must return an empty 204 response');
}

for (const [file, source] of clients) {
  if (!/updatePermission\s*:\s*\([^)]*\bid\b[^)]*permission[^)]*\)\s*=>/.test(source)) {
    failures.push(`${path.relative(root, file)} must expose collaborators.updatePermission`);
  }

  if (!/\/collaborators\/\$\{id\}`[\s\S]*method:\s*'PATCH'/.test(source)) {
    failures.push(`${path.relative(root, file)} must PATCH /collaborators/{id}`);
  }

  if (!/body:\s*JSON\.stringify\(\{\s*permission\s*\}\)/.test(source)) {
    failures.push(`${path.relative(root, file)} must send the backend permission payload`);
  }

  if (!/remove\s*:\s*\([^)]*\buserId\b[^)]*\)\s*=>/.test(source)) {
    failures.push(`${path.relative(root, file)} must expose collaborators.remove`);
  }

  if (!/\/collaborators\/\$\{userId\}`[\s\S]*method:\s*'DELETE'/.test(source)) {
    failures.push(`${path.relative(root, file)} must DELETE /collaborators/{user_id}`);
  }

  if (/\/collaborators\/\$\{userId\}\/remove`[\s\S]*method:\s*'POST'/.test(source)) {
    failures.push(`${path.relative(root, file)} must not use legacy POST /collaborators/{user_id}/remove`);
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
