#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/webhooks.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/webhooks/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const settingsPage = readFileSync(settingsPagePath, 'utf8');
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

if (!/Path\(\(owner,\s*repo,\s*id,\s*delivery_id\)\):\s*Path<\(String,\s*String,\s*i64,\s*i64\)>/.test(backend)) {
  failures.push('Webhook redelivery handler must destructure owner, repo, hook id, and delivery id path params.');
}

const scopedHelperCalls = backend.match(/resolve_webhook_in_repo\(&state\.db,\s*&owner,\s*&repo,\s*id\)\.await/g) || [];
if (scopedHelperCalls.length < 5) {
  failures.push('Webhook get/update/delete/deliveries/redeliver handlers must resolve hook ids within the routed repository.');
}

if (!/delivery\.webhook_id\s*==\s*hook\.id/.test(backend)) {
  failures.push('Webhook redelivery must verify the delivery belongs to the routed hook before redelivering.');
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
