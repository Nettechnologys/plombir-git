#!/usr/bin/env node
// Frontend + backend smoke checks for automated integration.
//
// 1) Backend health endpoint: /health
// 2) Backend public API endpoint: /api/v1/repos/explore
// 3) Frontend core routes accessibility
// 4) Frontend admin routes accessibility
// 5) Backend admin routes should enforce authorization when unauthenticated
//
// Optional env:
//   BACKEND_URL=http://127.0.0.1:8080
//   FRONTEND_URL=http://127.0.0.1:5173
//   API_BASE=http://127.0.0.1:8080/api/v1
//
// Exit code 1 if any check fails.

const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const FRONTEND_URL = (process.env.FRONTEND_URL || 'http://127.0.0.1:5173').replace(/\/$/, '');
const API_BASE = (process.env.API_BASE || `${BACKEND_URL}/api/v1`).replace(/\/$/, '');
const ADMIN_TOKEN = process.env.ADMIN_TOKEN || process.env.ADMIN_JWT || process.env.ACCESS_TOKEN || '';

const checks = [];
let failed = 0;

const FRONTEND_ROUTES = ['/', '/search', '/search?q=readme&type=all'];
const ADMIN_FRONTEND_ROUTES = ['/admin', '/admin/users', '/admin/orgs', '/admin/audit', '/admin/settings'];
const ADMIN_ROUTE_NAME = ADMIN_TOKEN ? 'with admin token' : 'skipped (set ADMIN_TOKEN)';

const ADMIN_API_TOKEN_INIT = ADMIN_TOKEN ? {
  headers: {
    Authorization: `Bearer ${ADMIN_TOKEN}`,
  },
} : undefined;

async function requestJson(name, url, init) {
  try {
    const res = await fetch(url, init);
    const body = await res.json().catch(() => null);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    checks.push(`✅ ${name}: ${res.status}`);
    return { ok: true, status: res.status, body };
  } catch (e) {
    checks.push(`❌ ${name}: ${e.message}`);
    failed += 1;
    return { ok: false, body: null };
  }
}

async function expectStatus(name, url, init, expectedStatuses) {
  try {
    const res = await fetch(url, init);
    if (!expectedStatuses.includes(res.status)) {
      checks.push(`❌ ${name}: HTTP ${res.status} (expect ${expectedStatuses.join(',')})`);
      failed += 1;
      return;
    }
    checks.push(`✅ ${name}: HTTP ${res.status}`);
  } catch (e) {
    checks.push(`❌ ${name}: ${e.message}`);
    failed += 1;
  }
}

async function expectJson(name, url, init, shapeCheck) {
  try {
    const res = await fetch(url, init);
    const contentType = res.headers.get('content-type') || '';
    if (!res.ok) {
      checks.push(`❌ ${name}: HTTP ${res.status}`);
      failed += 1;
      return;
    }

    if (!/application\/json/i.test(contentType)) {
      checks.push(`❌ ${name}: expected json but got ${contentType || 'unknown'}`);
      failed += 1;
      return;
    }

    const body = await res.json().catch(() => null);
    if (!body) {
      checks.push(`❌ ${name}: response is not valid JSON`);
      failed += 1;
      return;
    }

    const ok = shapeCheck(body);
    if (!ok) {
      checks.push(`❌ ${name}: response shape mismatch`);
      failed += 1;
      return;
    }

    checks.push(`✅ ${name}: JSON shape ok`);
  } catch (e) {
    checks.push(`❌ ${name}: ${e.message}`);
    failed += 1;
  }
}

function hasObject(v) {
  return typeof v === 'object' && v !== null;
}

function hasArray(v) {
  return Array.isArray(v);
}

function checkHtml(name, body) {
  if (typeof body !== 'string' || !body.toLowerCase().includes('<html')) {
    checks.push(`❌ ${name}: response is not HTML`);
    failed += 1;
    return;
  }
  checks.push(`✅ ${name}: html loaded`);
}

console.log('Integration smoke start');
console.log(`backend: ${BACKEND_URL}`);
console.log(`frontend: ${FRONTEND_URL}`);
console.log(`api: ${API_BASE}`);

const health = await requestJson('GET /health', `${BACKEND_URL}/health`);
if (
  health.ok &&
  health.body &&
  !['healthy', 'ok'].includes(String(health.body.status || ''))
) {
  checks.push(`❌ /health: status=${health.body.status}`);
  failed += 1;
} else if (health.ok) {
  checks.push(`✅ /health: status=${health.body?.status || 'ok'}`);
}

await requestJson('GET /api/v1/repos/explore', `${API_BASE}/repos/explore?per_page=1`);

for (const route of FRONTEND_ROUTES) {
  try {
    const res = await fetch(`${FRONTEND_URL}${route}`);
    const text = await res.text();
    if (!res.ok) {
      checks.push(`❌ frontend ${route}: HTTP ${res.status}`);
      failed += 1;
    } else {
      checkHtml(`frontend ${route}`, text);
    }
  } catch (e) {
    checks.push(`❌ frontend ${route}: ${e.message}`);
    failed += 1;
  }
}

for (const route of ADMIN_FRONTEND_ROUTES) {
  try {
    const res = await fetch(`${FRONTEND_URL}${route}`);
    const text = await res.text();
    if (!res.ok) {
      checks.push(`❌ frontend ${route}: HTTP ${res.status}`);
      failed += 1;
      continue;
    }

    checkHtml(`frontend ${route}`, text);
  } catch (e) {
    checks.push(`❌ frontend ${route}: ${e.message}`);
    failed += 1;
  }
}

await expectStatus(
  'Backend /admin/users without auth should deny',
  `${API_BASE}/admin/users`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend /admin/orgs without auth should deny',
  `${API_BASE}/admin/orgs`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend /admin/audit/logs without auth should deny',
  `${API_BASE}/admin/audit/logs`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend /admin/settings without auth should deny',
  `${API_BASE}/admin/settings`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend /admin/sso/providers without auth should deny',
  `${API_BASE}/admin/sso/providers`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend repo starred state without auth should deny',
  `${API_BASE}/repos/smoke-owner/smoke-repo/starred`,
  undefined,
  [401, 403],
);

await expectStatus(
  'Backend repo watch state without auth should deny',
  `${API_BASE}/repos/smoke-owner/smoke-repo/watch`,
  undefined,
  [401, 403],
);

if (ADMIN_TOKEN) {
  await expectJson(
    'Backend /admin/users with admin token should allow',
    `${API_BASE}/admin/users?page=1&per_page=1`,
    ADMIN_API_TOKEN_INIT,
    (body) => {
      return hasObject(body)
        && hasArray(body.data)
        && hasObject(body.pagination)
        && Number.isInteger(body.pagination.page)
        && Number.isInteger(body.pagination.per_page)
        && Number.isInteger(body.pagination.total)
        && Number.isInteger(body.pagination.total_pages);
    },
  );

  await expectJson(
    'Backend /admin/orgs with admin token should allow',
    `${API_BASE}/admin/orgs?page=1&per_page=1`,
    ADMIN_API_TOKEN_INIT,
    (body) => {
      return hasObject(body)
        && hasArray(body.data)
        && hasObject(body.pagination)
        && Number.isInteger(body.pagination.page)
        && Number.isInteger(body.pagination.per_page)
        && Number.isInteger(body.pagination.total)
        && Number.isInteger(body.pagination.total_pages);
    },
  );

  await expectJson(
    'Backend /admin/audit/logs with admin token should allow',
    `${API_BASE}/admin/audit/logs?page=0&page_size=1`,
    ADMIN_API_TOKEN_INIT,
    (body) => {
      return hasObject(body)
        && Number.isInteger(body.total)
        && Number.isInteger(body.page)
        && Number.isInteger(body.page_size)
        && hasArray(body.logs);
    },
  );

  await expectJson(
    'Backend /admin/settings with admin token should allow',
    `${API_BASE}/admin/settings`,
    ADMIN_API_TOKEN_INIT,
    (body) => {
      return hasObject(body)
        && typeof body.maintenance_mode === 'boolean'
        && (typeof body.banner_message === 'string' || body.banner_message === null)
        && ['info', 'warning', 'error'].includes(body.banner_type);
    },
  );

  await expectJson(
    'Backend /admin/sso/providers with admin token should allow',
    `${API_BASE}/admin/sso/providers`,
    ADMIN_API_TOKEN_INIT,
    (body) => hasArray(body),
  );
} else {
  checks.push(`⚪ Backend admin API checks with auth: skipped (${ADMIN_ROUTE_NAME})`);
}

for (const line of checks) {
  console.log(line);
}

if (failed > 0) {
  console.log(`\n❌ ${failed} check(s) failed`);
  process.exit(1);
}

console.log('\n✅ integration checks passed');
process.exit(0);
