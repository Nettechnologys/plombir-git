// The UI half of the route inventory: what a page offers a person to click,
// which API call sits behind it, and therefore which access level the backend
// will hold that click to.
//
// `route_table.rs` already makes the server side unforgettable — a route cannot
// be registered without declaring an `Access`, and the persona sweeps walk
// every declared row. Nothing did the same for the browser: a button that calls
// a route no test touches is invisible to every gate in the repo, and the 22
// `?raw` frontend tests assert on component *text*, so they pass over a
// component that throws on render.
//
// This module derives the missing side from the sources, so the inventory
// cannot drift the way a hand-maintained list does (`docs/FEATURE_INVENTORY.md`
// carries "последняя сверка 2026-07-23" and 962 commits have landed since).
//
// Everything is read off the production views from `ts-source.mjs`: a
// commented-out button is not shipped, so it must not appear in the inventory,
// and a commented-out `request()` call must read as a route the page no longer
// reaches.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import {
  extractBrowserTransportCalls,
  productionTsCode,
  productionTsSource,
} from './ts-source.mjs';
import { createLocalPathResolver, expandLocalPathCalls } from './ts-path-resolver.mjs';

/** Tags a person can act on. `input` only counts when it submits or is a button. */
const INTERACTIVE_TAGS = ['button', 'a', 'form', 'input', 'select', 'textarea'];

/**
 * Read a balanced block starting at `start` (which must index `open`).
 *
 * Anchored in the code view, where string, template and regex bodies are
 * blanked, so a brace inside a literal cannot unbalance the scan.
 */
function readBalanced(code, start, open, close) {
  if (code[start] !== open) return null;
  let depth = 0;
  for (let i = start; i < code.length; i += 1) {
    const ch = code[i];
    if (ch === open) depth += 1;
    else if (ch === close) {
      depth -= 1;
      if (depth === 0) return { start, end: i };
    }
  }
  return null;
}

/** Function body after a complete parameter list, past an object return type. */
function functionBody(code, openParen) {
  const params = readBalanced(code, openParen, '(', ')');
  if (!params) return null;
  const braceAt = code.indexOf('{', params.end + 1);
  if (braceAt === -1) return null;
  let block = readBalanced(code, braceAt, '{', '}');
  if (!block) return null;

  // `function f(): { disconnect(): void } { ... }` has two adjacent balanced
  // brace blocks. The first is a return type; treating it as the body silently
  // drops every transport from the real implementation.
  if (code.slice(params.end + 1, braceAt).includes(':')) {
    let next = block.end + 1;
    while (/\s/.test(code[next] || '')) next += 1;
    if (code[next] === '{') block = readBalanced(code, next, '{', '}');
  }
  return block;
}

/** Line number (1-based) of `offset` in `source`. */
function lineAt(source, offset) {
  let line = 1;
  for (let i = 0; i < offset && i < source.length; i += 1) {
    if (source[i] === '\n') line += 1;
  }
  return line;
}

/**
 * Split an object literal body into its top-level `key: value` entries.
 *
 * Depth is counted over the code view for the reason `readBalanced` is: an
 * unbalanced brace inside a template literal would merge two entries into one
 * and silently drop the second entry's route from the inventory.
 */
function splitObjectEntries(code, bodyStart, bodyEnd) {
  const entries = [];
  let depth = 0;
  let entryStart = bodyStart;
  for (let i = bodyStart; i < bodyEnd; i += 1) {
    const ch = code[i];
    if (ch === '{' || ch === '[' || ch === '(') depth += 1;
    else if (ch === '}' || ch === ']' || ch === ')') depth -= 1;
    else if (ch === ',' && depth === 0) {
      entries.push({ start: entryStart, end: i });
      entryStart = i + 1;
    }
  }
  if (entryStart < bodyEnd) entries.push({ start: entryStart, end: bodyEnd });
  return entries.filter((entry) => code.slice(entry.start, entry.end).trim());
}

/** Named local functions that an exported API member may delegate to once. */
function transportHelpers(code, text) {
  const helpers = new Map();
  const header = /(?:^|[^A-Za-z0-9_$])(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g;
  let match;
  while ((match = header.exec(code)) !== null) {
    const open = header.lastIndex - 1;
    const params = readBalanced(code, open, '(', ')');
    if (!params) continue;
    const braceAt = code.indexOf('{', params.end + 1);
    const body = braceAt === -1 ? null : readBalanced(code, braceAt, '{', '}');
    if (!body) continue;

    const names = splitObjectEntries(code, params.start + 1, params.end)
      .map((entry) => code.slice(entry.start, entry.end).match(/^\s*(?:\.\.\.)?([A-Za-z_$][\w$]*)/)?.[1])
      .filter(Boolean);
    helpers.set(match[1], {
      params: names,
      body: text.slice(body.start + 1, body.end),
    });
  }
  return helpers;
}

/** Resolve a literal argument through one local transport helper call. */
function helperTransportCalls(source, file, helpers) {
  const code = productionTsCode(source);
  const calls = [];
  for (const [name, helper] of helpers) {
    let cursor = 0;
    while (true) {
      const at = code.indexOf(name, cursor);
      if (at === -1) break;
      const before = code[at - 1];
      const after = code[at + name.length];
      if ((before && /[A-Za-z0-9_$]/.test(before)) || before === '.'
        || (after && /[A-Za-z0-9_$]/.test(after))) {
        cursor = at + 1;
        continue;
      }

      let open = at + name.length;
      while (/\s/.test(code[open] || '')) open += 1;
      if (code[open] === '<') {
        const generic = readBalanced(code, open, '<', '>');
        open = generic ? generic.end + 1 : open;
        while (/\s/.test(code[open] || '')) open += 1;
      }
      if (code[open] !== '(') {
        cursor = at + 1;
        continue;
      }
      const invocation = readBalanced(code, open, '(', ')');
      if (!invocation) {
        cursor = at + 1;
        continue;
      }

      const args = splitObjectEntries(code, open + 1, invocation.end);
      const bindings = {};
      helper.params.forEach((param, index) => {
        const arg = args[index];
        if (arg) bindings[param] = source.slice(arg.start, arg.end).trim();
      });
      calls.push(...extractBrowserTransportCalls(helper.body, file, { bindings }));
      cursor = invocation.end + 1;
    }
  }
  return calls;
}

/**
 * The API surface a client module exposes: `labels.create` → `POST /repos/…`.
 *
 * Two shapes ship today — a namespace object (`export const labels = { … }`)
 * and a bare exported function — and both are read, because a symbol this
 * misses becomes a button the inventory cannot resolve to a route.
 */
export function parseApiSurface(source, file) {
  const code = productionTsCode(source);
  const text = productionTsSource(source);
  const rows = [];
  const helpers = transportHelpers(code, text);
  const pathResolver = createLocalPathResolver(text);
  const endpointCalls = (body) => {
    const found = expandLocalPathCalls(body, pathResolver).flatMap((variant) => [
      ...extractBrowserTransportCalls(variant.source, file),
      ...helperTransportCalls(variant.source, file, helpers),
    ].map((call) => ({ ...call, constraints: variant.constraints })));
    const unique = new Map();
    for (const call of found) {
      unique.set(`${call.method}\u0000${call.base}\u0000${call.path}`, call);
    }
    return [...unique.values()];
  };

  const readEntries = (namespace, members, start, end) => {
    for (const entry of splitObjectEntries(code, start, end)) {
      const entryCode = code.slice(entry.start, entry.end);
      const head = entryCode.match(/^\s*([A-Za-z_$][\w$]*)\s*:/);
      if (!head) continue;

      const member = head[1];
      const path = [...members, member];
      let valueAt = entry.start + head[0].length;
      while (/\s/.test(code[valueAt] || '')) valueAt += 1;

      // An object-valued member is another namespace, not one endpoint whose
      // body happens to contain all of its children's requests. Recurse until
      // the leaf so `repos.templates.gitignores` and
      // `releases.attestation.sign` retain their real identities.
      if (code[valueAt] === '{') {
        const nested = readBalanced(code, valueAt, '{', '}');
        if (nested && nested.end <= entry.end) {
          readEntries(namespace, path, nested.start + 1, nested.end);
          continue;
        }
      }

      const body = text.slice(entry.start, entry.end);
      for (const call of endpointCalls(body)) {
        rows.push({
          symbol: [namespace, ...path].join('.'),
          namespace,
          member: path.join('.'),
          method: call.method.toUpperCase(),
          path: call.path,
          base: call.base,
          transport: call.transport,
          constraints: call.constraints,
          file,
          line: lineAt(text, entry.start),
        });
      }
    }
  };

  const nsRe = /export\s+const\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*)?=\s*\{/g;
  let match;
  while ((match = nsRe.exec(code)) !== null) {
    const braceAt = code.indexOf('{', match.index + match[0].length - 1);
    const block = readBalanced(code, braceAt, '{', '}');
    if (!block) continue;
    const namespace = match[1];
    readEntries(namespace, [], block.start + 1, block.end);
    nsRe.lastIndex = block.end;
  }

  const fnRe = /export\s+(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g;
  while ((match = fnRe.exec(code)) !== null) {
    const openParen = fnRe.lastIndex - 1;
    const block = functionBody(code, openParen);
    if (!block) continue;
    const body = text.slice(block.start, block.end);
    for (const call of endpointCalls(body)) {
      rows.push({
        symbol: match[1],
        namespace: null,
        member: match[1],
        method: call.method.toUpperCase(),
        path: call.path,
        base: call.base,
        transport: call.transport,
        constraints: call.constraints,
        file,
        line: lineAt(text, match.index),
      });
    }
    fnRe.lastIndex = block.end;
  }

  return rows;
}

/** Every `.ts` under `dir`, test doubles excluded. */
export function collectFiles(dir, predicate) {
  const found = [];
  const walk = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      if (entry.name.startsWith('.') || entry.name === 'node_modules') continue;
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile() && predicate(full)) found.push(full);
    }
  };
  walk(dir);
  return found;
}

// ── Pages ──────────────────────────────────────────────────────────────────

/** `onclick`, `on:click`, … — every spelling a handler is attached under. */
const HANDLER_ATTRS = ['onclick', 'on:click', 'onsubmit', 'on:submit', 'onchange', 'on:change'];

/**
 * The `<script>` ranges of a component, so markup scanning cannot mistake a
 * string in the script for a tag, nor a handler declaration for a button.
 */
function scriptRanges(code) {
  const ranges = [];
  const re = /<script\b[^>]*>/g;
  let match;
  while ((match = re.exec(code)) !== null) {
    const close = code.indexOf('</script>', re.lastIndex);
    if (close === -1) break;
    ranges.push({ start: match.index, end: close + '</script>'.length });
    re.lastIndex = close;
  }
  return ranges;
}

const inRanges = (ranges, offset) => ranges.some((r) => offset >= r.start && offset < r.end);

/**
 * Top-level declarations that can hold a handler body, with their ranges.
 *
 * Both spellings are read — `function save()` and `const save = () => {}` —
 * because a page mixes them freely and a body this misses turns into a button
 * the inventory reports as calling nothing.
 */
function declarationRanges(code) {
  const decls = [];
  const push = (name, bodyStart) => {
    if (bodyStart === -1) return;
    const block = readBalanced(code, bodyStart, '{', '}');
    if (block) decls.push({ name, start: block.start, end: block.end });
  };

  const fnRe = /(?:^|\s)(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g;
  let match;
  while ((match = fnRe.exec(code)) !== null) {
    const paren = readBalanced(code, code.indexOf('(', match.index + match[0].length - 1), '(', ')');
    if (!paren) continue;
    push(match[1], code.indexOf('{', paren.end));
  }

  const arrowRe = /(?:^|\s)(?:export\s+)?(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(/g;
  while ((match = arrowRe.exec(code)) !== null) {
    const paren = readBalanced(code, code.indexOf('(', match.index + match[0].length - 1), '(', ')');
    if (!paren) continue;
    const arrow = code.slice(paren.end + 1, paren.end + 12).match(/^\s*=>\s*\{/);
    if (!arrow) continue;
    push(match[1], code.indexOf('{', paren.end));
  }

  return decls;
}

/**
 * Browser transports executed directly by a Svelte module.
 *
 * API-object members are resolved through `parseApiSurface`; these rows cover
 * the other side of the boundary: a page or layout that itself performs a
 * request. A URL factory alone is deliberately excluded — only an executable
 * transport proves reachability. The file-qualified owner keeps two `load`
 * functions on different pages from collapsing into one synthetic symbol.
 */
export function parseDirectTransportSurface(source, file) {
  const code = productionTsCode(source);
  const text = productionTsSource(source);
  const decls = declarationRanges(code);
  return extractBrowserTransportCalls(text, file)
    .filter((call) => call.transport !== 'url')
    .map((call) => {
      const owner = ownerOf(decls, call.start);
      return {
        kind: 'transport',
        symbol: `${file}#${owner?.name ?? '<module>'}`,
        owner: owner?.name ?? null,
        method: call.method.toUpperCase(),
        path: call.path,
        base: call.base,
        transport: call.transport,
        file,
        line: lineAt(text, call.start),
      };
    });
}

/** The innermost declaration containing `offset`, or null for module scope. */
function ownerOf(decls, offset) {
  let best = null;
  for (const decl of decls) {
    if (offset < decl.start || offset > decl.end) continue;
    if (!best || decl.start > best.start) best = decl;
  }
  return best;
}

/**
 * Namespaces the page imports out of `$lib/api/…`.
 *
 * Read off the *text* view, not the code view: the module specifier is a string
 * literal, and the code view blanks string bodies — anchoring there matches no
 * import at all and reports every page as calling no API.
 */
function importedApiSymbols(text) {
  const names = new Set();
  const re = /import\s*\{([^}]*)\}\s*from\s*['"][^'"]*(?:\$lib\/api|lib\/api)[^'"]*['"]/g;
  let match;
  while ((match = re.exec(text)) !== null) {
    for (const raw of match[1].split(',')) {
      const name = raw.split(/\s+as\s+/).pop().trim();
      if (name) names.add(name);
    }
  }
  return names;
}

/**
 * The label a person actually reads on a control.
 *
 * Static text wins; an i18n call contributes its key, because `{t('repo.delete')}`
 * identifies the button far better than the empty string that stripping every
 * expression would leave behind.
 */
function labelOf(text, tagEnd, tag) {
  const close = text.indexOf(`</${tag}>`, tagEnd);
  const inner = close === -1 ? '' : text.slice(tagEnd + 1, close);
  const keys = [...inner.matchAll(/\bt\(\s*['"]([^'"]+)['"]/g)].map((m) => m[1]);
  const plain = inner
    .replace(/<[^>]*>/g, ' ')
    .replace(/\{[^}]*\}/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  if (plain) return plain.slice(0, 60);
  if (keys.length) return `i18n:${keys[0]}`;
  return '';
}

/**
 * Where a tag's opening `>` actually is.
 *
 * `indexOf('>')` is wrong on every modern Svelte control: `onclick={() => x()}`
 * puts a `>` inside an attribute value, so the naive scan cuts the tag in half
 * and the leftovers land in the button's label — which is how a control reads
 * as unhandled while its handler sits just past the cut.
 */
function tagEndAt(source, start) {
  let depth = 0;
  let quote = '';
  for (let i = start; i < source.length; i += 1) {
    const ch = source[i];
    if (quote) {
      if (ch === quote) quote = '';
      continue;
    }
    if (depth === 0 && (ch === '"' || ch === "'")) quote = ch;
    else if (ch === '{') depth += 1;
    else if (ch === '}') depth -= 1;
    else if (ch === '>' && depth === 0) return i;
  }
  return -1;
}

/** Read one attribute's value, `{expr}` or `"literal"`, from a tag's text. */
function attrValue(tagText, name) {
  const re = new RegExp(`\\b${name.replace(':', '\\:')}\\s*=\\s*`, 'i');
  const at = tagText.search(re);
  if (at === -1) {
    // A valueless attribute (`disabled`, `required`) is present-but-empty, which
    // is a different fact from absent — a `disabled` button is still a control.
    return new RegExp(`\\b${name.replace(':', '\\:')}\\b`, 'i').test(tagText) ? '' : null;
  }
  const rest = tagText.slice(at).replace(re, '');
  if (rest[0] === '{') {
    const block = readBalanced(rest, 0, '{', '}');
    return block ? rest.slice(1, block.end).trim() : null;
  }
  const quoted = rest.match(/^(['"])(.*?)\1/s);
  return quoted ? quoted[2].trim() : null;
}

/** `{handleSave}` / `{() => confirmDelete(x)}` → the local names it invokes. */
function handlerNames(expr) {
  if (!expr) return [];
  const bare = expr.trim().match(/^([A-Za-z_$][\w$]*)$/);
  if (bare) return [bare[1]];
  return [...expr.matchAll(/([A-Za-z_$][\w$]*)\s*\(/g)].map((m) => m[1]);
}


/**
 * Shared components the file mounts, with the props each mount binds.
 *
 * Without this the inventory reports `[owner]/[repo]/new` as offering no
 * controls at all: the page is a shim around `<FileEditor …/>`, and its Save
 * and Cancel buttons live in the component. "This page has nothing to test" is
 * the most dangerous thing an inventory can say wrongly, so the delegation is
 * followed rather than assumed away.
 */
function componentMounts(code, text, scripts, decls) {
  const imported = new Map();
  const importRe = /import\s+([A-Z][\w$]*)\s+from\s*['"]([^'"]+\.svelte)['"]/g;
  let match;
  while ((match = importRe.exec(text)) !== null) imported.set(match[1], match[2]);

  const mounts = [];
  const mountRe = /<([A-Z][\w$]*)(\s|\/|>)/g;
  while ((match = mountRe.exec(code)) !== null) {
    if (inRanges(scripts, match.index)) continue;
    const name = match[1];
    if (!imported.has(name)) continue;
    const end = tagEndAt(code, match.index);
    if (end === -1) continue;
    const tagText = text.slice(match.index, end + 1);
    const props = {};
    for (const prop of tagText.matchAll(/\b([A-Za-z_$][\w$]*)\s*=/g)) {
      const value = attrValue(tagText, prop[1]);
      if (value !== null) props[prop[1]] = value;
    }
    mounts.push({ name, source: imported.get(name), props, line: lineAt(text, match.index) });
  }
  return mounts;
}

/**
 * Everything one page offers a person, and what each control reaches.
 *
 * The chain a reviewer would trace by hand — button → handler → `labels.create`
 * → `POST /repos/{owner}/{repo}/labels` → the `Access` the router declares — is
 * resolved here for all of them at once. A control whose handler calls another
 * local function that makes the call is followed too (`MAX_HOPS`), because
 * "`handleSave` calls `save`" is the commonest spelling in these pages and
 * stopping at one hop would report the button as reaching nothing.
 */
const MAX_HOPS = 4;

/**
 * Call names that are language or framework machinery, not callback props.
 *
 * Without the filter every `String(x)` and `$effect(…)` in a handler would be
 * offered as a prop to bind, and a mount whose prop happens to share one of
 * those names would bind to the wrong thing.
 */
const RESERVED_CALLS = new Set([
  'if', 'for', 'while', 'switch', 'catch', 'return', 'typeof', 'await', 'new',
  'setTimeout', 'setInterval', 'clearTimeout', 'clearInterval', 'fetch', 'alert',
  'confirm', 'require', 'import', 'super', 'console', 'parseInt', 'parseFloat',
  'encodeURIComponent', 'decodeURIComponent', 'structuredClone', 'queueMicrotask',
]);

export function parsePageInventory(source, file, { includeDirectTransports = true } = {}) {
  const code = productionTsCode(source);
  const text = productionTsSource(source);
  const scripts = scriptRanges(code);
  const decls = declarationRanges(code);
  const apiNames = importedApiSymbols(text);
  const transportSurface = includeDirectTransports
    ? parseDirectTransportSurface(text, file)
    : [];

  // Which API symbols each declaration reaches directly, and which local
  // declarations it calls (the edges the hop-following walks).
  const directCalls = new Map();
  const localEdges = new Map();
  const propCalls = new Map();
  const transportCalls = new Map();
  const declNames = new Set(decls.map((d) => d.name));
  const moduleScopeCalls = new Set();
  const moduleScopeTransports = new Set();

  for (const row of transportSurface) {
    if (!row.owner) {
      moduleScopeTransports.add(row);
      continue;
    }
    if (!transportCalls.has(row.owner)) transportCalls.set(row.owner, new Set());
    transportCalls.get(row.owner).add(row);
  }

  const callRe = /([A-Za-z_$][\w$]*(?:(?:\?\.|\.)[A-Za-z_$][\w$]*)*)\s*\(/g;
  let match;
  while ((match = callRe.exec(code)) !== null) {
    if (!inRanges(scripts, match.index)) continue;
    const qualified = match[1].replaceAll('?.', '.');
    const parts = qualified.split('.');
    const head = parts[0];
    const owner = ownerOf(decls, match.index);
    if (apiNames.has(head)) {
      const symbol = parts.join('.');
      if (!owner) moduleScopeCalls.add(symbol);
      else {
        if (!directCalls.has(owner.name)) directCalls.set(owner.name, new Set());
        directCalls.get(owner.name).add(symbol);
      }
      continue;
    }
    if (parts.length === 1 && owner && declNames.has(head) && head !== owner.name) {
      if (!localEdges.has(owner.name)) localEdges.set(owner.name, new Set());
      localEdges.get(owner.name).add(head);
      continue;
    }
    // Neither a local declaration nor an API symbol: in a shared component this
    // is a callback prop, and following it is what connects `FileEditor`'s Save
    // button to the page handler that actually writes the file.
    if (parts.length === 1 && owner && /^[a-z_$][\w$]*$/.test(head) && !RESERVED_CALLS.has(head)) {
      if (!propCalls.has(owner.name)) propCalls.set(owner.name, new Set());
      propCalls.get(owner.name).add(head);
    }
  }

  const walk = (name, table) => {
    const seen = new Set([name]);
    const out = new Set(table.get(name) || []);
    let frontier = [name];
    for (let hop = 0; hop < MAX_HOPS && frontier.length; hop += 1) {
      const next = [];
      for (const current of frontier) {
        for (const callee of localEdges.get(current) || []) {
          if (seen.has(callee)) continue;
          seen.add(callee);
          next.push(callee);
          for (const symbol of table.get(callee) || []) out.add(symbol);
        }
      }
      frontier = next;
    }
    return [...out];
  };
  const reachedBy = (name) => walk(name, directCalls);
  const propsReachedBy = (name) => walk(name, propCalls);
  const transportsReachedBy = (name) => walk(name, transportCalls);

  const elements = [];
  const tagRe = new RegExp(`<(${INTERACTIVE_TAGS.join('|')})(\\s|>)`, 'gi');
  while ((match = tagRe.exec(code)) !== null) {
    if (inRanges(scripts, match.index)) continue;
    const tagEnd = tagEndAt(code, match.index);
    if (tagEnd === -1) continue;
    const tagText = text.slice(match.index, tagEnd + 1);
    const tag = match[1].toLowerCase();

    const inputType = tag === 'input' ? (attrValue(tagText, 'type') || 'text') : null;
    // A text input is not a control a person "presses"; a submit button is.
    if (tag === 'input' && !['submit', 'button', 'checkbox', 'radio', 'file'].includes(inputType)) continue;

    let handlerExpr = null;
    let handlerAttr = null;
    for (const attr of HANDLER_ATTRS) {
      const value = attrValue(tagText, attr);
      if (value) {
        handlerExpr = value;
        handlerAttr = attr;
        break;
      }
    }

    const href = tag === 'a' ? attrValue(tagText, 'href') : null;
    const responseField = href?.match(/(?:^|\.)([A-Za-z_$][\w$]*)$/)?.[1] ?? null;
    const refs = handlerNames(handlerExpr);
    const names = refs.filter((n) => declNames.has(n));
    const reaches = [...new Set(names.flatMap((n) => reachedBy(n)))];
    const transports = [...new Set(names.flatMap((n) => transportsReachedBy(n)))];
    const reachesProps = [...new Set([...refs, ...names.flatMap((n) => propsReachedBy(n))])];

    elements.push({
      tag,
      inputType,
      label: labelOf(text, tagEnd, tag),
      href,
      responseFields: responseField ? [responseField] : [],
      handlerAttr,
      handler: handlerExpr ? handlerExpr.replace(/\s+/g, ' ').slice(0, 80) : null,
      handlerNames: names,
      // Names the handler mentions that are NOT local declarations. In a shared
      // component these are props — `onclick={onSave}` — and the page that
      // mounts the component is what binds them to a real call.
      handlerRefs: refs.filter((n) => !declNames.has(n)),
      reaches,
      transports,
      reachesProps,
      line: lineAt(text, match.index),
    });
  }

  // What the page reaches without anyone clicking: module-scope calls plus
  // everything only `$effect` / `onMount` / a `load` helper reaches. It is still
  // surface — a page that renders a private repo's data on load needs the same
  // access answer as a button that fetches it — so it is reported, separately.
  const viaControls = new Set(elements.flatMap((element) => element.reaches));
  const transportsViaControls = new Set(elements.flatMap((element) => element.transports));
  const everything = new Set(moduleScopeCalls);
  const everyTransport = new Set(moduleScopeTransports);
  for (const symbols of directCalls.values()) {
    for (const symbol of symbols) everything.add(symbol);
  }
  for (const rows of transportCalls.values()) {
    for (const row of rows) everyTransport.add(row);
  }
  const passiveCalls = [...everything].filter((symbol) => !viaControls.has(symbol));
  const passiveTransports = [...everyTransport]
    .filter((row) => !transportsViaControls.has(row));

  return {
    file,
    apiNames: [...apiNames],
    elements,
    passiveCalls,
    passiveTransports,
    declarations: decls.map((d) => d.name),
    mounts: componentMounts(code, text, scripts, decls),
  };
}
