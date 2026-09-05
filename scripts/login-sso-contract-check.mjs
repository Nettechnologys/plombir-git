#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { loadRouteTable, routeFailures, rustStructBody, stripRustComments } from './lib/rust-source.mjs';
import { productionTsSource, tsInterfaceBody } from './lib/ts-source.mjs';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/auth.ts');
const loginPath = path.join(root, 'web/src/routes/login/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/sso.rs');
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');

// Both halves of this contract are read through their production view, so a
// commented-out declaration reads as a deleted one on either side of the wire.
const client = productionTsSource(readFileSync(clientPath, 'utf8'));
const login = productionTsSource(readFileSync(loginPath, 'utf8'));
const backend = stripRustComments(readFileSync(backendPath, 'utf8'));
const routes = loadRouteTable(routerPath);

const failures = [];

// Read inside the struct rather than `/pub struct SsoProviderInfo[\s\S]*slug:/`,
// which any later declaration in sso.rs satisfies — see `rustStructBody`.
const ssoProviderInfo = rustStructBody(backend, 'SsoProviderInfo');
if (ssoProviderInfo === null) {
  failures.push('api/sso.rs no longer defines a `struct SsoProviderInfo` this check can read');
} else {
  const fields = [
    ['slug', /\bslug:\s*String/],
    ['name', /\bname:\s*String/],
    ['provider_type', /\bprovider_type:\s*String/],
    ['icon_url', /\bicon_url:\s*Option<String>/],
  ];
  for (const [field, re] of fields) {
    if (!re.test(ssoProviderInfo)) {
      failures.push(`Backend SsoProviderInfo must declare \`${field}\` — the login page renders it before anyone is logged in`);
    }
  }
}

// The login page fetches this before anyone is logged in, so `Public` is the
// contract — not an oversight the sweep should later "tighten".
failures.push(
  ...routeFailures(routes, [
    { method: 'GET', path: '/auth/sso/providers', handler: 'api::sso::list_providers', access: 'Public' },
  ]),
);

// Same bridge on the client half of the same contract — see `tsInterfaceBody`.
const publicSsoProvider = tsInterfaceBody(client, 'PublicSsoProvider');
if (publicSsoProvider === null) {
  failures.push('web/src/lib/api/auth.ts no longer declares an `interface PublicSsoProvider` this check can read');
} else {
  const members = [
    ['slug', /\bslug:\s*string/],
    ['name', /\bname:\s*string/],
    ['provider_type', /\bprovider_type:\s*string/],
    ['icon_url', /\bicon_url:\s*string\s*\|\s*null/],
  ];
  for (const [member, re] of members) {
    if (!re.test(publicSsoProvider)) {
      failures.push(`API client PublicSsoProvider must type \`${member}\` as the backend sends it`);
    }
  }
}

if (!/listSsoProviders:\s*\(\)\s*=>\s*\n?\s*request<PublicSsoProvider\[\]>\('\/auth\/sso\/providers'\)/.test(client)) {
  failures.push('API client must call GET /auth/sso/providers for login provider discovery');
}

if (!/ssoAuthorizeUrl:\s*\(slug:\s*string\)\s*=>\s*\n?\s*withApiBase\(`\/auth\/sso\/\$\{encodeURIComponent\(slug\)\}`\)/.test(client)) {
  failures.push('API client must build encoded SSO authorize URLs');
}

if (!/auth\.listSsoProviders\(\)/.test(login)) {
  failures.push('Login page must load public SSO providers');
}

if (!/knownSsoProviders\.length\s*>\s*0/.test(login) || !/auth\.ssoAuthorizeUrl\(provider\.slug\)/.test(login)) {
  failures.push('Login page must render provider links to backend SSO authorize URLs');
}

if (!/provider\.icon_url/.test(login) || !/provider\.name/.test(login)) {
  failures.push('Login page must render backend-provided provider display data');
}

if (
  !/optionalSection\(\s*auth\.listSsoProviders\(\)/.test(login)
  || !/isUnavailable\(providers\)[\s\S]*?SSO_UNAVAILABLE/.test(login)
) {
  failures.push('Login page must preserve and log provider-list failures as an unavailable state');
}

if (
  !/ssoProviders\s*===\s*SSO_UNAVAILABLE/.test(login)
  || !/onclick=\{loadSsoProviders\}/.test(login)
) {
  failures.push('Login page must render an unavailable provider-list state with a retry');
}

if (failures.length > 0) {
  console.error('Login SSO frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Login SSO frontend/backend contract ok');
