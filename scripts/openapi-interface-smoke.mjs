#!/usr/bin/env node

// Replay the published OpenAPI spec against a running server.
//
// The spec is the contract a generated client, Swagger UI's "Try it out" and a
// human reading `/api-docs/` all go by, so the question this asks is the one
// they ask: does the URL the document advertises reach the endpoint it claims?
//
// For a long time it could not answer that. It built every request as
// `BACKEND_URL + path` while the router mounts the whole REST API under
// `/api/v1`, so all 288 requests went to a path no route claims — where the SPA
// fallback answers `index.html` with HTTP 200. The only failure threshold was
// `>= 500`, so those 200s printed as ✅ and the run passed having exercised the
// frontend shell 288 times (card_b23fa617838f).
//
// Both halves are fixed here, and the fix for the first is what makes the
// second possible:
//
//   * the request base comes from the spec's own `servers[0].url` — the
//     document now declares where its paths live, so this script no longer
//     guesses (and a spec that stops declaring it fails loudly below);
//   * a path that is not routed is detected rather than assumed. "404 fails"
//     would be wrong in both directions: an unmounted path does not 404 in
//     production, it returns the SPA, while `GET /repos/testuser/testrepo`
//     legitimately 404s because that repository does not exist. So the fallback
//     is CALIBRATED — one request to a path that certainly is not mounted, per
//     method — and any advertised path answered the same way is reported as
//     unrouted.
//
// Two passes, because they have different costs. The routing pass is anonymous,
// read-only and deterministic: one GET per advertised path, asserting only that
// the router answers (401, 403, 404 and 405 all prove it did). That is the pass
// CI runs, via OPENAPI_SMOKE_ROUTING_ONLY=1. The replay pass then exercises
// every (path, method) with a generated token and a sampled body, which mutates
// state and is for a throwaway instance.

const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const API_BASE = `${BACKEND_URL}/api/v1`;
const OPENAPI_URL = `${BACKEND_URL}/api-docs/openapi.json`;
const OPENAPI_TOKEN = process.env.OPENAPI_TOKEN || null;
const OPENAPI_REQUIRE_AUTH = String(process.env.OPENAPI_REQUIRE_AUTH || '1') === '1';
const ROUTING_ONLY = String(process.env.OPENAPI_SMOKE_ROUTING_ONLY || '0') === '1';

// A path no route will ever claim, used to learn what "not routed" looks like
// on this server. Deliberately not a plausible endpoint name: the calibration
// is worthless if the probe ever matches something.
const ABSENT_PATH = '/__forgekeep_openapi_smoke_absent__';

// A path the spec declares for POST only. Probed with GET, it must draw a
// router answer (405) — the control that proves the calibration above still
// discriminates. If this endpoint ever moves, the control fails and says so
// rather than letting the routing pass degrade into a no-op.
const ROUTED_CONTROL_PATH = '/users/login';

// Where requests are sent: the spec's own `servers[0].url`, resolved against
// BACKEND_URL. Assigned once the document has been read.
let REQUEST_BASE = BACKEND_URL;

const SAMPLE_BY_NAME = {
  owner: 'testuser',
  repo: 'testrepo',
  name: 'demo',
  title: 'readme',
  number: '1',
  id: '1',
  sha: 'main',
  ref: 'main',
  path: 'README.md',
  format: 'npm',
  username: 'testuser',
  password: 'password123',
  email: 'user@example.com',
  branch: 'main',
  base_branch: 'main',
  head_branch: 'main',
  new_name: 'demo',
  file: 'README.md',
  tag: 'v1.0.0',
  issue_number: '1',
  pull_number: '1',
  token: 'smoke-token',
  query: 'readme',
  q: 'readme',
  type: 'all',
  state: 'open',
  action: 'close',
  org: 'demoorg',
  name_: 'demo',
  target: 'refs/heads/main',
  visibility: 'public',
};

const SAMPLE_PARAMS = {
  owner: 'testuser',
  repo: 'testrepo',
  name: 'demo',
  title: 'readme',
  number: '1',
  id: '1',
  sha: 'main',
  ref: 'main',
  path: 'README.md',
  format: 'npm',
  username: 'testuser',
  branch: 'main',
  base_branch: 'main',
  head_branch: 'main',
  new_name: 'demo',
  file: 'README.md',
  tag: 'v1.0.0',
  issue_number: '1',
  pull_number: '1',
};

const SKIP_METHODS = new Set(['head']);
const TIMEOUT_MS = Number(process.env.OPENAPI_SMOKE_TIMEOUT_MS || 10000);
const FAIL_ON_PARAMETER_MISMATCH = String(process.env.OPENAPI_SMOKE_STRICT_ALIGN || '1') === '1';

const checks = [];
let failed = 0;

function sampleForParam(name) {
  const key = String(name).replace(/\./g, '_').toLowerCase();
  if (SAMPLE_BY_NAME[key]) return SAMPLE_BY_NAME[key];
  if (SAMPLE_PARAMS[key]) return SAMPLE_PARAMS[key];

  if (key.endsWith('_name')) {
    const base = key.replace(/_name$/, '');
    return SAMPLE_PARAMS[base] || 'demo';
  }

  if (/^(\d+|id|number|num|page|limit|offset)/.test(key)) return '1';
  if (key.includes('sha') || key.includes('ref')) return 'main';
  if (key.includes('branch')) return 'main';
  if (key.includes('path')) return 'README.md';
  if (key.includes('format')) return 'npm';
  if (key.includes('owner') || key.includes('user') || key.includes('org')) return 'testuser';
  return 'demo';
}

function resolveTemplate(pathTemplate, params = {}, includeQuery = false) {
  let path = String(pathTemplate || '');
  const [basePath, query] = path.split('?');
  const resolveSegment = (value, keyMap) => {
    return String(value).replace(/\{([^}]+)\}/g, (_, key) => {
      const name = String(key || '').trim();
      const sample = keyMap[name] ?? sampleForParam(name);
      return encodeURIComponent(sample);
    });
  };

  const resolvedPath = resolveSegment(basePath, params);
  const resolvedQuery = query ? resolveSegment(query, params) : '';

  if (!includeQuery) return resolvedPath;
  return resolvedQuery ? `${resolvedPath}?${resolvedQuery}` : resolvedPath;
}

function normalizeSpecRef(ref) {
  return String(ref || '')
    .trim()
    .replace(/^#\//, '')
    .split('/')
    .filter(Boolean);
}

function resolveRef(value, openapiDoc) {
  if (!value || typeof value !== 'object' || !value.$ref) return value;
  const parts = normalizeSpecRef(value.$ref);
  let cur = openapiDoc;
  for (const part of parts) {
    cur = cur?.[part];
  }
  return cur || value;
}

function sampleForSchema(schema, openapiDoc, hint) {
  const resolved = resolveRef(schema, openapiDoc) || {};
  if (resolved.example !== undefined) return resolved.example;
  if (resolved.default !== undefined) return resolved.default;
  if (resolved.const !== undefined) return resolved.const;
  if (Array.isArray(resolved.enum) && resolved.enum.length > 0) return resolved.enum[0];

  if (Array.isArray(resolved.oneOf) && resolved.oneOf.length > 0) {
    return sampleForSchema(resolved.oneOf[0], openapiDoc, hint);
  }

  if (Array.isArray(resolved.anyOf) && resolved.anyOf.length > 0) {
    return sampleForSchema(resolved.anyOf[0], openapiDoc, hint);
  }

  if (Array.isArray(resolved.allOf) && resolved.allOf.length > 0) {
    const target = resolved.allOf.find((entry) => (entry.type || '').toLowerCase() === 'object') || resolved.allOf[0];
    return sampleForSchema(target, openapiDoc, hint);
  }

  if (resolved.type === 'string' || (resolved.type === undefined && resolved.properties)) {
    if (resolved.format === 'date-time') return '2026-06-19T00:00:00Z';
    if (resolved.format === 'date') return '2026-06-19';
    if (resolved.format === 'email') return 'user@example.com';
    if (resolved.format === 'uuid') return '11111111-2222-3333-4444-555555555555';
    return resolved.format === 'uri' ? 'https://example.com' : sampleForParam(hint || 'name');
  }

  if (resolved.type === 'number' || resolved.type === 'integer') {
    return 1;
  }

  if (resolved.type === 'boolean') {
    return true;
  }

  if (resolved.type === 'array') {
    return [sampleForSchema(resolved.items || {}, openapiDoc, hint)];
  }

  if (resolved.type === 'object' || resolved.properties) {
    const props = resolved.properties || {};
    const required = new Set(resolved.required || []);
    const body = {};

    for (const [k, v] of Object.entries(props)) {
      if (required.has(k) || required.size === 0) {
        body[k] = sampleForSchema(v, openapiDoc, k);
      }
    }

    if (Object.keys(body).length === 0) {
      for (const [k, v] of Object.entries(props)) {
        body[k] = sampleForSchema(v, openapiDoc, k);
        break;
      }
    }

    return body;
  }

  return sampleForParam(hint || 'value');
}

function shouldAuth(operation, openapiDoc) {
  const hasRequiredSecurity = (list) => {
    if (!Array.isArray(list)) return false;
    return list.some((entry) => entry && Object.keys(entry).length > 0);
  };

  if (operation.security !== undefined) {
    return hasRequiredSecurity(operation.security);
  }

  return hasRequiredSecurity(openapiDoc.security);
}

function normalizeParameters(op = {}, pathItem = {}) {
  const result = [];
  const all = [...(pathItem.parameters || []), ...(op.parameters || [])];
  const seen = new Set();

  for (const p of all) {
    if (!p || p.$ref) continue;
    const key = `${p.in || 'query'}:${p.name}`;
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(p);
  }

  return result;
}

function buildParamsFromSpec(op, pathItem, openapiDoc) {
  const params = normalizeParameters(op, pathItem);
  const byIn = {
    path: {},
    query: {},
    header: {},
  };
  for (const p of params) {
    if (!p || !p.in) continue;
    if (p.in !== 'query' && p.in !== 'path' && p.in !== 'header') continue;

    const key = String(p.name || '').toLowerCase();
    const sample = sampleForParam(key);
    byIn[p.in][p.name] = String(sample);

    const schema = resolveRef(p.schema || {}, openapiDoc);
    if (schema?.type && ['integer', 'number', 'boolean'].includes(schema.type)) {
      byIn[p.in][p.name] = schema.type === 'boolean' ? 'true' : '1';
    }
  }

  return {
    path: byIn.path,
    header: byIn.header,
    query: byIn.query,
  };
}

function buildPath(rawPath, pathParams) {
  const path = resolveTemplate(rawPath, pathParams);
  return `${REQUEST_BASE}${path}`;
}

function buildQueryFromParams(queryParams) {
  const query = Object.entries(queryParams || {})
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
    .sort((a, b) => a.localeCompare(b))
    .join('&');
  return query ? `?${query}` : '';
}

function buildPayloadForOperation(operation, openapiDoc) {
  const reqBody = operation?.requestBody;
  if (!reqBody || reqBody.content === undefined) return null;
  const content = reqBody.content || {};
  const candidate = content['application/json'] || content['application/problem+json'] || content['text/plain'];
  if (!candidate || !candidate.schema) return null;

  const schema = resolveRef(candidate.schema, openapiDoc);
  if (!schema) return null;
  return JSON.stringify(sampleForSchema(schema, openapiDoc));
}

function isParameterMismatch(method, rawPath, resolvedPath, pathParams) {
  if (!FAIL_ON_PARAMETER_MISMATCH) return false;
  const raw = String(rawPath || '');
  const cleanRaw = raw.split('?')[0].replace(/\/+$/g, '');
  // The whole request base comes off, not just the origin: `REQUEST_BASE` now
  // carries the spec's server prefix as well, and comparing `/api/v1/repos/x`
  // against the spec's `/repos/{owner}` would report every path as a
  // substitution mismatch.
  const withoutBase = String(resolvedPath || '').split('?')[0];
  const cleanResolved = (withoutBase.startsWith(REQUEST_BASE)
    ? withoutBase.slice(REQUEST_BASE.length)
    : withoutBase.replace(/^https?:\/\/[^/]+/, '')
  ).replace(/\/+$/g, '');

  if (!cleanRaw.includes('{') && !cleanRaw.includes('}')) return false;
  const rawSegments = cleanRaw.split('/').filter(Boolean);
  const resolvedSegments = cleanResolved.split('/').filter(Boolean);
  if (rawSegments.length !== resolvedSegments.length) return true;

  const byName = {
    ...(pathParams || {}),
  };

  for (let i = 0; i < rawSegments.length; i++) {
    const left = rawSegments[i];
    const right = resolvedSegments[i];
    const match = left.match(/^\{(.+)\}$/);
    if (match) {
      const key = match[1].trim();
      const expected = encodeURIComponent(byName[key] ?? sampleForParam(key));
      if (right !== expected) {
        checks.push(`❌ ${method.toUpperCase()} ${rawPath}: parameter substitution mismatch, {${key}} -> ${right || '(missing)'}, expected ${expected}`);
        return true;
      }
      continue;
    }

    if (left !== right) {
      checks.push(`❌ ${method.toUpperCase()} ${rawPath}: path segment mismatch`);
      return true;
    }
  }

  return false;
}

function shouldInclude(method) {
  return ['get', 'post', 'put', 'patch', 'delete', 'head', 'options'].includes(method);
}

async function requestWithTimeout(url, init) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(url, {
      ...init,
      signal: controller.signal,
    });
    return { ok: true, response: res };
  } catch (err) {
    return { ok: false, error: err };
  } finally {
    clearTimeout(timer);
  }
}

async function ensureToken() {
  const username = `smoke_${Date.now()}_${Math.floor(Math.random() * 10000)}`;
  const email = `${username}@example.com`;
  const password = 'Qz7$wRtm';
  const registerPayload = {
    username,
    email,
    password,
  };

  const registerUrl = `${API_BASE}/users/register`;
  const regResp = await requestWithTimeout(registerUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(registerPayload),
  });

  if (!regResp.ok) {
    checks.push(`⚠️ User registration failed: ${regResp.error.message}`);
    return null;
  }

  if (regResp.response.status === 201 || regResp.response.status === 200) {
    const body = await regResp.response.json().catch(() => ({}));
    if (body?.token) return body.token;
  }

  const loginResp = await requestWithTimeout(`${API_BASE}/users/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: username, password }),
  });

  if (!loginResp.ok) {
    checks.push(`⚠️ Failed to generate token: ${loginResp.error.message}`);
    return null;
  }

  if (loginResp.response.status < 400) {
    const loginBody = await loginResp.response.json().catch(() => ({}));
    return loginBody?.token || null;
  }

  checks.push(`⚠️ Failed to generate token: HTTP ${loginResp.response.status}`);
  return null;
}

function isFailureStatus(status) {
  return status >= 500;
}

/**
 * Resolve the base every advertised path is relative to, from the spec itself.
 *
 * A relative `servers` URL is resolved by the consumer against the document's
 * own origin (OpenAPI 3.0 §4.7.5), which is what this reproduces; an absolute
 * one is honoured as written, so a spec that names a public hostname is
 * replayed against that hostname rather than silently against BACKEND_URL.
 *
 * A document with no `servers` is the defect this script exists to catch, not a
 * missing feature to work around — so it stops here instead of falling back to
 * the origin and replaying 288 requests at the SPA.
 */
function resolveRequestBase(doc) {
  const servers = Array.isArray(doc.servers) ? doc.servers : [];
  const url = String(servers[0]?.url || '').trim().replace(/\/+$/g, '');
  if (!url) {
    console.log(
      '❌ The OpenAPI document declares no servers(...), so it does not say where its paths are served.\n' +
        '   Every path in it is then resolved against the document origin, which the REST router does not\n' +
        '   claim — the SPA fallback answers instead. Declare the mount prefix in the servers(...) entry of\n' +
        '   #[openapi(...)] in crates/rg-http/src/openapi.rs.',
    );
    process.exit(1);
  }
  if (/^https?:\/\//i.test(url)) return url;
  return `${BACKEND_URL}${url.startsWith('/') ? url : `/${url}`}`;
}

/**
 * What a response looks like to the router question: status plus media type.
 *
 * Enough to tell a routed answer from the fallback (`200 text/html` with a
 * bundle on disk, `404 text/plain` without one) and never enough to confuse two
 * routed answers with each other — every documented endpoint answers JSON,
 * including its errors (`AppError` in crates/rg-http/src/error.rs).
 */
function responseSignature(res) {
  const type = String(res.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
  return `${res.status} ${type || '<no content-type>'}`;
}

/**
 * The signature of "this server routes nothing here", measured per method.
 *
 * Per method because the fallback is not one handler: `ServeDir` serves the
 * bundle for GET and rejects the other verbs, so a POST to an absent path
 * answers differently from a GET to the same path. Calibrating once with GET
 * and comparing a POST against it would clear every POST in the spec.
 */
const fallbackSignatures = new Map();
async function fallbackSignatureFor(method) {
  const cached = fallbackSignatures.get(method);
  if (cached !== undefined) return cached;

  const probe = await requestWithTimeout(`${REQUEST_BASE}${ABSENT_PATH}`, { method: method.toUpperCase() });
  if (!probe.ok || !probe.response) {
    console.log(`❌ Could not probe ${method.toUpperCase()} ${ABSENT_PATH}: ${probe.error?.message || 'network error'}`);
    process.exit(1);
  }
  const signature = responseSignature(probe.response);
  fallbackSignatures.set(method, signature);
  return signature;
}

console.log('Full interface smoke test started');
console.log(`backend: ${BACKEND_URL}`);
console.log(`openapi: ${OPENAPI_URL}`);

let token = OPENAPI_TOKEN;
let response = await requestWithTimeout(OPENAPI_URL, {
  method: 'GET',
  ...(OPENAPI_TOKEN ? { headers: { authorization: `Bearer ${OPENAPI_TOKEN}` } } : {}),
});

const openapiStatus = response.response?.status ?? 0;
if (response.ok && response.response?.ok) {
  checks.push(`❌ /api-docs/openapi.json is accessible without authentication (HTTP ${openapiStatus})`);
  if (OPENAPI_REQUIRE_AUTH) {
    process.exit(1);
  }
  checks.push('ℹ️ Doc authentication is disabled, continuing to retest anonymously');
}

const unauthorized = response.ok && response.response?.status === 401;
if (unauthorized) {
  checks.push('✅ /api-docs/openapi.json returned 401, matching the doc authentication expectation');
  checks.push('⚠️ Attempting to auto-generate an auth token and retry');
  token = await ensureToken();
  if (!token) {
    console.log('❌ OpenAPI docs require authentication, but no usable token could be generated');
    process.exit(1);
  }
  response = await requestWithTimeout(OPENAPI_URL, {
    method: 'GET',
    headers: { authorization: `Bearer ${token}` },
  });
}

if (!response.ok || !response.response) {
  console.log(`❌ Failed to read the OpenAPI spec: ${response.error?.message || `HTTP ${response.response?.status || 'network error'}`}`);
  process.exit(1);
}

if (!response.response.ok) {
  console.log(`❌ OpenAPI spec returned an error: HTTP ${response.response.status}`);
  process.exit(1);
}

const uiUnauthResp = await requestWithTimeout(`${BACKEND_URL}/api-docs/`, { method: 'GET' });
const uiUnauthStatus = uiUnauthResp.response?.status ?? 0;
if (OPENAPI_REQUIRE_AUTH) {
  if (!uiUnauthResp.ok || uiUnauthStatus !== 401) {
    console.log(`❌ /api-docs/ authentication behaviour is abnormal: HTTP ${uiUnauthStatus || 'network error'}`);
    process.exit(1);
  }
  checks.push('✅ /api-docs/ returned 401, matching the authentication expectation');
} else if (uiUnauthResp.ok && uiUnauthResp.response) {
  checks.push(`ℹ️ /api-docs/ is directly accessible (HTTP ${uiUnauthResp.response.status})`);
}

const openapi = await response.response.json().catch(() => ({}));
const paths = openapi.paths || {};
const components = openapi.components || {};
const openapiDoc = { ...openapi, components };

REQUEST_BASE = resolveRequestBase(openapi);
checks.push(`✅ Spec declares its own base: requests go to ${REQUEST_BASE}`);

// The base this script bootstraps with — `ensureToken` has to register a user
// before it can read the spec that says where registration lives — is the same
// prefix, and now that the spec declares one the two can be compared instead of
// both being asserted by hand.
if (REQUEST_BASE !== API_BASE) {
  console.log(
    `❌ The spec is served under ${REQUEST_BASE}, but this script bootstraps its token against ${API_BASE}.\n` +
      '   One of the two is stale. Fix the servers(...) entry in crates/rg-http/src/openapi.rs or API_BASE here.',
  );
  process.exit(1);
}

// ── What the document says about access ───────────────────────────────────
//
// Asserted before either pass, so the routing-only run CI does is the one that
// notices. A routing probe cannot see this half of the contract: it sends no
// token, and a 401 counts as "routed" — which is exactly how the document went
// its whole life declaring no `securitySchemes` and no per-operation `security`
// at all, telling every reader the entire API was anonymous (card_018b2dd39652).
//
// `security` is derived from the access level each route declares in the
// `RouteTable`; the mechanical comparison of the two lives in the build that can
// read both sides, `crates/rg-http/tests/integration/openapi_security_guard.rs`.
// What is left for here is what only a live document shows: that the derivation
// reached the wire, and that every scheme an operation names is one the document
// actually defines — a requirement pointing at an undefined scheme is invalid
// OpenAPI, and Swagger UI answers it by showing no field at all.
const securitySchemes = Object.keys(components.securitySchemes || {});
const operationEntries = Object.values(paths)
  .flatMap((item) => Object.entries(item || {}))
  .filter(([method, operation]) => shouldInclude(String(method).toLowerCase())
    && operation && typeof operation === 'object');
const declaringSecurity = operationEntries.filter(([, operation]) => shouldAuth(operation, openapiDoc));
const namedSchemes = new Set(
  operationEntries
    .flatMap(([, operation]) => (Array.isArray(operation.security) ? operation.security : []))
    .flatMap((requirement) => Object.keys(requirement || {})),
);
const undefinedSchemes = [...namedSchemes].filter((name) => !securitySchemes.includes(name));

if (securitySchemes.length === 0) {
  console.log(
    '❌ The published document declares no components.securitySchemes, so it says the whole API is\n' +
      '   anonymous: Swagger UI has no "Authorize" button and a generated client has no field for a\n' +
      '   token. Check that #[openapi(...)] in crates/rg-http/src/openapi.rs still carries\n' +
      '   modifiers(&SecurityAddon).',
  );
  process.exit(1);
}
if (declaringSecurity.length === 0) {
  console.log(
    `❌ The document defines ${securitySchemes.length} security scheme(s) but not one of its\n` +
      `   ${operationEntries.length} operations requires any of them. The per-operation derivation\n` +
      '   (openapi::stamp_security, fed by the route table) did not reach the served document.',
  );
  process.exit(1);
}
if (undefinedSchemes.length > 0) {
  console.log(
    `❌ Operations require security scheme(s) the document never defines: ${undefinedSchemes.join(', ')}.\n` +
      `   Defined: ${securitySchemes.join(', ') || '<none>'}. A client cannot satisfy a requirement it\n` +
      '   cannot look up.',
  );
  process.exit(1);
}
checks.push(
  `✅ Access is documented: ${declaringSecurity.length}/${operationEntries.length} operations require ` +
    `one of ${securitySchemes.join(', ')}`,
);

// ── Pass 1: every advertised path is routed ───────────────────────────────
//
// Anonymous and GET-only, so it mutates nothing and can run against any live
// instance. A 401, 403, 404 or 405 all count as routed: each of them can only
// come from the router or a handler, never from the fallback that answers a
// path no route claims.

const fallbackGet = await fallbackSignatureFor('get');
const controlResp = await requestWithTimeout(`${REQUEST_BASE}${ROUTED_CONTROL_PATH}`, { method: 'GET' });
if (!controlResp.ok || !controlResp.response) {
  console.log(`❌ Could not probe the routing control GET ${ROUTED_CONTROL_PATH}: ${controlResp.error?.message || 'network error'}`);
  process.exit(1);
}
const controlSignature = responseSignature(controlResp.response);
if (controlSignature === fallbackGet) {
  console.log(
    `❌ The routing check cannot discriminate: a mounted path (GET ${ROUTED_CONTROL_PATH}) and an absent one\n` +
      `   (GET ${ABSENT_PATH}) both answer "${controlSignature}". Either ${ROUTED_CONTROL_PATH} stopped being\n` +
      '   mounted, or the fallback changed shape — until this is fixed the pass below would assert nothing.',
  );
  process.exit(1);
}
checks.push(`✅ Routing probe calibrated: unrouted answers "${fallbackGet}", the control answers "${controlSignature}"`);

let unrouted = 0;
for (const [rawPath, item] of Object.entries(paths)) {
  const methods = item || {};
  const firstOperation = Object.values(methods).find((op) => op && typeof op === 'object') || {};
  const params = buildParamsFromSpec(firstOperation, methods, openapiDoc);
  const url = buildPath(rawPath, params.path);

  const probe = await requestWithTimeout(url, { method: 'GET' });
  if (!probe.ok || !probe.response) {
    checks.push(`❌ GET ${url}: ${probe.error.message}`);
    failed += 1;
    continue;
  }

  if (responseSignature(probe.response) === fallbackGet) {
    checks.push(
      `❌ ${rawPath}: advertised by the spec but no route claims ${url} — the request fell through to the ` +
        `fallback (${fallbackGet}). A client generated from this spec goes to the same place.`,
    );
    failed += 1;
    unrouted += 1;
  }
}
checks.push(`✅ Routing pass: ${Object.keys(paths).length - unrouted}/${Object.keys(paths).length} advertised paths are routed`);

if (ROUTING_ONLY) {
  for (const line of checks) console.log(line);
  if (failed > 0) {
    console.log(`\n❌ OpenAPI routing check failed: ${failed} problem(s) across ${Object.keys(paths).length} advertised paths`);
    process.exit(1);
  }
  console.log(`\n✅ OpenAPI routing check passed: ${Object.keys(paths).length} advertised paths are routed`);
  process.exit(0);
}

// ── Pass 2: replay every operation ────────────────────────────────────────
//
// Mutating: it registers a user, sends sampled bodies and calls the delete
// verbs. For a throwaway instance, not a populated one.

if (!token) {
  token = await ensureToken();
}

// How many operations will actually carry the token, counted rather than
// claimed. `shouldAuth` goes by what the document declares, which is why this
// number was zero for as long as the document declared nothing: every protected
// endpoint was replayed anonymously and the pass below stopped at the 401 wall
// instead of reaching the handler (card_018b2dd39652). It used to print
// "protected endpoints will be replayed with an auth header" unconditionally,
// which was the pleasant version of the same fact.
//
// The token is a session token, so the operations requiring `foreignToken` — a
// runner token, a CI job token, an LFS action token — are counted in but are
// still answered at their gate. That is a property of those endpoints, not a
// gap here: the document names a different credential for them, and this script
// has none to offer.
const authed = Object.values(paths)
  .flatMap((item) => Object.entries(item || {}))
  .filter(([method, operation]) => shouldInclude(String(method).toLowerCase())
    && !SKIP_METHODS.has(String(method).toLowerCase())
    && shouldAuth(operation, openapiDoc)).length;

if (!token) {
  checks.push('⚠️ No token could be generated; every endpoint runs anonymously (many will return 401)');
} else if (authed === 0) {
  checks.push(
    '⚠️ A token was generated but the spec declares no security for any operation, so none of the replay '
      + 'below carries it — protected endpoints are exercised only as far as their auth gate',
  );
} else {
  checks.push(`✅ JWT token generated; ${authed} operation(s) declare security and will carry an auth header`);
}

const entries = Object.entries(paths);
let total = 0;

for (const [rawPath, item] of entries) {
  const methods = item || {};
  for (const [method, operation] of Object.entries(methods)) {
    const lower = String(method).toLowerCase();
    if (!shouldInclude(lower)) continue;
    if (SKIP_METHODS.has(lower)) continue;

    total += 1;
    const params = buildParamsFromSpec(operation, methods, openapiDoc);
    const resolvedPath = buildPath(rawPath, params.path);
    const query = buildQueryFromParams(params.query);
    const headers = {
      ...((shouldAuth(operation, openapiDoc) && token) ? { authorization: `Bearer ${token}` } : {}),
    };

    const body = shouldInclude(lower) && lower !== 'get' && lower !== 'head' && lower !== 'options'
      ? buildPayloadForOperation(operation, openapiDoc)
      : null;

    if (body && body !== '{}') {
      headers['content-type'] = 'application/json';
    }
    const url = `${resolvedPath}${query}`;

    const req = await requestWithTimeout(url, {
      method: lower.toUpperCase(),
      headers: Object.keys(headers).length > 0 ? headers : undefined,
      body,
    });

    if (isParameterMismatch(lower, rawPath, resolvedPath, params.path)) failed += 1;

    if (!req.ok) {
      checks.push(`❌ ${lower.toUpperCase()} ${resolvedPath}: ${req.error.message}`);
      failed += 1;
      continue;
    }

    const fallbackForMethod = await fallbackSignatureFor(lower);
    if (responseSignature(req.response) === fallbackForMethod) {
      checks.push(
        `❌ ${lower.toUpperCase()} ${resolvedPath}: not routed — answered like an absent path ` +
          `(${fallbackForMethod}), so the spec advertises an operation the server does not serve.`,
      );
      failed += 1;
    } else if (isFailureStatus(req.response.status)) {
      checks.push(`❌ ${lower.toUpperCase()} ${resolvedPath}: HTTP ${req.response.status}`);
      failed += 1;
    } else {
      checks.push(`✅ ${lower.toUpperCase()} ${resolvedPath}: HTTP ${req.response.status}`);
    }
  }
}

for (const line of checks) {
  console.log(line);
}

if (failed > 0) {
  console.log(`\n❌ OpenAPI interface smoke test failed: ${failed}/${total} errors`);
  process.exit(1);
}

console.log(`\n✅ OpenAPI interface smoke test passed: ${total} requested`);
process.exit(0);
