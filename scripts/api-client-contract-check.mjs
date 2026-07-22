#!/usr/bin/env node

import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const OPENAPI_SPEC_FILE = process.env.OPENAPI_SPEC_FILE || process.env.OPENAPI_SPEC_PATH || '';
const OPENAPI_SOURCE_DIR = process.env.OPENAPI_SOURCE_DIR || 'crates/rg-http/src';
const OPENAPI_BASE_PATH = process.env.OPENAPI_BASE_PATH || '/api/v1';
const OPENAPI_URL = `${BACKEND_URL}/api-docs/openapi.json`;
const CLIENT_SOURCE = process.env.CLIENT_FILES || 'web/src/lib/api';
const STRICT = String(process.env.CLIENT_CONTRACT_STRICT || '1') === '1';

const ISSUE = {
  count: 0,
  lines: [],
};

const OPENAPI_BASE_PATH_CANON = normalizeBasePath(OPENAPI_BASE_PATH);

function requestWithTimeout(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 12000);
  return fetch(url, { signal: controller.signal })
    .then((res) => ({ ok: true, response: res }))
    .catch((error) => ({ ok: false, error }))
    .finally(() => clearTimeout(timer));
}

function normalizeBasePath(raw) {
  const trimmed = String(raw || '').trim().replace(/\/+$/g, '');
  if (!trimmed || trimmed === '/') return '';
  return trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
}

function normalizeApiPath(pathSource) {
  const src = String(pathSource || '').trim();
  if (!src) return '/';
  const normalized = src.startsWith('/') ? src : `/${src}`;
  const noQuery = normalized.split('?')[0];
  const noTrailing = noQuery.replace(/\/+$/g, '') || '/';

  if (!OPENAPI_BASE_PATH_CANON) return noTrailing;
  if (noTrailing === OPENAPI_BASE_PATH_CANON) return '/';
  if (noTrailing.startsWith(`${OPENAPI_BASE_PATH_CANON}/`)) {
    return noTrailing.slice(OPENAPI_BASE_PATH_CANON.length);
  }

  return noTrailing;
}

function readLocalOpenApi() {
  if (!OPENAPI_SPEC_FILE) return null;
  try {
    const raw = readFileSync(OPENAPI_SPEC_FILE, 'utf8');
    return JSON.parse(raw);
  } catch (error) {
    console.log(`❌ 读取本地 OpenAPI 文件失败: ${OPENAPI_SPEC_FILE} -> ${error?.message || String(error)}`);
    return null;
  }
}

function readBalancedBlock(source, start, openChar, closeChar) {
  let i = start;
  let depth = 0;
  let inString = null;
  let escaped = false;

  while (i < source.length) {
    const char = source[i];

    if (inString) {
      if (escaped) {
        escaped = false;
        i += 1;
        continue;
      }
      if (char === '\\') {
        escaped = true;
        i += 1;
        continue;
      }
      if (char === inString) {
        inString = null;
      }
      i += 1;
      continue;
    }

    if (char === '"' || char === "'" || char === '`') {
      inString = char;
      i += 1;
      continue;
    }

    if (char === openChar) {
      depth += 1;
      i += 1;
      continue;
    }

    if (char === closeChar) {
      if (depth > 0) {
        depth -= 1;
        i += 1;
        if (depth === 0) {
          return { text: source.slice(start, i), end: i };
        }
        continue;
      }
      i += 1;
      continue;
    }

    if (char === '$' && source[i + 1] === '{' && closeChar === '}' && openChar === '{') {
      const expr = readTemplateExpr(source, i + 2);
      i = (expr?.next ?? (source.length - 1)) + 1;
      continue;
    }

    i += 1;
  }

  return null;
}

function readTemplateExpr(source, start) {
  let i = start;
  let depth = 1;
  let inString = null;
  let escaped = false;

  while (i < source.length) {
    const char = source[i];
    if (inString) {
      if (escaped) {
        escaped = false;
        i += 1;
        continue;
      }
      if (char === '\\') {
        escaped = true;
        i += 1;
        continue;
      }
      if (char === inString) {
        inString = null;
      }
      i += 1;
      continue;
    }

    if (char === '"' || char === "'" || char === '`') {
      inString = char;
      i += 1;
      continue;
    }

    if (char === '{') {
      depth += 1;
      i += 1;
      continue;
    }

    if (char === '}') {
      depth -= 1;
      if (depth === 0) {
        return { expr: source.slice(start, i), next: i };
      }
      i += 1;
      continue;
    }

    if (char === '$' && source[i + 1] === '{') {
      depth += 1;
      i += 2;
      continue;
    }

    i += 1;
  }

  return { expr: source.slice(start), next: source.length - 1 };
}

function readStringLiteral(source, start) {
  const quote = source[start];
  if (!quote || !['"', "'", '`'].includes(quote)) return null;

  if (quote === "'" || quote === '"') {
    let i = start + 1;
    let escaped = false;
    while (i < source.length) {
      const char = source[i];
      if (escaped) {
        escaped = false;
        i += 1;
        continue;
      }
      if (char === '\\') {
        escaped = true;
        i += 1;
        continue;
      }
      if (char === quote) {
        return { value: source.slice(start + 1, i), end: i + 1 };
      }
      i += 1;
    }
    return null;
  }

  let i = start + 1;
  let escaped = false;
  let inString = null;
  let exprDepth = 0;

  while (i < source.length) {
    const char = source[i];
    if (inString) {
      if (escaped) {
        escaped = false;
        i += 1;
        continue;
      }
      if (char === '\\') {
        escaped = true;
        i += 1;
        continue;
      }
      if (char === inString) {
        inString = null;
        i += 1;
        continue;
      }
      i += 1;
      continue;
    }

    if (escaped) {
      escaped = false;
      i += 1;
      continue;
    }

    if (char === '\\') {
      escaped = true;
      i += 1;
      continue;
    }

    if (char === '$' && source[i + 1] === '{') {
      exprDepth += 1;
      i += 2;
      continue;
    }

    if (exprDepth > 0) {
      if (char === '"' || char === "'" || char === '`') {
        inString = char;
        i += 1;
        continue;
      }
      if (char === '{') {
        exprDepth += 1;
        i += 1;
        continue;
      }
      if (char === '}') {
        exprDepth -= 1;
        i += 1;
        continue;
      }
      i += 1;
      continue;
    }

      if (char === '`') {
        return { value: source.slice(start + 1, i), end: i + 1 };
      }

    if (char === '"' || char === "'") {
      inString = char;
      i += 1;
      continue;
    }

    i += 1;
  }

  return null;
}

function skipWhitespace(source, index) {
  let i = index;
  while (i < source.length && /\s/.test(source[i])) {
    i += 1;
  }
  return i;
}

function isIdentifierChar(char) {
  return /[A-Za-z0-9_$]/.test(String(char));
}

function unwrapParamExpression(expression) {
  const wrappers = new Set([
    'String',
    'Number',
    'encodeURIComponent',
    'encodeRepoPath',
    'decodeURIComponent',
    'normalize',
  ]);
  let current = String(expression || '').trim();
  let changed = true;

  while (changed) {
    changed = false;
    const m = current.match(/^([A-Za-z_$][A-Za-z0-9_$]*)\(([\s\S]*)\)$/);
    if (!m) break;
    if (!wrappers.has(m[1])) break;
    current = m[2].trim();
    changed = true;
  }

  return current;
}

function isQueryLikeTemplateExpression(expr) {
  const text = String(expr || '').trim();
  if (!text) return true;
  if (/\bqs\s*\(/.test(text)) return true;
  if (text.includes('?') && text.includes(':')) return true;
  return false;
}

function normalizeParamExpr(expr) {
  const text = unwrapParamExpression(expr).trim();
  const m = text.match(/([A-Za-z_$][A-Za-z0-9_$]*)$/);
  if (m) return m[1];
  return 'param';
}

function normalizeTemplateExpression(expr, prevChar, nextChar) {
  const text = String(expr || '').trim();
  const previous = prevChar || '';
  const next = nextChar || '';
  const keepAsPathSegment =
    (previous === '/' || previous === '') &&
    (next === '/' || next === '?' || next === '#' || next === '' || next === '&' || next === ';' || next === '$');

  if (!keepAsPathSegment) return '';
  if (isQueryLikeTemplateExpression(text)) return '';
  return normalizeParamExpr(text);
}

function normalizeTemplatePath(pathSource) {
  const raw = String(pathSource || '')
    .trim()
    .replace(/\s+/g, '');

  if (!raw) return '/';
  let src = raw.startsWith('/') ? raw : `/${raw}`;
  let out = '';

  for (let i = 0; i < src.length; i += 1) {
    const char = src[i];
    if (char === '$' && src[i + 1] === '{') {
      const expr = readTemplateExpr(src, i + 2);
      const nextIndex = expr?.next ?? (src.length - 1);
      const restAfterExpr = src.slice(nextIndex + 1);
      const isQueryExpr = /^\s*\$\{\s*qs\s*\(/.test(restAfterExpr);
      const nextChar = isQueryExpr ? '?' : (restAfterExpr[0] || '');
      const prevChar = src[i - 1] || '';
      const key = normalizeTemplateExpression(expr?.expr || '', prevChar, nextChar);
      if (key) {
        out += `{${key}}`;
      }
      i = nextIndex;
      continue;
    }

    out += char;
  }

  const collapsed = out.replace(/\/{2,}/g, '/');
  return normalizeApiPath(collapsed);
}

function normalizeOpenApiPath(pathSource) {
  return normalizeApiPath(String(pathSource || '').trim());
}

function collectClientFilesFromDir(targetDir, files) {
  const entries = readdirSync(targetDir, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.name === 'target' || entry.name.startsWith('.')) {
      continue;
    }

    const full = path.join(targetDir, entry.name);
    if (entry.isDirectory()) {
      collectClientFilesFromDir(full, files);
      continue;
    }

    if (entry.isFile() && /\.(ts|svelte\.ts)$/.test(entry.name)) {
      files.add(full);
    }
  }
}

function resolveClientSources(rawSources) {
  const sources = String(rawSources || CLIENT_SOURCE)
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean);
  const set = new Set();

  for (const source of sources) {
    const abs = path.isAbsolute(source) ? source : path.resolve(process.cwd(), source);
    let stat;
    try {
      stat = statSync(abs);
    } catch (error) {
      console.log(`⚠️  无法读取前端客户端路径: ${abs}`);
      continue;
    }

    if (stat.isFile()) {
      if (/\.(ts|svelte\.ts)$/.test(abs)) {
        set.add(abs);
      }
      continue;
    }

    if (!stat.isDirectory()) {
      console.log(`⚠️  CLIENT_FILES 条目不是文件/目录: ${abs}`);
      continue;
    }

    collectClientFilesFromDir(abs, set);
  }

  const list = [...set];
  const files = [];
  for (const file of list) {
    if (/\.(ts|svelte\.ts)$/.test(file)) {
      files.push(file);
    }
  }

  if (files.length === 0) {
    console.log(`⚠️  未在 ${rawSources || CLIENT_SOURCE} 发现可扫描的 API 客户端文件`);
  }

  return files.sort();
}

function loadOpenApiFromRustSource() {
  const paths = {};
  const files = [];
  let rootStat;

  try {
    rootStat = statSync(OPENAPI_SOURCE_DIR);
  } catch (error) {
    console.log(`⚠️  OpenAPI 源码目录不可读: ${OPENAPI_SOURCE_DIR}`);
    return null;
  }

  if (!rootStat.isDirectory()) {
    console.log(`⚠️  OpenAPI_SOURCE_DIR 需要是目录: ${OPENAPI_SOURCE_DIR}`);
    return null;
  }

  function walk(currentDir) {
    const entries = readdirSync(currentDir, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.name.startsWith('.')) {
        continue;
      }
      const full = path.join(currentDir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === 'target') {
          continue;
        }
        walk(full);
        continue;
      }
      if (entry.isFile() && entry.name.endsWith('.rs')) {
        files.push(full);
      }
    }
  }

  walk(OPENAPI_SOURCE_DIR);

  if (files.length === 0) {
    console.log(`⚠️  未在 ${OPENAPI_SOURCE_DIR} 发现 .rs 文件`);
    return null;
  }

  for (const file of files) {
    const src = readFileSync(file, 'utf8');
    let cursor = 0;
    const startToken = '#[utoipa::path(';
    while (true) {
      const start = src.indexOf(startToken, cursor);
      if (start === -1) break;

      let i = start + startToken.length;
      let depth = 1;
      while (i < src.length && depth > 0) {
        if (src[i] === '(') {
          depth += 1;
        } else if (src[i] === ')') {
          depth -= 1;
        }
        i += 1;
      }

      const body = src.slice(start + startToken.length, i - 1);
      cursor = i;

      const methodMatch = body.match(/^\s*([a-zA-Z]+)\s*,/);
      const pathMatch = body.match(/path\s*=\s*['"]([^'\"]+)['"]/);
      if (!methodMatch || !pathMatch) {
        continue;
      }

      const method = methodMatch[1].toLowerCase();
      const normalizedPath = normalizeOpenApiPath(pathMatch[1]);
      if (!normalizedPath) continue;
      paths[normalizedPath] = paths[normalizedPath] || {};
      paths[normalizedPath][method] = {
        requestBody: null,
      };
    }
  }

  return Object.keys(paths).length > 0 ? { paths } : null;
}

function splitSegments(pathSource) {
  return String(pathSource || '')
    .split('?')[0]
    .replace(/\/+$/g, '')
    .split('/')
    .filter(Boolean);
}

function isParamSegment(segment) {
  return segment.startsWith('{') && segment.endsWith('}');
}

function extractParamNames(pathSource) {
  return splitSegments(pathSource)
    .filter(isParamSegment)
    .map((segment) => segment.slice(1, -1).replace(/^\*+/, ''));
}

function toSnakeCase(value) {
  return String(value || '')
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/-/g, '_')
    .toLowerCase();
}

const PARAM_EQUIVALENT_GROUPS = [
  ['repo', 'name'],
  ['format', 'type'],
  ['issue_number', 'number'],
  ['pull_number', 'number'],
  ['pipeline_id', 'id'],
  ['job_id', 'id'],
  ['board_id', 'id'],
  ['col_id', 'col'],
  ['card_id', 'card'],
  ['user_id', 'user'],
  ['rev_id', 'rev'],
];

const PARAM_EQUIVALENCE = new Map();
for (const group of PARAM_EQUIVALENT_GROUPS) {
  for (const rawKey of group) {
    const key = toSnakeCase(rawKey);
    PARAM_EQUIVALENCE.set(key, new Set(group.map((value) => toSnakeCase(value))));
  }
}

const KNOWN_BODY_CONTRACTS = [
  {
    method: 'post',
    path: '/repos/{owner}/{repo}/pulls',
    required: ['title', 'head', 'base'],
    forbidden: ['head_branch', 'base_branch'],
  },
  {
    method: 'post',
    path: '/repos/{owner}/{repo}/pulls/{number}/reviews',
    required: ['action'],
    forbidden: ['verdict'],
  },
];

function equivalentParam(left, right) {
  const a = toSnakeCase(left);
  const b = toSnakeCase(right);
  if (a === b) return true;
  const aSet = PARAM_EQUIVALENCE.get(a);
  if (aSet?.has(b)) return true;
  const bSet = PARAM_EQUIVALENCE.get(b);
  if (bSet?.has(a)) return true;
  return false;
}

function countStaticSegments(pathSource) {
  return splitSegments(pathSource).filter((segment) => !isParamSegment(segment)).length;
}

function matchPathPatternScore(clientPath, openapiPath, options = {}) {
  const preferStaticCount = options.preferStaticCount ?? 0;
  const a = splitSegments(openapiPath);
  const b = splitSegments(clientPath);
  if (a.length !== b.length) return null;

  let staticMatch = 0;
  let paramMatch = 0;

  for (let i = 0; i < a.length; i += 1) {
    const left = a[i];
    const right = b[i];
    const leftParam = isParamSegment(left);
    const rightParam = isParamSegment(right);
    if (!leftParam && !rightParam) {
      if (left !== right) return null;
      staticMatch += 1;
      continue;
    }

    if (leftParam || rightParam) {
      if (leftParam !== rightParam) {
        return null;
      }
      paramMatch += 1;
      continue;
    }
  }

  if (staticMatch < preferStaticCount) {
    return null;
  }

  return { staticMatch, paramMatch };
}

function matchPathPattern(clientPath, openapiPath) {
  const clientStatic = countStaticSegments(clientPath);
  return matchPathPatternScore(clientPath, openapiPath, { preferStaticCount: clientStatic }) !== null;
}

function findOpenApiMatch(openapiPaths, clientMethod, clientPath) {
  const clientStatic = countStaticSegments(clientPath);
  const matches = [];
  for (const [candidatePath, item] of Object.entries(openapiPaths || {})) {
    if (!item || typeof item !== 'object') continue;
    if (!item[clientMethod]) continue;
    const score = matchPathPatternScore(clientPath, candidatePath, { preferStaticCount: clientStatic });
    if (!score) continue;
    matches.push({
      path: candidatePath,
      staticMatch: score.staticMatch,
      paramMatch: score.paramMatch,
    });
  }

  matches.sort((x, y) => y.staticMatch - x.staticMatch || x.paramMatch - y.paramMatch);
  return matches.map((entry) => entry.path);
}

function extractBody(cfg) {
  if (!cfg) return { present: false, keys: [], dynamic: false };
  const trimmed = cfg.replace(/\n/g, ' ');
  const present = /\bbody:\s*/.test(trimmed);
  if (!present) return { present: false, keys: [], dynamic: false };

  const bodyRe = /body:\s*JSON\.stringify\(\s*\{([\s\S]*?)\}\s*\)/i;
  const m = trimmed.match(bodyRe);
  if (!m || !m[1]) {
    return { present: true, keys: [], dynamic: true };
  }

  const body = m[1];
  const keyRe = /([A-Za-z_][A-Za-z0-9_]*)\s*:/g;
  const keys = [];
  let km;
  while ((km = keyRe.exec(body)) !== null) {
    keys.push(km[1]);
  }
  return { present: true, keys, dynamic: false };
}

function inspectBodyAlignment(operation, bodyKeys) {
  const reqBody = operation?.requestBody;
  if (!reqBody?.content) return { ok: true, details: null };
  const content = reqBody.content['application/json'] || reqBody.content['application/problem+json'];
  const schema = content?.schema;
  if (!schema || !schema.properties) return { ok: true, details: null };
  const required = schema.required || [];
  if (required.length === 0) return { ok: true, details: null };
  if (!bodyKeys.present) {
    return { ok: false, details: '未检测到 body，请确认请求体是否已上传' };
  }
  if (bodyKeys.dynamic) return { ok: true, details: null };

  const missing = required.filter((name) => !bodyKeys.keys.includes(name));
  if (missing.length === 0) return { ok: true, details: null };
  return { ok: false, details: `缺少 required body 字段: ${missing.join(',')}` };
}

function inspectKnownBodyContracts(method, targetPath, bodyKeys) {
  const contract = KNOWN_BODY_CONTRACTS.find((entry) => {
    return entry.method === method && matchPathPattern(targetPath, entry.path);
  });
  if (!contract) return { ok: true, details: null };

  if (!bodyKeys.present) {
    return { ok: false, details: `未检测到 body，接口需要字段: ${contract.required.join(', ')}` };
  }
  if (bodyKeys.dynamic) return { ok: true, details: null };

  const missing = contract.required.filter((name) => !bodyKeys.keys.includes(name));
  const forbidden = contract.forbidden.filter((name) => bodyKeys.keys.includes(name));
  const details = [];
  if (missing.length > 0) details.push(`缺少字段: ${missing.join(', ')}`);
  if (forbidden.length > 0) details.push(`发送了后端不接收的旧字段: ${forbidden.join(', ')}`);

  return details.length === 0 ? { ok: true, details: null } : { ok: false, details: details.join('；') };
}

function parseMethod(source, start) {
  if (source[start] !== '<') return start;

  const generic = readBalancedBlock(source, start, '<', '>');
  if (generic) {
    return generic.end;
  }
  return start;
}

function extractRequestCalls(source, file) {
  const calls = [];
  let cursor = 0;

  while (true) {
    const idx = source.indexOf('request', cursor);
    if (idx === -1) break;

    const before = source[idx - 1];
    const after = source[idx + 'request'.length];
    if ((before && isIdentifierChar(before)) || (after && isIdentifierChar(after))) {
      cursor = idx + 1;
      continue;
    }

    let i = idx + 'request'.length;
    i = skipWhitespace(source, i);

    i = parseMethod(source, i);
    i = skipWhitespace(source, i);
    if (source[i] !== '(') {
      cursor = idx + 1;
      continue;
    }
    i += 1;

    i = skipWhitespace(source, i);
    const pathArg = readStringLiteral(source, i);
    if (!pathArg) {
      cursor = i + 1;
      continue;
    }

    const targetPath = normalizeTemplatePath(pathArg.value);
    i = skipWhitespace(source, pathArg.end);

    let method = 'get';
    let body = { present: false, keys: [], dynamic: false };
    if (source[i] === ',') {
      i = skipWhitespace(source, i + 1);
      if (source[i] === '{') {
        const cfg = readBalancedBlock(source, i, '{', '}');
        if (cfg) {
          const cfgText = cfg.text;
          const methodMatch = cfgText.match(/method:\s*['"]([A-Za-z]+)['"]/i);
          if (methodMatch) {
            method = methodMatch[1].toLowerCase();
          }
          body = extractBody(cfgText);
          i = cfg.end;
        }
      }
    }

    while (i < source.length && source[i] !== ')') {
      i += 1;
    }
    if (source[i] === ')') {
      calls.push({
        method,
        path: targetPath,
        file,
        body,
      });
      cursor = i + 1;
      continue;
    }

    cursor = idx + 1;
  }

  return calls;
}

function dedupeCalls(calls) {
  const seen = new Set();
  const out = [];

  for (const call of calls) {
    const key = `${call.method}|${call.path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(call);
  }

  return out;
}

function inspectFrontendFlowContracts() {
  const packageDetailFile = path.resolve(
    process.cwd(),
    'web/src/routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte',
  );
  const authStoreFile = path.resolve(process.cwd(), 'web/src/lib/stores/auth.svelte.ts');
  const loginPageFile = path.resolve(process.cwd(), 'web/src/routes/login/+page.svelte');
  const repoHeaderFile = path.resolve(process.cwd(), 'web/src/lib/components/RepoHeader.svelte');
  const reposApiFile = path.resolve(process.cwd(), 'crates/rg-http/src/api/repos.rs');

  let packageDetailSource = '';
  try {
    packageDetailSource = readFileSync(packageDetailFile, 'utf8');
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程缺失: 无法读取包详情页（${packageDetailFile}）`);
    return;
  }

  const loadPackageMatch = packageDetailSource.match(/async function loadPackage\(\)\s*\{([\s\S]*?)\n  \}/);
  if (!loadPackageMatch) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程缺失: 包详情页未找到 loadPackage()（来源: ${packageDetailFile}）`);
    return;
  }

  if (!/\bpackages\.getVersions\(/.test(loadPackageMatch[1]) && !/\bloadVersions\(\)/.test(loadPackageMatch[1])) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程未加载版本: 包详情页 loadPackage() 没有调用版本列表接口（来源: ${packageDetailFile}）`);
  }

  let authStoreSource = '';
  let loginPageSource = '';
  try {
    authStoreSource = readFileSync(authStoreFile, 'utf8');
    loginPageSource = readFileSync(loginPageFile, 'utf8');
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程缺失: 无法读取登录/MFA 流程文件（${authStoreFile}, ${loginPageFile}）`);
    return;
  }

  const loginFunctionMatch = authStoreSource.match(/export async function login\(username: string, password: string\)\s*\{([\s\S]*?)\n\}/);
  if (!loginFunctionMatch) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程缺失: auth store 未找到 login(username, password)（来源: ${authStoreFile}）`);
  } else {
    const loginBody = loginFunctionMatch[1];
    if (!/res\.mfa_required/.test(loginBody)) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ 登录契约未处理 MFA: /users/login 可返回 mfa_required=true，但 auth store 未分支处理（来源: ${authStoreFile}）`);
    }
    const mfaIndex = loginBody.indexOf('res.mfa_required');
    const tokenIndex = loginBody.indexOf('setToken(res.token)');
    if (mfaIndex === -1 || tokenIndex === -1 || tokenIndex < mfaIndex) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ 登录契约会保存空 MFA token: setToken(res.token) 必须发生在 mfa_required 分支之后（来源: ${authStoreFile}）`);
    }
  }

  if (!/export async function verifyMfa\(/.test(authStoreSource) || !/\bauth\.verifyMfa\(/.test(authStoreSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ MFA 流程缺失: auth store 未调用 /users/mfa/verify（来源: ${authStoreFile}）`);
  }

  if (!/\bisMfaRequired\(\)/.test(loginPageSource) || !/\bverifyMfa\(/.test(loginPageSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ MFA UI 缺失: 登录页没有展示并提交二步验证码（来源: ${loginPageFile}）`);
  }

  let repoHeaderSource = '';
  try {
    repoHeaderSource = readFileSync(repoHeaderFile, 'utf8');
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 前端流程缺失: 无法读取仓库头部组件（${repoHeaderFile}）`);
    return;
  }

  if (/archive\/main\.zip/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 仓库归档链接硬编码 main: RepoHeader 必须使用仓库 default_branch 或后端返回值（来源: ${repoHeaderFile}）`);
  }

  if (!/\bdefaultBranch\b/.test(repoHeaderSource) || !/\brepos\.get\(/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 仓库归档链接未对齐默认分支: RepoHeader 应从 props 或 /repos/{owner}/{repo} 获取 default_branch（来源: ${repoHeaderFile}）`);
  }

  if (!/\bdownloadApiFile\(/.test(repoHeaderSource) || !/archive\/\$\{encodeURIComponent\(archiveRef\)\}\.zip/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 仓库归档下载未使用认证 API helper/ref 编码: RepoHeader 下载应对齐后端 /archive/{ref}.zip 并携带 Bearer auth（来源: ${repoHeaderFile}）`);
  }

  let reposApiSource = '';
  try {
    reposApiSource = readFileSync(reposApiFile, 'utf8');
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 后端契约缺失: 无法读取仓库 API 文件（${reposApiFile}）`);
    return;
  }

  const repoResponseMatch = reposApiSource.match(/pub struct RepoResponse\s*\{([\s\S]*?)\n\}/);
  if (!repoResponseMatch) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ 后端契约缺失: RepoResponse schema 未定义（来源: ${reposApiFile}）`);
    return;
  }

  for (const field of ['default_branch', 'stars_count', 'forks_count', 'fork_id']) {
    if (!new RegExp(`\\bpub\\s+${field}\\s*:`).test(repoResponseMatch[1])) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ 仓库详情响应 schema 漂移: 前端仓库页依赖 ${field}，RepoResponse 未声明（来源: ${reposApiFile}）`);
    }
  }
}

async function main() {
  let openapi = readLocalOpenApi();
  if (!openapi) {
    const sourceOpenApi = loadOpenApiFromRustSource();
    if (sourceOpenApi) {
      console.log('✅ 已使用 Rust 源码自动提取的 OpenAPI 路由信息进行对齐（离线模式）');
      openapi = sourceOpenApi;
    }
  }

  if (!openapi) {
    const openapiResp = await requestWithTimeout(OPENAPI_URL);
    if (!openapiResp.ok) {
      console.log(`❌ 无法读取 OpenAPI: ${openapiResp.error?.message || `HTTP ${openapiResp.response?.status}`}`);
      process.exit(1);
    }
    if (!openapiResp.response.ok) {
      console.log(`❌ OpenAPI 返回异常: HTTP ${openapiResp.response.status}`);
      process.exit(1);
    }
    openapi = await openapiResp.response.json().catch(() => ({}));
  }

  const rawPaths = {};
  const openapiPaths = openapi.paths || {};
  for (const [rawPath, methods] of Object.entries(openapiPaths)) {
    const normalizedPath = normalizeOpenApiPath(rawPath);
    rawPaths[normalizedPath] = rawPaths[normalizedPath] || {};
    for (const [method, operation] of Object.entries(methods || {})) {
      if (typeof operation !== 'object') continue;
      rawPaths[normalizedPath][method.toLowerCase()] = operation;
    }
  }

  const clientFiles = resolveClientSources(CLIENT_SOURCE);
  if (clientFiles.length === 0) {
    console.log('❌ 未发现可扫描的前端 API 调用点');
    process.exit(1);
  }

  const calls = [];
  for (const file of clientFiles) {
    const src = readFileSync(file, 'utf8');
    calls.push(...extractRequestCalls(src, file));
  }

  const normalizedCalls = dedupeCalls(calls);
  for (const call of normalizedCalls) {
    const method = call.method;
    const targetPath = normalizeApiPath(call.path);
    const matches = findOpenApiMatch(rawPaths, method, targetPath);
    if (matches.length === 0) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ 前端路由未对齐: ${method.toUpperCase()} ${targetPath} 在 OpenAPI 中无对应 path/method（来源: ${call.file}）`);
      continue;
    }

    const firstMatch = rawPaths[matches[0]][method];
    const apiParams = new Set(extractParamNames(matches[0]));
    const clientParams = new Set(extractParamNames(targetPath));

    for (const name of clientParams) {
      const matched = [...apiParams].some((entry) => equivalentParam(name, entry));
      if (!matched) {
        ISSUE.count += 1;
        ISSUE.lines.push(`⚠️ 参数名潜在不一致: ${method.toUpperCase()} ${targetPath}, 客户端使用 ${name}，接口定义 ${Array.from(apiParams).join(', ') || '无参数'}（来源: ${call.file})`);
      }
    }
    for (const name of apiParams) {
      const matched = [...clientParams].some((entry) => equivalentParam(name, entry));
      if (!matched) {
        ISSUE.count += 1;
        ISSUE.lines.push(`⚠️ 参数名缺失: ${method.toUpperCase()} ${targetPath}, OpenAPI 需要 {${name}} 但客户端模板未显式包含（来源: ${call.file})`);
      }
    }

    const bodyCheck = inspectBodyAlignment(firstMatch, call.body || {});
    if (!bodyCheck.ok) {
      ISSUE.count += 1;
      ISSUE.lines.push(`⚠️ 请求体参数缺失: ${method.toUpperCase()} ${targetPath} -> ${bodyCheck.details}（来源: ${call.file})`);
    }

    const knownBodyCheck = inspectKnownBodyContracts(method, targetPath, call.body || {});
    if (!knownBodyCheck.ok) {
      ISSUE.count += 1;
      ISSUE.lines.push(`⚠️ 已知请求体契约不一致: ${method.toUpperCase()} ${targetPath} -> ${knownBodyCheck.details}（来源: ${call.file})`);
    }
  }

  inspectFrontendFlowContracts();

  for (const line of ISSUE.lines) {
    console.log(line);
  }

  const totalCalls = normalizedCalls.length;
  const mismatches = ISSUE.lines.length;
  if (mismatches > 0) {
    console.log(`\n❌ 前后端接口对齐检查: ${mismatches}/${totalCalls} 条存在问题`);
  } else {
    console.log(`\n✅ 前后端接口对齐检查通过: ${totalCalls} 条前端请求`);
  }

  if (STRICT && mismatches > 0) {
    process.exit(1);
  }
}

main().catch((err) => {
  console.log(`❌ 对齐检查执行失败: ${err?.message || String(err)}`);
  process.exit(1);
});
