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
