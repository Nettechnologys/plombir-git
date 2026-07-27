#!/usr/bin/env node

import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

import { loadRouteTable, routeFailures } from './lib/rust-source.mjs';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/mfa.ts');
const pagePath = path.join(root, 'web/src/routes/settings/security/+page.svelte');
const navbarPath = path.join(root, 'web/src/lib/components/Navbar.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/mfa.rs');
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');

const failures = [];

function read(file) {
  return readFileSync(file, 'utf8');
}

function expect(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

if (!existsSync(pagePath)) {
  failures.push('Security settings page is missing');
}

const client = read(clientPath);
const page = existsSync(pagePath) ? read(pagePath) : '';
const navbar = read(navbarPath);
const backend = read(backendPath);
const routes = loadRouteTable(routerPath);

// Managing your own second factor requires being the first factor: every one
// of these is `User`. `/users/mfa/verify` is deliberately `Public` (it runs
// mid-login, before a session exists) and is not part of this set.
for (const [method, route, handler] of [
  ['post', '/users/mfa/setup', 'setup_mfa'],
  ['post', '/users/mfa/enable', 'enable_mfa'],
  ['get', '/users/mfa/backup', 'get_backup_codes'],
  ['post', '/users/mfa/disable', 'disable_mfa'],
]) {
  expect(
    backend,
    new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`),
    `Backend MFA ${method.toUpperCase()} ${route} annotation is missing or changed`,
  );
  failures.push(
    ...routeFailures(routes, [{ method, path: route, handler: `api::mfa::${handler}`, access: 'User' }]),
  );
}

for (const [name, route, httpMethod] of [
  ['setup', '/users/mfa/setup', 'POST'],
  ['enable', '/users/mfa/enable', 'POST'],
  ['backup', '/users/mfa/backup', 'GET'],
  ['disable', '/users/mfa/disable', 'POST'],
]) {
  expect(client, new RegExp(`${name}: [\\s\\S]*['"]${route}['"]`), `API client is missing mfa.${name} route`);
  if (httpMethod !== 'GET') {
    expect(client, new RegExp(`${name}: [\\s\\S]*method:\\s*['"]${httpMethod}['"]`), `API client mfa.${name} must use ${httpMethod}`);
  }
}

// The page imports MFA helpers alongside unrelated ones (passkeys, ...), so assert on the
// individual specifiers of the client import instead of on one exact adjacent spelling.
const clientImport = page.match(
  /import\s*\{([^}]*)\}\s*from\s*['"]\$lib\/api\/(?:client\.svelte|mfa)['"]/,
);
if (!clientImport) {
  failures.push('Security page must import MFA helpers from the API client');
} else {
  const specifiers = clientImport[1]
    .split(',')
    .map((entry) => entry.trim().replace(/\s+/g, ' '))
    .filter(Boolean);
  for (const required of ['mfa', 'type MfaBackupStatus', 'type MfaSetupResponse']) {
    if (!specifiers.includes(required)) {
      failures.push(`Security page must use typed MFA client export \`${required}\``);
    }
  }
}
expect(page, /mfa\.setup\(\)/, 'Security page must call mfa.setup() for QR enrollment');
expect(page, /mfa\.enable\(verificationCode\.trim\(\)\)/, 'Security page must enable MFA with the entered code');
expect(page, /mfa\.backup\(\)/, 'Security page must load backup code status');
expect(page, /mfa\.disable\(disablePassword\)/, 'Security page must disable MFA with current password');
expect(page, /{@html setup\.qr_svg}/, 'Security page must render backend QR SVG from setup response');
expect(navbar, /href="\/settings\/security"/, 'User menu must link to security settings');

if (failures.length > 0) {
  console.error('MFA settings frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('MFA settings frontend/backend contract ok');
