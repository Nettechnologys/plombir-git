#!/usr/bin/env node

// Test processes build many throwaway pools in parallel. They must use the
// longer test acquire timeout; the production `rg_db::connect` wrapper uses the
// deliberately shorter outage budget and turns a busy CI disk into a setup
// failure that looks like a broken test.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { stripCfgTestItems, stripRustNonCode } from './lib/rust-consumer-contract.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(scriptsDir, '..');
const cratesDir = path.join(root, 'crates');
const MIN_TEST_OPENERS = 100;

function rustFiles(dir) {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (entry.name.startsWith('.') || entry.name === 'target') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) files.push(full);
  }
  return files;
}

function relative(file) {
  return path.relative(root, file).split(path.sep).join('/');
}

function lineAt(source, index) {
  return source.slice(0, index).split('\n').length;
}

function removedAt(production, index, length) {
  return !/\S/.test(production.slice(index, index + length));
}

function moduleCandidates(owner, name) {
  const parsed = path.parse(owner);
  const base = ['lib', 'main', 'mod'].includes(parsed.name)
    ? parsed.dir
    : path.join(parsed.dir, parsed.name);
  return [path.join(base, `${name}.rs`), path.join(base, name, 'mod.rs')];
}

function explicitModuleCandidate(owner, raw, clean, declarationIndex) {
  const pathAttribute = /#\s*\[\s*path\s*=\s*"([^"\r\n]+)"\s*\]/g;
  let candidate = null;

  for (const match of raw.matchAll(pathAttribute)) {
    if (match.index >= declarationIndex) break;
    if (removedAt(clean, match.index, match[0].length)) continue;

    const end = match.index + match[0].length;
    const between = clean.slice(end, declarationIndex);
    if (/^(?:\s*#\[[^\]]*\])*\s*$/.test(between)) {
      candidate = path.join(path.dirname(owner), match[1]);
    }
  }

  return candidate;
}

function callEnd(source, open) {
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === '(') depth += 1;
    else if (source[i] === ')') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return null;
}

function callArguments(source, open, end) {
  const arguments_ = [];
  const delimiters = [];
  let start = open + 1;
  const closing = new Map([
    [')', '('],
    [']', '['],
    ['}', '{'],
  ]);

  for (let i = start; i < end - 1; i += 1) {
    const character = source[i];
    if ('([{'.includes(character)) {
      delimiters.push(character);
    } else if (closing.has(character)) {
      if (delimiters.pop() !== closing.get(character)) return null;
    } else if (character === ',' && delimiters.length === 0) {
      arguments_.push(source.slice(start, i).trim());
      start = i + 1;
    }
  }

  if (delimiters.length > 0) return null;
  arguments_.push(source.slice(start, end - 1).trim());
  return arguments_;
}

const files = rustFiles(cratesDir);
const sources = new Map();
for (const file of files) {
  const raw = readFileSync(file, 'utf8');
  const clean = stripRustNonCode(raw);
  sources.set(file, { raw, clean, production: stripCfgTestItems(clean) });
}

// `crates/*/tests/**` is test-only by layout. Also follow external modules
// declared by a cfg(test) item, including `#[path = "..."]` modules, so an
// external test file cannot hide a production-timeout opener from this sweep.
const testOnlyFiles = new Set(
  files.filter((file) => /^crates\/[^/]+\/tests\//.test(relative(file))),
);
let changed = true;
while (changed) {
  changed = false;
  for (const file of files) {
    const { raw, clean, production } = sources.get(file);
    for (const match of clean.matchAll(/\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/g)) {
      const declarationIsTestOnly =
        testOnlyFiles.has(file) || removedAt(production, match.index, match[0].length);
      if (!declarationIsTestOnly) continue;
      const explicit = explicitModuleCandidate(file, raw, clean, match.index);
      const target = [explicit, ...moduleCandidates(file, match[1])].find(
        (candidate) => candidate && existsSync(candidate),
      );
      if (target && !testOnlyFiles.has(target)) {
        testOnlyFiles.add(target);
        changed = true;
      }
    }
  }
}

const violations = [];
let testOpeners = 0;
let inlineTestOpeners = 0;
const opener = /\b(?:(rg_db|crate|super|self)\s*::\s*)?(connect|connect_with_timeouts|connect_with_pool)\s*\(/g;
const hiddenOpenerImports = [
  /\buse\s+rg_db\s*::\s*\*\s*;/g,
  /\buse\s+(?:rg_db|crate|super|self)\s+as\s+[A-Za-z_][A-Za-z0-9_]*\s*;/g,
  /\buse\s+(?:rg_db|crate|super|self)\s*::\s*\{[^;}]*\bself\s+as\s+[A-Za-z_][A-Za-z0-9_]*[^;}]*\}\s*;/g,
  /\bextern\s+crate\s+rg_db\s+as\s+[A-Za-z_][A-Za-z0-9_]*\s*;/g,
  /\buse\s+(?:rg_db|crate|super|self)\s*::\s*(?:connect|connect_with_timeouts|connect_with_pool)\b[^;]*;/g,
  /\buse\s+(?:rg_db|crate|super|self)\s*::\s*\{[^;}]*\b(?:connect|connect_with_timeouts|connect_with_pool)\b[^;}]*\}\s*;/g,
];

for (const file of files) {
  const { clean, production } = sources.get(file);
  for (const match of clean.matchAll(opener)) {
    const qualifier = match[1];
    const isRgDbSource = relative(file).startsWith('crates/rg-db/');
    const prefix = clean.slice(Math.max(0, match.index - 80), match.index);
    const isFunctionDefinition = qualifier === undefined && /\bfn\s*$/.test(prefix);
    if (isFunctionDefinition) continue;
    const isBareInternalCall =
      qualifier === undefined && isRgDbSource && !/(?:\.|::)\s*$/.test(prefix);
    if (qualifier === undefined && !isBareInternalCall) continue;
    if (qualifier !== undefined && qualifier !== 'rg_db' && !isRgDbSource) continue;

    const isWholeTestFile = testOnlyFiles.has(file);
    const isInlineTestItem = removedAt(production, match.index, match[0].length);
    if (!isWholeTestFile && !isInlineTestItem) continue;

    testOpeners += 1;
    if (isInlineTestItem) inlineTestOpeners += 1;

    const name = match[2];
    const end = callEnd(clean, match.index + match[0].lastIndexOf('('));
    if (end === null) {
      violations.push({
        file,
        index: match.index,
        reason: `cannot parse ${qualifier}::${name} call`,
      });
      continue;
    }
    if (name === 'connect') {
      violations.push({
        file,
        index: match.index,
        reason: `\`${qualifier ? `${qualifier}::` : ''}connect\` uses the production acquire timeout in test code`,
      });
    } else {
      const arguments_ = callArguments(clean, match.index + match[0].lastIndexOf('('), end);
      const acquireTimeout = arguments_?.[1] ?? '';
      if (/^(?:(?:rg_db|crate|super|self)\s*::\s*)?TEST_CONNECT_TIMEOUT_SECS$/.test(acquireTimeout)) {
        continue;
      }
      violations.push({
        file,
        index: match.index,
        reason: `${qualifier ? `${qualifier}::` : ''}${name} does not use TEST_CONNECT_TIMEOUT_SECS as its acquire timeout`,
      });
    }
  }

  for (const pattern of hiddenOpenerImports) {
    for (const match of clean.matchAll(pattern)) {
      const namesInternalModule = /\b(?:crate|super|self)\b/.test(match[0]);
      if (namesInternalModule && !relative(file).startsWith('crates/rg-db/')) {
        continue;
      }
      const isTestScope =
        testOnlyFiles.has(file) || removedAt(production, match.index, match[0].length);
      if (isTestScope) {
        violations.push({
          file,
          index: match.index,
          reason: 'test code hides an rg_db opener behind an import or module alias',
        });
      }
    }
  }
}

if (testOpeners < MIN_TEST_OPENERS || inlineTestOpeners === 0 || testOnlyFiles.size === 0) {
  console.error(
    `❌ Test DB opener sweep saw ${testOpeners} opener(s), ${inlineTestOpeners} in inline cfg(test) ` +
      `items, across ${testOnlyFiles.size} test-only file(s). Expected at least ${MIN_TEST_OPENERS} ` +
      'openers plus both test layouts; the inventory is incomplete, not green.',
  );
  process.exit(1);
}

if (violations.length > 0) {
  for (const violation of violations) {
    const source = sources.get(violation.file).clean;
    console.error(`❌ ${relative(violation.file)}:${lineAt(source, violation.index)}: ${violation.reason}`);
  }
  console.error(
    'Use rg_db::connect_with_pool(url, rg_db::TEST_CONNECT_TIMEOUT_SECS, ' +
      'rg_db::DEFAULT_IDLE_TIMEOUT_SECS, rg_db::DEFAULT_MAX_CONNECTIONS), or the matching ' +
      '`crate::` names inside rg-db.',
  );
  process.exit(1);
}

console.log(
  `test DB connect timeout contract ok (${testOpeners} opener calls, ${inlineTestOpeners} in inline cfg(test) items)`,
);
