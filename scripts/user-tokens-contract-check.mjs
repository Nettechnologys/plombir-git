#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import {
  loadRouteTable,
  productionRustSource,
  requireBlock,
  routeFailures,
  rustFnBlock,
  rustStructBody,
} from './lib/rust-source.mjs';

const root = process.cwd();
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');
const backendPath = path.join(root, 'crates/rg-http/src/api/users.rs');
const clientPath = path.join(root, 'web/src/lib/api/tokens.ts');
const pagePath = path.join(root, 'web/src/routes/settings/tokens/+page.svelte');
const navbarPath = path.join(root, 'web/src/lib/components/Navbar.svelte');

const routes = loadRouteTable(routerPath);
const backend = readFileSync(backendPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const navbar = readFileSync(navbarPath, 'utf8');
const failures = [];

// Personal access tokens are the caller's own credentials: `User`, never
// anonymous — an unauthenticated route here would hand out other people's PATs.
failures.push(
  ...routeFailures(routes, [
    { method: 'GET', path: '/users/tokens', handler: 'api::users::list_tokens', access: 'User' },
    { method: 'POST', path: '/users/tokens', handler: 'api::users::create_token', access: 'User' },
    { method: 'DELETE', path: '/users/tokens/{id}', handler: 'api::users::delete_token', access: 'User' },
  ]),
);

// Handlers and the DTO are asserted as declarations. The raw-source greps this
// replaces read a commented-out `pub async fn create_token` — or a
// commented-out `pub struct AccessTokenResponse` — as a live one, so removing
// the sanitized DTO left the gate green while the handler below could go back
// to serializing rows straight out of the database (card_64b6ede78939).
for (const handler of ['list_tokens', 'create_token', 'delete_token']) {
  if (!rustFnBlock(backend, handler)) {
    failures.push('Backend users API must keep token list/create/delete handlers');
    break;
  }
}

if (rustStructBody(backend, 'AccessTokenResponse') === null) {
  failures.push('Backend token listing must use a sanitized AccessTokenResponse DTO');
}

// Anchored on the handler alone. The previous anchor also required the doc
// comment of the *next* handler to follow, so merely reordering the file
// emptied the block this assertion inspects — and the assertion below is
// negative, so an empty block would have waved `token_hash` through.
const listTokensBody = requireBlock(
  productionRustSource(backend),
  /pub async fn list_tokens[\s\S]*?\n\}/,
  'Backend list_tokens handler body could not be located for the token_hash leak check',
  failures,
);
if (listTokensBody && (/serde_json::json!\(tokens\)/.test(listTokensBody) || /token_hash/.test(listTokensBody))) {
  failures.push('Backend token listing must not serialize DB token_hash fields');
}

if (!/export const tokens\s*=\s*\{/.test(client)) {
  failures.push('API client must export tokens helper');
}

if (!/list:\s*\(\)\s*=>[\s\S]*request<Array<\{[\s\S]*last_used_at\?:\s*string\s*\|\s*null[\s\S]*\}>>\('\/users\/tokens'\)/.test(client)) {
  failures.push('API client tokens.list must call GET /users/tokens with sanitized metadata shape');
}

if (!/create:\s*\([^)]*name[^)]*scopes[^)]*expires_at[^)]*\)\s*=>[\s\S]*request<\{[^}]*token:\s*string[\s\S]*\}>\('\/users\/tokens'/.test(client)) {
  failures.push('API client tokens.create must return the one-time raw token');
}

if (!/delete:\s*\([^)]*id[^)]*\)\s*=>\s*\n?\s*request<void>\(`\/users\/tokens\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'\s*\}\)/.test(client)) {
  failures.push('API client tokens.delete must call DELETE /users/tokens/{id}');
}

if (!/from '\$lib\/api\/client\.svelte'/.test(page) || !/tokens\.list\(\)/.test(page)) {
  failures.push('Tokens settings page must load existing tokens through the API client');
}

if (!/tokens\.create\(/.test(page) || !/created\.token/.test(page)) {
  failures.push('Tokens settings page must create tokens and display the one-time raw token');
}

if (!/tokens\.delete\(token\.id\)/.test(page)) {
  failures.push('Tokens settings page must revoke tokens through the API client');
}

if (!/isLoggedIn\(\)/.test(page) || !/goto\('\/login'\)/.test(page)) {
  failures.push('Tokens settings page must redirect anonymous users to login');
}

if (!/href="\/settings\/tokens"/.test(navbar)) {
  failures.push('Authenticated user menu must link to /settings/tokens');
}

if (failures.length > 0) {
  console.error('User token frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('User token frontend/backend contract ok');
