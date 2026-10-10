// Helpers for asserting against the frontend sources from the contract checks.
//
// The Rust side of every frontend/backend contract has `scripts/lib/rust-
// source.mjs`: every reader there anchors in a *production view* where comments
// and `#[cfg(test)]` items are blanked, so "commented out" fails like
// "deleted". The client side had only the two finders below, and both anchored
// in the raw bytes — so the very defect the Rust half was hardened against
// stayed wide open on the other end of the same wire. Proved on
// `web/src/lib/api/packages.ts`: commenting out the line that sets
// `Content-Disposition` left `package-publish-contract-check.mjs` green, over a
// header the client no longer sends (card_a54b6a2db9f2).
//
// `productionTsSource` / `productionTsCode` close that. They are the TypeScript
// twins of `productionRustSource` / `productionRustCode`, byte-aligned with the
// input and with each other, so a finder can locate a declaration in the
// code-only view and slice the value out of the text view at the same offsets.
//
// There is no `#[cfg(test)]` half to blank here: the web sources carry no test
// doubles at module scope, so a production view of a `.ts` file is exactly a
// comment-free one.

import { jsCodeView, jsTextView } from './js-source.mjs';

const blankExceptNewlines = (span) => span.replace(/[^\n]/g, ' ');

/**
 * Whether `source` is a Svelte component rather than a plain `.ts` module.
 *
 * A component is markup at its top level; a module is JavaScript. The two need
 * different comment syntaxes blanked, and blanking the wrong one is a silent
 * false green — hence a rule narrow enough to be checked against the tree
 * rather than a guess: a line that *starts* with an HTML tag or an HTML
 * comment. Measured on `web/src` at the time of writing: all 72 `.svelte` files
 * match, none of the `.ts` files do (a `<script>` spelled inside a string
 * literal, which `markdown.ts` does carry, never starts a line).
 *
 * Truth boundary: a `.ts` module that one day opens a line with `<` inside a
 * template literal would be read as markup, and its `//` comments would stop
 * being blanked. `raw-source-assertion-contract-check-regression.mjs` pins both
 * directions of this decision so the rule cannot drift unobserved.
 */
export function isSvelteComponent(source) {
  return /^[ \t]*<(?:!--|\/?[A-Za-z][A-Za-z0-9-]*(?:[\s/>]|$))/m.test(source);
}

/** End index (exclusive) of the `>` closing the tag opening at `start`. */
function tagEnd(source, start) {
  let i = start;
  while (i < source.length) {
    const ch = source[i];
    if (ch === '"' || ch === "'") {
      i += 1;
      while (i < source.length && source[i] !== ch) i += 1;
    } else if (ch === '>') {
      return i + 1;
    }
    i += 1;
  }
  return null;
}

/** Blank `<!-- … -->` spans, keeping every byte position and newline. */
function blankMarkupComments(span) {
  let out = '';
  let i = 0;
  while (i < span.length) {
    const open = span.indexOf('<!--', i);
    if (open < 0) {
      out += span.slice(i);
      break;
    }
    out += span.slice(i, open);
    const close = span.indexOf('-->', open + 4);
    const end = close < 0 ? span.length : close + 3;
    out += blankExceptNewlines(span.slice(open, end));
    i = end;
  }
  return out;
}

/** Blank CSS block comments, keeping every byte position and newline. */
function blankCssComments(span) {
  let out = '';
  let i = 0;
  while (i < span.length) {
    const open = span.indexOf('/*', i);
    if (open < 0) {
      out += span.slice(i);
      break;
    }
    out += span.slice(i, open);
    const close = span.indexOf('*/', open + 2);
    const end = close < 0 ? span.length : close + 2;
    out += blankExceptNewlines(span.slice(open, end));
    i = end;
  }
  return out;
}

/**
 * A Svelte component split into the regions that need different lexers:
 * `<script>` bodies are JavaScript, `<style>` bodies are CSS, everything else —
 * the open tags included — is markup.
 *
 * Regions tile the whole source in order, so rendering them back concatenated
 * reproduces its length exactly.
 */
function svelteRegions(source) {
  const regions = [];
  let cursor = 0;
  const open = /<(script|style)\b/gi;
  for (let m = open.exec(source); m !== null; m = open.exec(source)) {
    if (m.index < cursor) continue;
    const bodyStart = tagEnd(source, m.index);
    if (bodyStart === null) continue;
    const tag = m[1].toLowerCase();
    const rest = source.slice(bodyStart);
    const closeAt = rest.search(new RegExp(`</${tag}\\s*>`, 'i'));
    const bodyEnd = closeAt < 0 ? source.length : bodyStart + closeAt;
    regions.push({ kind: 'markup', start: cursor, end: bodyStart });
    regions.push({ kind: tag === 'script' ? 'js' : 'css', start: bodyStart, end: bodyEnd });
    cursor = bodyEnd;
    open.lastIndex = bodyEnd;
  }
  regions.push({ kind: 'markup', start: cursor, end: source.length });
  return regions;
}

function renderSvelte(source, { blankLiterals }) {
  let out = '';
  for (const region of svelteRegions(source)) {
    const span = source.slice(region.start, region.end);
    if (region.kind === 'js') out += blankLiterals ? jsCodeView(span) : jsTextView(span);
    else if (region.kind === 'css') out += blankCssComments(span);
    else out += blankMarkupComments(span);
  }
  return out;
}

/**
 * The comment-free, string-bearing view of a `.ts` module or `.svelte`
 * component — the view every assertion about the frontend should be made over.
 *
 * A construct that was merely commented out is gone from the bundle the browser
 * runs, so it must read as deleted here: `//` and block comments inside script
 * bodies, `<!-- … -->` in markup and CSS block comments in `<style>` are all
 * blanked. String, template and regex bodies stay intact — that is where the
 * values a check reads live — a route in a `href`, a header name, a fetch path.
 *
 * Byte-aligned with `source`, so an offset found here addresses the original.
 */
export function productionTsSource(source) {
  return isSvelteComponent(source) ? renderSvelte(source, { blankLiterals: false }) : jsTextView(source);
}

/**
 * The structure-only twin of `productionTsSource`: comments blanked as above,
 * and inside JavaScript the string, template and regex bodies blanked too.
 *
 * This is what the finders below anchor in, for the reason `rustStructBody`
 * anchors in `productionRustCode`: `interface Foo` spelled inside a diagnostic
 * message is not a declaration, and letting it win means slicing a body out of
 * a string. Markup and CSS carry no literals that can be blanked reliably —
 * an apostrophe in prose is not a quote — so there they are the same view.
 */
export function productionTsCode(source) {
  return isSvelteComponent(source) ? renderSvelte(source, { blankLiterals: true }) : jsCodeView(source);
}

/**
 * The member block of a top-level `interface <name> { … }`, or `null`.
 *
 * The idiom this replaces is `/interface Foo[\s\S]*field: T/`, which does not
 * assert that `Foo` declares the field: `[\s\S]*` bridges from the interface
 * header to a member of some *other* declaration further down the file, so
 * moving the field out keeps the gate green under a message naming `Foo`. It is
 * the TypeScript twin of the struct bridge documented on `rustStructBody`, and
 * it sat on the same lines — a check asserting a DTO on both sides of the wire
 * had the same hole in both halves.
 *
 * The declaration is located in `productionTsCode` and the members are sliced
 * out of `productionTsSource`, exactly as `rustStructBody` does it: a
 * commented-out interface cannot be the one that answers, and a commented-out
 * member cannot satisfy an assertion about the shape the client sends.
 *
 * Relies on prettier putting a top-level interface's closing brace at column 0
 * (nested object types are indented, so they cannot end the block early). A
 * shape this cannot read returns `null` so the caller fails loudly instead of
 * asserting over an empty string.
 */
export function tsInterfaceBody(source, name) {
  const code = productionTsCode(source);
  const text = productionTsSource(source);
  const start = code.search(new RegExp(`^(?:export\\s+)?(?:declare\\s+)?interface ${name}\\b`, 'm'));
  if (start < 0) return null;
  const rest = code.slice(start);
  const open = rest.indexOf('{');
  if (open < 0) return null;
  const close = rest.search(/\n\}/);
  if (close < 0 || close < open) return null;
  return text.slice(start + open + 1, start + close);
}

/**
 * The body of a `function <name>(…) { … }`, or `null`.
 *
 * The function twin of `tsInterfaceBody`, and the client-side twin of
 * `rustFnBlock`. It replaces asserting a rule against the whole file: a policy
 * expressed as `/\[A-Z\]/.test(source)` under a message naming one validator is
 * satisfied by *any* occurrence of that literal — and a Svelte page that also
 * carries the policy as an HTML `pattern=` attribute has a second copy of every
 * character class sitting right there. Deleting the rule from the validator
 * then leaves the gate green, quoting the attribute back at itself
 * (card_a08ef8308236).
 *
 * The header is found in `productionTsCode` and the body sliced out of
 * `productionTsSource`, so a commented-out function cannot answer and a
 * commented-out rule inside a live one cannot satisfy the assertion.
 *
 * The closing brace is matched at the header's own indentation, which is what
 * lets this read a function nested in a Svelte `<script>` block as well as a
 * top-level one; prettier keeps that column, and CI keeps the tree formatted.
 * A shape this cannot read returns `null` so the caller fails loudly rather
 * than asserting over an empty string.
 */
export function tsFunctionBody(source, name) {
  const code = productionTsCode(source);
  const text = productionTsSource(source);
  const header = new RegExp(
    `^([ \\t]*)(?:export\\s+)?(?:default\\s+)?(?:async\\s+)?function\\s+${name}\\s*(?:<[^>]*>)?\\s*\\(`,
    'm',
  ).exec(code);
  if (header === null) return null;

  const rest = code.slice(header.index);
  let depth = 0;
  let i = header[0].length - 1;
  while (i < rest.length) {
    if (rest[i] === '(') depth += 1;
    else if (rest[i] === ')') {
      depth -= 1;
      if (depth === 0) break;
    }
    i += 1;
  }
  if (depth !== 0) return null;

  const open = rest.indexOf('{', i + 1);
  if (open < 0) return null;
  const close = rest.search(new RegExp(`\\n${header[1]}\\}`));
  if (close < 0 || close < open) return null;
  return text.slice(header.index + open + 1, header.index + close);
}

// ── The client's own request call sites ────────────────────────────────────
//
// `web/src` reaches the server through several transports: `request('<path>',
// { method })`, `downloadApiFile('<path>')`, direct `fetch(withApiBase(...))`
// and XHR where upload progress is needed. The API/client contract and the UI
// inventory read that surface here rather than each from its own regex, because
// a second, slightly-wrong parser is how a gate goes green without understanding
// anything: a generic argument containing a `;` (`request<{ id: number;
// username: string }>(...)`) is enough to make a naive pattern miss a live call
// and report a live route as an orphan.
//
// What comes back is what the *source* says — method, path with `${expr}`
// collapsed to `{param}`, and the raw config block — and nothing about what a
// caller then compares it to. Base-path conventions and body expectations are
// the caller's, and used to be baked in here, which is why this returns
// `config` as text rather than an interpreted body.

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
/**
 * The parameter name a template expression collapses to when its value cannot
 * be named — a call, an index, anything the reader would be guessing at.
 *
 * Exported because a caller has to be able to recognise it and skip the call
 * out loud rather than compare a guess against a real URL.
 */
export const OPAQUE_SEGMENT = '__opaque__';

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

  if (isQueryLikeTemplateExpression(text)) return '';
  if (keepAsPathSegment) return normalizeParamExpr(text);

  // An encoded identifier may own a path segment even when the wire spelling
  // adds a static suffix such as `.zip`. Keep this deliberately narrower than
  // `unwrapParamExpression`: an arbitrary expression inside encodeURIComponent
  // is not a statically-known route parameter.
  const encodedIdentifier = text.match(
    /^encodeURIComponent\(([A-Za-z_$][A-Za-z0-9_$]*)\)$/,
  );
  if ((previous === '/' || previous === '') && encodedIdentifier) {
    return encodedIdentifier[1];
  }

  // Preserve uncertainty instead of silently deleting a dynamic fragment and
  // manufacturing a different-looking URL. Callers fail closed on this marker.
  if (previous === '/' || previous === '') return OPAQUE_SEGMENT;
  return '';
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
      // The browser client keeps owner and repository values in encoded URL
      // segments via repoPath. Preserve that known route shape for the static
      // OpenAPI and UI-inventory checks instead of treating it as opaque.
      const repoPathArgs = expr?.expr.trim().match(/^repoPath\(\s*(owner)\s*,\s*(repo|name)\s*\)$/);
      const repoOwnerArgs = expr?.expr.trim().match(/^repoOwnerPath\(\s*(owner)\s*\)$/);
      if (repoPathArgs || repoOwnerArgs) {
        out += repoPathArgs ? `/repos/{owner}/{${repoPathArgs[2]}}` : '/repos/{owner}';
        i = nextIndex;
        continue;
      }
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
  return collapsed.replace(/\/+$/g, '') || '/';
}

function parseMethod(source, start) {
  if (source[start] !== '<') return start;

  const generic = readBalancedBlock(source, start, '<', '>');
  if (generic) {
    return generic.end;
  }
  return start;
}

function extractDirectRequestCalls(source, file) {
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
    let config = '';
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
          config = cfgText;
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
        config,
        transport: 'request',
        base: 'api',
        start: idx,
      });
      cursor = i + 1;
      continue;
    }

    cursor = idx + 1;
  }

  return calls;
}

function boundPathArgument(source, start, bindings) {
  const i = skipWhitespace(source, start);
  const literal = readStringLiteral(source, i);
  if (literal) return literal;

  const identifier = source.slice(i).match(/^([A-Za-z_$][A-Za-z0-9_$]*)/);
  if (!identifier || !Object.hasOwn(bindings, identifier[1])) return null;
  const resolved = boundPathArgument(String(bindings[identifier[1]]), 0, {});
  return resolved ? { value: resolved.value, end: i + identifier[0].length } : null;
}

function wrappedPathArgument(source, start, bindings, wrappers = []) {
  const i = skipWhitespace(source, start);
  for (const wrapper of wrappers) {
    if (!source.startsWith(wrapper, i)) continue;
    const after = source[i + wrapper.length];
    if (after && isIdentifierChar(after)) continue;
    const open = skipWhitespace(source, i + wrapper.length);
    if (source[open] !== '(') continue;
    const call = readBalancedBlock(source, open, '(', ')');
    if (!call) return null;
    const path = boundPathArgument(source, open + 1, bindings);
    return path ? { value: path.value, end: call.end, wrapper } : null;
  }
  const path = boundPathArgument(source, i, bindings);
  return path ? { ...path, wrapper: null } : null;
}

function methodAndConfig(source, start, fallback) {
  let i = skipWhitespace(source, start);
  let method = fallback;
  let config = '';
  if (source[i] === ',') {
    i = skipWhitespace(source, i + 1);
    if (source[i] === '{') {
      const cfg = readBalancedBlock(source, i, '{', '}');
      if (cfg) {
        config = cfg.text;
        const match = config.match(/method:\s*['\"]([A-Za-z]+)['\"]/i);
        if (match) method = match[1].toLowerCase();
      }
    }
  }
  return { method, config };
}

function namedCalls(source, code, name, file, bindings, options = {}) {
  const calls = [];
  const ranges = [];
  const callRanges = [];
  let cursor = 0;
  while (true) {
    const idx = code.indexOf(name, cursor);
    if (idx === -1) break;
    const before = code[idx - 1];
    const after = code[idx + name.length];
    if ((before && isIdentifierChar(before)) || (after && isIdentifierChar(after))) {
      cursor = idx + 1;
      continue;
    }
    if (options.requireNew) {
      let beforeNew = idx - 1;
      while (beforeNew >= 0 && /\s/.test(code[beforeNew])) beforeNew -= 1;
      const end = beforeNew + 1;
      while (beforeNew >= 0 && isIdentifierChar(code[beforeNew])) beforeNew -= 1;
      if (code.slice(beforeNew + 1, end) !== 'new') {
        cursor = idx + 1;
        continue;
      }
    }

    let i = skipWhitespace(code, idx + name.length);
    i = parseMethod(code, i);
    i = skipWhitespace(code, i);
    if (code[i] !== '(') {
      cursor = idx + 1;
      continue;
    }
    const block = readBalancedBlock(source, i, '(', ')');
    if (!block) {
      cursor = idx + 1;
      continue;
    }
    ranges.push({ start: idx, end: block.end });

    const path = wrappedPathArgument(source, i + 1, bindings, options.wrappers);
    if (path) {
      const { method, config } = methodAndConfig(
        source,
        path.end,
        options.defaultMethod ?? 'get',
      );
      calls.push({
        method,
        path: normalizeTemplatePath(path.value),
        file,
        config,
        transport: options.transport ?? name,
        base: options.baseByWrapper?.[path.wrapper] ?? options.defaultBase ?? 'api',
        start: idx,
      });
      callRanges.push({ start: idx, end: block.end });
    }
    cursor = block.end;
  }
  return { calls, ranges, callRanges };
}

function xhrCalls(source, code, file, bindings) {
  const calls = [];
  const ranges = [];
  const openCall = /\b[A-Za-z_$][A-Za-z0-9_$]*\s*\.\s*open\s*\(/g;
  let match;
  while ((match = openCall.exec(code)) !== null) {
    const open = code.indexOf('(', match.index);
    const block = readBalancedBlock(source, open, '(', ')');
    if (!block) continue;
    ranges.push({ start: match.index, end: block.end });

    const method = readStringLiteral(source, skipWhitespace(source, open + 1));
    if (!method) continue;
    let i = skipWhitespace(source, method.end);
    if (source[i] !== ',') continue;
    i = skipWhitespace(source, i + 1);
    const path = wrappedPathArgument(source, i, bindings, ['withApiBase']);
    if (!path) continue;
    calls.push({
      method: method.value.toLowerCase(),
      path: normalizeTemplatePath(path.value),
      file,
      config: '',
      transport: 'xhr',
      base: 'api',
      start: match.index,
    });
  }
  return { calls, ranges };
}

function insideAnyRange(index, ranges) {
  return ranges.some((range) => index >= range.start && index < range.end);
}

/**
 * Extract every HTTP-shaped call used by the browser client.
 *
 * `bindings` is deliberately one-hop and literal-only. `parseApiSurface` uses
 * it for helpers such as `uploadReleaseAsset(path, ...)`; anything more dynamic
 * stays absent instead of becoming an invented route.
 */
export function extractBrowserTransportCalls(source, file, { bindings = {} } = {}) {
  const code = productionTsCode(source);
  const calls = extractDirectRequestCalls(source, file);
  const claimed = [];

  const xhr = xhrCalls(source, code, file, bindings);
  calls.push(...xhr.calls);
  claimed.push(...xhr.ranges);

  const fetches = namedCalls(source, code, 'fetch', file, bindings, {
    wrappers: ['withApiBase', 'withBackendBase'],
    defaultMethod: 'get',
    defaultBase: 'root',
    baseByWrapper: { withApiBase: 'api', withBackendBase: 'root' },
    transport: 'fetch',
  });
  calls.push(...fetches.calls);
  claimed.push(...fetches.ranges);

  const downloads = namedCalls(source, code, 'downloadApiFile', file, bindings, {
    defaultMethod: 'get',
    defaultBase: 'api',
    transport: 'download',
  });
  calls.push(...downloads.calls);
  claimed.push(...downloads.ranges);

  // A WebSocket handshake is an HTTP GET even though the browser upgrades the
  // transport immediately afterwards. Require the constructor spelling so a
  // helper declaration or a URL string cannot become network evidence.
  const websockets = namedCalls(source, code, 'WebSocket', file, bindings, {
    wrappers: ['withWebSocketApiBase'],
    defaultMethod: 'get',
    defaultBase: 'root',
    baseByWrapper: { withWebSocketApiBase: 'api' },
    transport: 'websocket',
    requireNew: true,
  });
  calls.push(...websockets.calls);
  claimed.push(...websockets.ranges);

  const urls = namedCalls(source, code, 'withApiBase', file, bindings, {
    defaultMethod: 'get',
    defaultBase: 'api',
    transport: 'url',
  });
  calls.push(...urls.calls.filter((call, index) => (
    !insideAnyRange(urls.callRanges[index].start, claimed)
  )));

  const unique = new Map();
  for (const call of calls) {
    unique.set(`${call.method}\u0000${call.base}\u0000${call.path}`, call);
  }
  return [...unique.values()];
}

/**
 * HTTP/OpenAPI-shaped browser calls.
 *
 * A WebSocket starts with an HTTP GET handshake, but it is not an OpenAPI
 * operation. Keep that transport in the wider UI-surface extractor without
 * making every existing API-contract consumer special-case it.
 */
export function extractRequestCalls(source, file, options = {}) {
  return extractBrowserTransportCalls(source, file, options)
    .filter((call) => call.transport !== 'websocket');
}
