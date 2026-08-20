// Shared source inventory for consumer contract checks.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';

const EXCLUDED_DIRS = new Set(['benches', 'target', 'tests']);

function blankExceptNewlines(source) {
  return source.replace(/[^\n]/g, ' ');
}

function quotedEnd(source, start) {
  const quote = source[start];
  let i = start + 1;
  while (i < source.length) {
    if (source[i] === '\\') {
      i += 2;
      continue;
    }
    i += 1;
    if (source[i - 1] === quote) break;
  }
  return i;
}

function charLiteralEnd(source, start) {
  if (source[start] !== "'") return null;
  let i = start + 1;
  if (source[i] === '\\') {
    i += 1;
    if (source[i] === 'x') {
      i += 3;
    } else if (source[i] === 'u' && source[i + 1] === '{') {
      const close = source.indexOf('}', i + 2);
      if (close < 0) return null;
      i = close + 1;
    } else {
      i += 1;
    }
  } else {
    const point = source.codePointAt(i);
    if (point === undefined || point === 0x0a || point === 0x0d) return null;
    i += point > 0xffff ? 2 : 1;
  }
  return source[i] === "'" ? i + 1 : null;
}

function rawStringEnd(source, start) {
  const match = /^(?:br|r)(#+)?"/.exec(source.slice(start));
  if (!match) return null;
  const hashes = match[1] ?? '';
  const close = `"${hashes}`;
  const end = source.indexOf(close, start + match[0].length);
  return end < 0 ? source.length : end + close.length;
}

/**
 * One lexical pass over Rust source that blanks the non-executable spans.
 *
 * `blankStrings` decides whether string and char literals are blanked too or
 * copied through verbatim. Either way the literal is *lexed* — a `//` inside
 * `r#"…"#` is data, not the beginning of a comment — and every blanked span
 * keeps its length and its newlines, so the returned view is byte-aligned with
 * the input. That alignment is the point: two views of the same file taken
 * from this scanner can be read at the same offsets, which is what lets a
 * parser find its tokens in code-only text and then slice the match out of the
 * text that still has the strings in it.
 */
function blankRustNonCode(source, { blankStrings }) {
  let out = '';
  let i = 0;
  const literal = (end) => {
    const span = source.slice(i, end);
    out += blankStrings ? blankExceptNewlines(span) : span;
    i = end;
  };
  while (i < source.length) {
    if (source.slice(i, i + 2) === '//') {
      const end = source.indexOf('\n', i + 2);
      const stop = end < 0 ? source.length : end;
      out += blankExceptNewlines(source.slice(i, stop));
      i = stop;
      continue;
    }
    if (source.slice(i, i + 2) === '/*') {
      let depth = 1;
      let end = i + 2;
      while (end < source.length && depth > 0) {
        if (source.slice(end, end + 2) === '/*') {
          depth += 1;
          end += 2;
        } else if (source.slice(end, end + 2) === '*/') {
          depth -= 1;
          end += 2;
        } else {
          end += 1;
        }
      }
      out += blankExceptNewlines(source.slice(i, end));
      i = end;
      continue;
    }
    const rawEnd = rawStringEnd(source, i);
    if (rawEnd !== null) {
      literal(rawEnd);
      continue;
    }
    const charEnd = charLiteralEnd(source, i);
    if (charEnd !== null) {
      literal(charEnd);
      continue;
    }
    if (source.slice(i, i + 2) === 'b"') {
      literal(quotedEnd(source, i + 1));
      continue;
    }
    if (source[i] === '"') {
      literal(quotedEnd(source, i));
      continue;
    }
    out += source[i];
    i += 1;
  }
  return out;
}

/**
 * Replace Rust comments and string literals with spaces while preserving line
 * numbers. Doing both in one lexical pass matters for raw strings: a `//`
 * inside `r#"…"#` is data, not the beginning of a comment.
 */
export function stripRustNonCode(source) {
  return blankRustNonCode(source, { blankStrings: true });
}

/**
 * Replace Rust comments with spaces, keeping string literals intact — the
 * string-preserving twin of `stripRustNonCode`, byte-aligned with it.
 *
 * Use it when the text being read *is* a string literal (an annotation's
 * `path = "…"`, a parameter description) but the token that located it must
 * come from executable code only.
 */
export function blankRustComments(source) {
  return blankRustNonCode(source, { blankStrings: false });
}

function attributeEnd(source, start) {
  let depth = 0;
  let i = start;
  while (i < source.length) {
    const rawEnd = rawStringEnd(source, i);
    if (rawEnd !== null) {
      i = rawEnd;
      continue;
    }
    if (source[i] === '"' || source[i] === '\'') {
      i = quotedEnd(source, i);
      continue;
    }
    if (source[i] === '[') depth += 1;
    else if (source[i] === ']') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
    i += 1;
  }
  return source.length;
}

function attributedItemEnd(source, start) {
  let i = start;
  while (i < source.length) {
    const rawEnd = rawStringEnd(source, i);
    if (rawEnd !== null) {
      i = rawEnd;
      continue;
    }
    if (source[i] === '"' || source[i] === '\'') {
      i = quotedEnd(source, i);
      continue;
    }
    if (source[i] === ';') return i + 1;
    if (source[i] !== '{') {
      i += 1;
      continue;
    }

    let depth = 1;
    i += 1;
    while (i < source.length && depth > 0) {
      const nestedRawEnd = rawStringEnd(source, i);
      if (nestedRawEnd !== null) {
        i = nestedRawEnd;
        continue;
      }
      if (source[i] === '"' || source[i] === '\'') {
        i = quotedEnd(source, i);
        continue;
      }
      if (source[i] === '{') depth += 1;
      else if (source[i] === '}') depth -= 1;
      i += 1;
    }
    return i;
  }
  return source.length;
}

/**
 * The `[start, end)` spans of every item whose `#[cfg(...)]` expression
 * mentions the `test` atom.
 *
 * Returned separately from the blanking so the spans can be located in one
 * view and blanked in another: item boundaries must be counted where strings
 * and comments are already spaces (a `}` inside a raw string is data, not the
 * end of a test module), while the view a caller reads values out of still
 * needs its literals. Both views come from the same byte-aligned scanner, so
 * one set of offsets addresses either.
 */
export function cfgTestItemRanges(source) {
  const ranges = [];
  let cursor = 0;
  const marker = /^[ \t]*#\[cfg\([^\]]*\btest\b[^\]]*\)\]/gm;
  for (const match of source.matchAll(marker)) {
    if (match.index < cursor) continue;
    let itemStart = match.index + match[0].length;

    // Keep sibling attributes on the same item inside the removed span.
    while (true) {
      const next = /^[ \t\r\n]*#\[/.exec(source.slice(itemStart));
      if (!next) break;
      const attributeStart = itemStart + next[0].lastIndexOf('#');
      itemStart = attributeEnd(source, attributeStart);
    }

    const end = attributedItemEnd(source, itemStart);
    ranges.push([match.index, end]);
    cursor = end;
  }
  return ranges;
}

/**
 * `view` with `ranges` replaced by spaces, newlines and length preserved.
 *
 * Keeping the length is what lets the result stay byte-aligned with every
 * other view of the same file, and keeping the newlines is what keeps
 * diagnostics on their original lines.
 */
export function blankRanges(view, ranges) {
  let out = '';
  let cursor = 0;
  for (const [start, end] of ranges) {
    if (start < cursor) continue;
    out += view.slice(cursor, start);
    out += blankExceptNewlines(view.slice(start, end));
    cursor = end;
  }
  return out + view.slice(cursor);
}

/**
 * Drop every item whose `#[cfg(...)]` expression mentions the `test` atom.
 *
 * The removed span is replaced with spaces (newlines survive), so two tokens
 * on either side cannot be glued into a made-up call and diagnostics retain
 * their original line numbers.
 */
export function stripCfgTestItems(source) {
  return blankRanges(source, cfgTestItemRanges(source));
}

/** Recursively list Rust files while excluding test-only source trees. */
export function rustFiles(dir) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (entry.name.startsWith('.')) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!EXCLUDED_DIRS.has(entry.name)) found.push(...rustFiles(full));
    } else if (entry.isFile() && entry.name.endsWith('.rs')) {
      found.push(full);
    }
  }
  return found;
}

/** Load comment-free, test-item-free Rust source for the production workspace. */
export function loadProductionRust(cratesDir) {
  const production = new Map();
  for (const file of rustFiles(cratesDir)) {
    production.set(
      file,
      stripCfgTestItems(stripRustNonCode(readFileSync(file, 'utf8'))),
    );
  }
  return production;
}

/**
 * Inventory public free functions below `scannedDirs` and find names with no
 * call-shaped production occurrence anywhere in `production`.
 */
export function findPublicFunctionOrphans({ root, scannedDirs, production }) {
  const scannedFiles = scannedDirs.flatMap((dir) => rustFiles(path.join(root, dir)));
  const declarations = [];
  // ABI strings have already been blanked by `stripRustNonCode`, so
  // `extern "C" fn` reaches this pass as `extern     fn`.
  const declaration = /^pub\s+(?:(?:async|const|unsafe)\s+)*(?:extern\s+)?fn\s+(\w+)/gm;

  for (const file of scannedFiles) {
    const source =
      production.get(file) ??
      stripCfgTestItems(stripRustNonCode(readFileSync(file, 'utf8')));
    for (const match of source.matchAll(declaration)) {
      declarations.push({ name: match[1], file });
    }
  }

  const orphans = [];
  for (const { name, file } of declarations) {
    let consumers = 0;
    for (const source of production.values()) {
      consumers += consumerCalls(source, name);
      if (consumers > 0) break;
    }
    if (consumers === 0) orphans.push({ name, file });
  }

  return { declarations, orphans, scannedFiles };
}

/**
 * Occurrences in `source` that could be reaching the FREE function `name`.
 *
 * The whole inventory above is free functions and nothing else: the
 * declaration regex is anchored at column zero, and an inherent or trait
 * method is written indented inside its `impl` or `trait` block. That is what
 * makes the receiver question cheap here — a free function cannot be reached
 * through a dot or through a `Type::` qualifier, so those two spellings can be
 * dropped without resolving a single type.
 *
 * `\b${name}\s*\(` alone did not ask, and the word boundary sits happily
 * after a dot, so any `whatever.name(…)` on any type in the tree answered for
 * a function nothing calls — and this gate only reddens at zero. The same
 * collision was settled the same way one check over, in
 * `authz-gate-dialect-contract-check.mjs` (card_bfaf56626c5f); the difference
 * is the surface, 962 public functions instead of 31 gates, and that here it
 * was already hiding two real orphans rather than waiting to.
 *
 * Spelling by spelling:
 *
 *   - bare `name(` and `module::name(` (lowercase qualifier — Rust spells a
 *     module in snake_case) reach a free function and nothing else.
 *   - `Type::name(` and `Self::name(` reach the method of a type. No type
 *     declares this name at column zero, so such a caller is a phantom.
 *   - `receiver.name(` reaches a method, never a free function.
 *   - `<T as Trait>::name(` names no segment this reader can weigh, so it is
 *     COUNTED: a spelling the lock does not parse must not manufacture an
 *     accusation.
 *   - `fn name(` is the declaration itself, in whichever file it lives. Asking
 *     the shape rather than the filename also stops a same-named method's own
 *     declaration from counting as a consumer of this one.
 *
 * Callers may select generic arguments explicitly — `unseal_state::<RegState>(…)`
 * — and counting only `name(` once made a live passkey boundary look orphaned,
 * so that form stays part of the call shape.
 */
function consumerCalls(source, name) {
  const call = new RegExp(
    `(?<![A-Za-z0-9_])${name}\\s*(?:(?:::\\s*)?<[^;{}()]*>)?\\s*\\(`,
    'g',
  );
  let count = 0;
  for (let m = call.exec(source); m !== null; m = call.exec(source)) {
    const before = source.slice(0, m.index);
    if (/\bfn\s+$/.test(before)) continue;
    if (/\.\s*$/.test(before)) continue;
    const qualifier = /(\w+)\s*::\s*$/.exec(before);
    if (qualifier !== null && /^[A-Z]/.test(qualifier[1])) continue;
    count += 1;
  }
  return count;
}
