#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');
const backendPath = path.join(root, 'crates/rg-http/src/api/users.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const pagePath = path.join(root, 'web/src/routes/settings/tokens/+page.svelte');
const navbarPath = path.join(root, 'web/src/lib/components/Navbar.svelte');

const router = readFileSync(routerPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const navbar = readFileSync(navbarPath, 'utf8');
const failures = [];

if (!/\.route\(\s*"\/users\/tokens",\s*get\(api::users::list_tokens\)\.post\(api::users::create_token\)/.test(router)) {
  failures.push('Backend router must expose GET/POST /users/tokens');
}

if (!/\.route\(\s*"\/users\/tokens\/\{id\}",\s*delete\(api::users::delete_token\)/.test(router)) {
  failures.push('Backend router must expose DELETE /users/tokens/{id}');
}

if (!/pub async fn list_tokens/.test(backend) || !/pub async fn create_token/.test(backend) || !/pub async fn delete_token/.test(backend)) {
  failures.push('Backend users API must keep token list/create/delete handlers');
}

if (!/pub struct AccessTokenResponse/.test(backend)) {
  failures.push('Backend token listing must use a sanitized AccessTokenResponse DTO');
}

const listTokensBody = backend.match(/pub async fn list_tokens[\s\S]*?\n}\n\n\/\/\/ POST \/api\/v1\/users\/tokens/)?.[0] || '';
if (/serde_json::json!\(tokens\)/.test(listTokensBody) || /token_hash/.test(listTokensBody)) {
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
