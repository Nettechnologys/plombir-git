#!/usr/bin/env node

import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import { loadUtoipaPaths, rustStructBody } from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const OPENAPI_SPEC_FILE = process.env.OPENAPI_SPEC_FILE || process.env.OPENAPI_SPEC_PATH || '';
const OPENAPI_SOURCE_DIR = process.env.OPENAPI_SOURCE_DIR || 'crates/rg-http/src/api';
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
    console.log(`❌ Failed to read local OpenAPI file: ${OPENAPI_SPEC_FILE} -> ${error?.message || String(error)}`);
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

// Marker for a path segment the static parser cannot resolve — e.g. a segment
// produced by a URL-building helper (`request(`${buildPath(...)}/${id}`)`). Such
// a call cannot be matched against the OpenAPI routes without executing the
// helper, so we skip it rather than emit a bogus "route not aligned" failure.
const OPAQUE_SEGMENT = '__opaque__';

function normalizeParamExpr(expr) {
  const text = unwrapParamExpression(expr).trim();
  const m = text.match(/([A-Za-z_$][A-Za-z0-9_$]*)$/);
  if (m) return m[1];
  // A function call we could not unwrap (helper returning a URL fragment):
  // the resulting path is not statically resolvable.
  if (text.includes('(')) return OPAQUE_SEGMENT;
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
      console.log(`⚠️  Unable to read frontend client path: ${abs}`);
      continue;
    }

    if (stat.isFile()) {
      if (/\.(ts|svelte\.ts)$/.test(abs)) {
        set.add(abs);
      }
      continue;
    }

    if (!stat.isDirectory()) {
      console.log(`⚠️  CLIENT_FILES entry is not a file/directory: ${abs}`);
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
    console.log(`⚠️  No scannable API client files found in ${rawSources || CLIENT_SOURCE}`);
  }

  return files.sort();
}

function loadOpenApiFromRustSource() {
  const paths = {};
  let rootStat;

  try {
    rootStat = statSync(OPENAPI_SOURCE_DIR);
  } catch (error) {
    console.log(`⚠️  OpenAPI source directory is not readable: ${OPENAPI_SOURCE_DIR}`);
    return null;
  }

  if (!rootStat.isDirectory()) {
    console.log(`⚠️  OPENAPI_SOURCE_DIR must be a directory: ${OPENAPI_SOURCE_DIR}`);
    return null;
  }

  for (const annotation of loadUtoipaPaths(OPENAPI_SOURCE_DIR).values()) {
    if (annotation.method === null || annotation.path === null) {
      throw new Error(
        `${annotation.file}:${annotation.line}: could not read ` +
          `${annotation.method === null ? 'the method' : 'path = "…"'} from #[utoipa::path(...)]`,
      );
    }

    const method = annotation.method.toLowerCase();
    const normalizedPath = normalizeOpenApiPath(annotation.path);
    paths[normalizedPath] = paths[normalizedPath] || {};
    paths[normalizedPath][method] = {
      requestBody: null,
    };
  }

  return { paths };
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
  ['comment_id', 'id'],
  ['secret_name', 'name'],
  ['col_id', 'col'],
  ['card_id', 'card'],
  ['user_id', 'user'],
  ['rev_id', 'rev'],
];

// A param may appear in several groups (e.g. `id` pairs with pipeline_id, job_id,
// comment_id, …). Union every group it belongs to instead of letting the last
// assignment overwrite the earlier ones — otherwise only the final group's
// equivalences would survive for that key.
const PARAM_EQUIVALENCE = new Map();
for (const group of PARAM_EQUIVALENT_GROUPS) {
  const canonical = group.map((value) => toSnakeCase(value));
  for (const key of canonical) {
    let bucket = PARAM_EQUIVALENCE.get(key);
    if (!bucket) {
      bucket = new Set();
      PARAM_EQUIVALENCE.set(key, bucket);
    }
    for (const value of canonical) {
      bucket.add(value);
    }
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
  {
    // The branch a manual run is asked for. The backend read `ref_name` while
    // the client sent `ref`, so every trigger fell through to a hardcoded
    // `refs/heads/main` (card_64804da48693).
    method: 'post',
    path: '/repos/{owner}/{repo}/pipelines',
    required: ['ref'],
    forbidden: ['ref_name'],
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
  // Shorthand properties carry no colon, so the scan above walked straight past
  // `JSON.stringify({ ref })` and reported a body with no fields at all — which
  // is exactly the shape the pipeline trigger uses, and exactly the shape whose
  // field name went unchecked while the backend read a different one.
  const shorthandRe = /(?:^|,)\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?=,|$)/g;
  while ((km = shorthandRe.exec(body)) !== null) {
    if (!keys.includes(km[1])) keys.push(km[1]);
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
    return { ok: false, details: 'No body detected; verify whether the request body is sent' };
  }
  if (bodyKeys.dynamic) return { ok: true, details: null };

  const missing = required.filter((name) => !bodyKeys.keys.includes(name));
  if (missing.length === 0) return { ok: true, details: null };
  return { ok: false, details: `Missing required body fields: ${missing.join(',')}` };
}

function inspectKnownBodyContracts(method, targetPath, bodyKeys) {
  const contract = KNOWN_BODY_CONTRACTS.find((entry) => {
    return entry.method === method && matchPathPattern(targetPath, entry.path);
  });
  if (!contract) return { ok: true, details: null };

  if (!bodyKeys.present) {
    return { ok: false, details: `No body detected; the endpoint requires fields: ${contract.required.join(', ')}` };
  }
  if (bodyKeys.dynamic) return { ok: true, details: null };

  const missing = contract.required.filter((name) => !bodyKeys.keys.includes(name));
  const forbidden = contract.forbidden.filter((name) => bodyKeys.keys.includes(name));
  const details = [];
  if (missing.length > 0) details.push(`Missing fields: ${missing.join(', ')}`);
  if (forbidden.length > 0) details.push(`Sent legacy fields the backend does not accept: ${forbidden.join(', ')}`);

  return details.length === 0 ? { ok: true, details: null } : { ok: false, details: details.join('; ') };
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
    packageDetailSource = productionTsSource(readFileSync(packageDetailFile, 'utf8'));
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow missing: unable to read package detail page (${packageDetailFile})`);
    return;
  }

  const loadPackageMatch = packageDetailSource.match(/async function loadPackage\(\)\s*\{([\s\S]*?)\n  \}/);
  if (!loadPackageMatch) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow missing: loadPackage() not found in package detail page (source: ${packageDetailFile})`);
    return;
  }

  if (!/\bpackages\.getVersions\(/.test(loadPackageMatch[1]) && !/\bloadVersions\(\)/.test(loadPackageMatch[1])) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow does not load versions: package detail page loadPackage() does not call the version-list endpoint (source: ${packageDetailFile})`);
  }

  let authStoreSource = '';
  let loginPageSource = '';
  try {
    authStoreSource = productionTsSource(readFileSync(authStoreFile, 'utf8'));
    loginPageSource = productionTsSource(readFileSync(loginPageFile, 'utf8'));
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow missing: unable to read login/MFA flow files (${authStoreFile}, ${loginPageFile})`);
    return;
  }

  const loginFunctionMatch = authStoreSource.match(/export async function login\(username: string, password: string\)\s*\{([\s\S]*?)\n\}/);
  if (!loginFunctionMatch) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow missing: login(username, password) not found in auth store (source: ${authStoreFile})`);
  } else {
    const loginBody = loginFunctionMatch[1];
    if (!/res\.mfa_required/.test(loginBody)) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ Login contract does not handle MFA: /users/login can return mfa_required=true, but the auth store has no branch for it (source: ${authStoreFile})`);
    }
    const mfaIndex = loginBody.indexOf('res.mfa_required');
    const tokenIndex = loginBody.indexOf('setToken(res.token)');
    if (mfaIndex === -1 || tokenIndex === -1 || tokenIndex < mfaIndex) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ Login contract would store an empty MFA token: setToken(res.token) must occur after the mfa_required branch (source: ${authStoreFile})`);
    }
  }

  if (!/export async function verifyMfa\(/.test(authStoreSource) || !/\bauth\.verifyMfa\(/.test(authStoreSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ MFA flow missing: auth store does not call /users/mfa/verify (source: ${authStoreFile})`);
  }

  if (!/\bisMfaRequired\(\)/.test(loginPageSource) || !/\bverifyMfa\(/.test(loginPageSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ MFA UI missing: the login page does not display and submit the two-step verification code (source: ${loginPageFile})`);
  }

  let repoHeaderSource = '';
  try {
    repoHeaderSource = productionTsSource(readFileSync(repoHeaderFile, 'utf8'));
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Frontend flow missing: unable to read repo header component (${repoHeaderFile})`);
    return;
  }

  if (/archive\/main\.zip/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Repo archive link hardcodes main: RepoHeader must use the repo default_branch or the backend-returned value (source: ${repoHeaderFile})`);
  }

  if (!/\bdefaultBranch\b/.test(repoHeaderSource) || !/\brepos\.get\(/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Repo archive link not aligned with default branch: RepoHeader should obtain default_branch from props or /repos/{owner}/{repo} (source: ${repoHeaderFile})`);
  }

  if (!/\bdownloadApiFile\(/.test(repoHeaderSource) || !/archive\/\$\{encodeURIComponent\(archiveRef\)\}\.zip/.test(repoHeaderSource)) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Repo archive download does not use the authenticated API helper/ref encoding: RepoHeader download should align with backend /archive/{ref}.zip and carry Bearer auth (source: ${repoHeaderFile})`);
  }

  let reposApiSource = '';
  try {
    reposApiSource = readFileSync(reposApiFile, 'utf8');
  } catch (error) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Backend contract missing: unable to read repo API file (${reposApiFile})`);
    return;
  }

  // Read through the shared struct reader: it anchors the declaration in the
  // production code-only view and hands back the field block from the
  // comment-free view. The raw `pub struct RepoResponse { … }` match this
  // replaces accepted a commented-out declaration, and its field greps counted
  // a commented-out field as declared — the frontend then depends on a key the
  // response never carries (card_64b6ede78939).
  const repoResponseFields = rustStructBody(reposApiSource, 'RepoResponse');
  if (repoResponseFields === null) {
    ISSUE.count += 1;
    ISSUE.lines.push(`❌ Backend contract missing: RepoResponse schema is not defined (source: ${reposApiFile})`);
    return;
  }

  for (const field of ['default_branch', 'stars_count', 'forks_count', 'fork_id']) {
    if (!new RegExp(`\\bpub\\s+${field}\\s*:`).test(repoResponseFields)) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ Repo detail response schema drift: frontend repo page depends on ${field}, but RepoResponse does not declare it (source: ${reposApiFile})`);
    }
  }
}

async function main() {
  let openapi = readLocalOpenApi();
  if (!openapi) {
    const sourceOpenApi = loadOpenApiFromRustSource();
    if (sourceOpenApi) {
      console.log('✅ Aligned using OpenAPI route info auto-extracted from Rust source (offline mode)');
      openapi = sourceOpenApi;
    }
  }

  if (!openapi) {
    const openapiResp = await requestWithTimeout(OPENAPI_URL);
    if (!openapiResp.ok) {
      console.log(`❌ Unable to read OpenAPI: ${openapiResp.error?.message || `HTTP ${openapiResp.response?.status}`}`);
      process.exit(1);
    }
    if (!openapiResp.response.ok) {
      console.log(`❌ OpenAPI returned an error: HTTP ${openapiResp.response.status}`);
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
    console.log('❌ No scannable frontend API call sites found');
    process.exit(1);
  }

  const calls = [];
  for (const file of clientFiles) {
    // A commented-out `request<T>('/path')` is not a call the client makes, so
    // it must not enter the census the OpenAPI comparison runs over.
    const src = productionTsSource(readFileSync(file, 'utf8'));
    calls.push(...extractRequestCalls(src, file));
  }

  let skippedDynamic = 0;
  const normalizedCalls = dedupeCalls(calls);
  for (const call of normalizedCalls) {
    const method = call.method;
    const targetPath = normalizeApiPath(call.path);
    if (targetPath.includes(`{${OPAQUE_SEGMENT}}`)) {
      skippedDynamic += 1;
      console.log(`ℹ️  Skipped dynamic client route (URL built by a helper, not statically resolvable): ${method.toUpperCase()} ${call.path} (source: ${call.file})`);
      continue;
    }
    const matches = findOpenApiMatch(rawPaths, method, targetPath);
    if (matches.length === 0) {
      ISSUE.count += 1;
      ISSUE.lines.push(`❌ Frontend route not aligned: ${method.toUpperCase()} ${targetPath} has no matching path/method in OpenAPI (source: ${call.file})`);
      continue;
    }

    const firstMatch = rawPaths[matches[0]][method];
    const apiParams = new Set(extractParamNames(matches[0]));
    const clientParams = new Set(extractParamNames(targetPath));

    for (const name of clientParams) {
      const matched = [...apiParams].some((entry) => equivalentParam(name, entry));
      if (!matched) {
        ISSUE.count += 1;
        ISSUE.lines.push(`⚠️ Potential parameter name mismatch: ${method.toUpperCase()} ${targetPath}, client uses ${name}, endpoint defines ${Array.from(apiParams).join(', ') || 'no parameters'} (source: ${call.file})`);
      }
    }
    for (const name of apiParams) {
      const matched = [...clientParams].some((entry) => equivalentParam(name, entry));
      if (!matched) {
        ISSUE.count += 1;
        ISSUE.lines.push(`⚠️ Parameter name missing: ${method.toUpperCase()} ${targetPath}, OpenAPI requires {${name}} but the client template does not include it explicitly (source: ${call.file})`);
      }
    }

    const bodyCheck = inspectBodyAlignment(firstMatch, call.body || {});
    if (!bodyCheck.ok) {
      ISSUE.count += 1;
      ISSUE.lines.push(`⚠️ Request body parameter missing: ${method.toUpperCase()} ${targetPath} -> ${bodyCheck.details} (source: ${call.file})`);
    }

    const knownBodyCheck = inspectKnownBodyContracts(method, targetPath, call.body || {});
    if (!knownBodyCheck.ok) {
      ISSUE.count += 1;
      ISSUE.lines.push(`⚠️ Known request body contract mismatch: ${method.toUpperCase()} ${targetPath} -> ${knownBodyCheck.details} (source: ${call.file})`);
    }
  }

  inspectFrontendFlowContracts();

  for (const line of ISSUE.lines) {
    console.log(line);
  }

  const totalCalls = normalizedCalls.length;
  const mismatches = ISSUE.lines.length;
  const skippedNote = skippedDynamic > 0 ? ` (${skippedDynamic} dynamic route(s) skipped)` : '';
  if (mismatches > 0) {
    console.log(`\n❌ Frontend/backend API alignment check: ${mismatches}/${totalCalls} issues found${skippedNote}`);
  } else {
    console.log(`\n✅ Frontend/backend API alignment check passed: ${totalCalls} frontend requests${skippedNote}`);
  }

  if (STRICT && mismatches > 0) {
    process.exit(1);
  }
}

main().catch((err) => {
  console.log(`❌ Alignment check execution failed: ${err?.message || String(err)}`);
  process.exit(1);
});
