#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const pagePath = path.join(root, 'web/src/routes/admin/settings/+page.svelte');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const backendPath = path.join(root, 'crates/rg-http/src/api/admin.rs');

const page = readFileSync(pagePath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');

const failures = [];

for (const method of [
  'listSsoProviders',
  'createSsoProvider',
  'updateSsoProvider',
  'deleteSsoProvider',
]) {
  if (!new RegExp(`\\b${method}:\\s*\\(`).test(client)) {
    failures.push(`API client must expose admin.${method}`);
  }
  if (!new RegExp(`admin\\.${method}\\(`).test(page)) {
    failures.push(`Admin settings page must call admin.${method}`);
  }
}

for (const route of [
  "'/admin/sso/providers'",
  '`/admin/sso/providers/${id}`',
]) {
  if (!client.includes(route)) {
    failures.push(`API client must target ${route}`);
  }
}

if (!/ssoProviders\s*=\s*providers/.test(page)) {
  failures.push('Admin settings page must render providers returned by the backend');
}

if (!/editingSsoId[\s\S]*admin\.updateSsoProvider/.test(page)) {
  failures.push('Admin settings page must update existing SSO providers');
}

if (!/client_secret:\s*ssoForm\.client_secret\s*\|\|\s*undefined/.test(page)) {
  failures.push('Admin settings page must omit blank client_secret fields');
}

if (!/ldap_bind_password:\s*ssoForm\.ldap_bind_password\s*\|\|\s*undefined/.test(page)) {
  failures.push('Admin settings page must omit blank LDAP bind password fields');
}

if (!backend.includes('.or(existing_provider.client_secret_enc)')) {
  failures.push('Backend PATCH must preserve existing client_secret_enc when no replacement secret is sent');
}

if (!backend.includes('.or(existing_provider.ldap_bind_password_enc)')) {
  failures.push('Backend PATCH must preserve existing ldap_bind_password_enc when no replacement password is sent');
}

if (failures.length > 0) {
  console.error('Admin SSO settings frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Admin SSO settings frontend/backend contract ok');
