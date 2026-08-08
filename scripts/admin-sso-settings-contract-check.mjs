#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const pagePath = path.join(root, 'web/src/routes/admin/settings/+page.svelte');
const clientPath = path.join(root, 'web/src/lib/api/admin.ts');
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

// ── Provider types: the form must be able to produce every kind the backend
// accepts, and no kind it does not.
//
// The select used to offer one combined "OAuth2 / OIDC" option worth
// `oauth2`, so `oidc` — the only type that reads `discovery_url` — was
// unreachable from the admin UI while the Discovery URL field sat in the form
// promising otherwise (card_742a8bb4de37). A set comparison is what turns that
// back into a failing check instead of a dead end an operator finds.
const validatorMatch = backend.match(
  /fn validate_sso_provider_request\([\s\S]*?match provider_type \{([\s\S]*?)\n {8}other =>/,
);
if (!validatorMatch) {
  failures.push('Cannot read the provider types validate_sso_provider_request accepts');
} else {
  const backendTypes = new Set(
    [...validatorMatch[1].matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]),
  );
  const selectMatch = page.match(/<select id="sso-type"[\s\S]*?<\/select>/);
  if (!selectMatch) {
    failures.push('Admin settings page must offer a provider type selector');
  } else {
    const uiTypes = new Set(
      [...selectMatch[0].matchAll(/<option value="([^"]+)"/g)].map((m) => m[1]),
    );
    for (const type of backendTypes) {
      if (!uiTypes.has(type)) {
        failures.push(
          `Provider type '${type}' is accepted by the backend but cannot be selected in the admin form`,
        );
      }
    }
    for (const type of uiTypes) {
      if (!backendTypes.has(type)) {
        failures.push(
          `Admin form offers provider type '${type}', which validate_sso_provider_request rejects`,
        );
      }
    }
  }
}

// The field only `oidc` reads must only be shown for `oidc`; otherwise the form
// invites an operator to fill in a value the chosen type throws away.
if (!/\{#if ssoForm\.provider_type === 'oidc'\}[\s\S]{0,400}?id="sso-discovery-url"/.test(page)) {
  failures.push("Discovery URL must be shown only when the provider type is 'oidc'");
}

if (failures.length > 0) {
  console.error('Admin SSO settings frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Admin SSO settings frontend/backend contract ok');
