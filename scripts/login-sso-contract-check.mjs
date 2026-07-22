#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const loginPath = path.join(root, 'web/src/routes/login/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/sso.rs');
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');

const client = readFileSync(clientPath, 'utf8');
const login = readFileSync(loginPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const router = readFileSync(routerPath, 'utf8');

const failures = [];

if (!/pub\s+struct\s+SsoProviderInfo\s*\{[\s\S]*slug:\s*String[\s\S]*name:\s*String[\s\S]*provider_type:\s*String[\s\S]*icon_url:\s*Option<String>[\s\S]*\}/.test(backend)) {
  failures.push('Backend public SSO provider response must include slug, name, provider_type, and icon_url');
}

if (!/route\("\/auth\/sso\/providers",\s*get\(api::sso::list_providers\)\)/.test(router)) {
  failures.push('Router must expose GET /auth/sso/providers');
}

if (!/export\s+interface\s+PublicSsoProvider\s*\{[\s\S]*slug:\s*string[\s\S]*name:\s*string[\s\S]*provider_type:\s*string[\s\S]*icon_url:\s*string\s*\|\s*null[\s\S]*\}/.test(client)) {
  failures.push('API client must type public SSO providers');
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

if (!/ssoProviders\.length\s*>\s*0/.test(login) || !/auth\.ssoAuthorizeUrl\(provider\.slug\)/.test(login)) {
  failures.push('Login page must render provider links to backend SSO authorize URLs');
}

if (!/provider\.icon_url/.test(login) || !/provider\.name/.test(login)) {
  failures.push('Login page must render backend-provided provider display data');
}

if (failures.length > 0) {
  console.error('Login SSO frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Login SSO frontend/backend contract ok');
