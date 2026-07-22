#!/usr/bin/env node

const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const API_BASE = `${BACKEND_URL}/api/v1`;
const OPENAPI_URL = `${BACKEND_URL}/api-docs/openapi.json`;
const OPENAPI_TOKEN = process.env.OPENAPI_TOKEN || null;
const OPENAPI_REQUIRE_AUTH = String(process.env.OPENAPI_REQUIRE_AUTH || '1') === '1';

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
  return `${BACKEND_URL}${path}`;
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
  const cleanResolved = String(resolvedPath || '').split('?')[0].replace(/^https?:\/\/[^/]+/, '').replace(/\/+$/g, '');

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
        checks.push(`❌ ${method.toUpperCase()} ${rawPath}: 参数替换异常，{${key}} -> ${right || '(missing)'}, 期望 ${expected}`);
        return true;
      }
      continue;
    }

    if (left !== right) {
      checks.push(`❌ ${method.toUpperCase()} ${rawPath}: 路径片段不一致`);
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
    checks.push(`⚠️ 用户注册失败: ${regResp.error.message}`);
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
    checks.push(`⚠️ 生成 Token 失败：${loginResp.error.message}`);
    return null;
  }

  if (loginResp.response.status < 400) {
    const loginBody = await loginResp.response.json().catch(() => ({}));
    return loginBody?.token || null;
  }

  checks.push(`⚠️ 生成 Token 失败：HTTP ${loginResp.response.status}`);
  return null;
}

function isFailureStatus(status) {
  return status >= 500;
}

console.log('接口全量冒烟开始');
console.log(`backend: ${BACKEND_URL}`);
console.log(`openapi: ${OPENAPI_URL}`);

let token = OPENAPI_TOKEN;
let response = await requestWithTimeout(OPENAPI_URL, {
  method: 'GET',
  ...(OPENAPI_TOKEN ? { headers: { authorization: `Bearer ${OPENAPI_TOKEN}` } } : {}),
});

const openapiStatus = response.response?.status ?? 0;
if (response.ok && response.response?.ok) {
  checks.push(`❌ /api-docs/openapi.json 未启用鉴权即可访问（HTTP ${openapiStatus}）`);
  if (OPENAPI_REQUIRE_AUTH) {
    process.exit(1);
  }
  checks.push('ℹ️ 未开启文档鉴权，继续使用匿名状态复测');
}

const unauthorized = response.ok && response.response?.status === 401;
if (unauthorized) {
  checks.push('✅ /api-docs/openapi.json 返回 401，符合文档鉴权预期');
  checks.push('⚠️ 尝试自动生成鉴权 Token 继续重试');
  token = await ensureToken();
  if (!token) {
    console.log('❌ OpenAPI 文档需鉴权，但未能生成可用 Token');
    process.exit(1);
  }
  response = await requestWithTimeout(OPENAPI_URL, {
    method: 'GET',
    headers: { authorization: `Bearer ${token}` },
  });
}

if (!response.ok || !response.response) {
  console.log(`❌ 读取 OpenAPI 规范失败: ${response.error?.message || `HTTP ${response.response?.status || 'network error'}`}`);
  process.exit(1);
}

if (!response.response.ok) {
  console.log(`❌ OpenAPI 规范返回异常: HTTP ${response.response.status}`);
  process.exit(1);
}

const uiUnauthResp = await requestWithTimeout(`${BACKEND_URL}/api-docs/`, { method: 'GET' });
const uiUnauthStatus = uiUnauthResp.response?.status ?? 0;
if (OPENAPI_REQUIRE_AUTH) {
  if (!uiUnauthResp.ok || uiUnauthStatus !== 401) {
    console.log(`❌ /api-docs/ 鉴权行为异常: HTTP ${uiUnauthStatus || 'network error'}`);
    process.exit(1);
  }
  checks.push('✅ /api-docs/ 返回 401，符合鉴权预期');
} else if (uiUnauthResp.ok && uiUnauthResp.response) {
  checks.push(`ℹ️ /api-docs/ 可直接访问（HTTP ${uiUnauthResp.response.status}）`);
}

const openapi = await response.response.json().catch(() => ({}));
const paths = openapi.paths || {};
const components = openapi.components || {};
const openapiDoc = { ...openapi, components };

if (!token) {
  token = await ensureToken();
}
if (token) {
  checks.push('✅ 已生成 JWT token，受保护接口将带鉴权头重放');
} else {
  checks.push('⚠️ 受保护接口将不带鉴权头执行（部分接口会返回 401）');
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

    if (isFailureStatus(req.response.status)) {
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
  console.log(`\n❌ OpenAPI 接口冒烟失败: ${failed}/${total} 个异常`);
  process.exit(1);
}

console.log(`\n✅ OpenAPI 接口冒烟通过: ${total} 个已请求`);
process.exit(0);
