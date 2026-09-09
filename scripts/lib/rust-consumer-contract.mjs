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
    const charEnd = charLiteralEnd(source, i);
    if (charEnd !== null) {
      i = charEnd;
      continue;
    }
    if (source[i] === '"') {
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
    const charEnd = charLiteralEnd(source, i);
    if (charEnd !== null) {
      i = charEnd;
      continue;
    }
    if (source[i] === '"') {
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
      const nestedCharEnd = charLiteralEnd(source, i);
      if (nestedCharEnd !== null) {
        i = nestedCharEnd;
        continue;
      }
      if (source[i] === '"') {
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
 * The comma-separated arguments of `name(...)`, when `predicate` is exactly
 * that call, or `null` when it is anything else.
 *
 * Splitting on top-level commas only is what keeps `all(test, any(unix,
 * windows))` from being read as three siblings.
 */
function cfgListArguments(predicate, name) {
  const rest = predicate.trim();
  if (!rest.startsWith(name)) return null;
  const call = rest.slice(name.length).trimStart();
  if (!call.startsWith('(') || !call.endsWith(')')) return null;
  const inner = call.slice(1, -1);

  const args = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < inner.length; i += 1) {
    if (inner[i] === '(') depth += 1;
    else if (inner[i] === ')') depth = Math.max(0, depth - 1);
    else if (inner[i] === ',' && depth === 0) {
      args.push(inner.slice(start, i).trim());
      start = i + 1;
    }
  }
  const tail = inner.slice(start).trim();
  if (tail !== '') args.push(tail);

  return args;
}

/**
 * Whether a `cfg` predicate is false in every build that is not a test build.
 *
 * `all(test, unix)` is: dropping `test` drops the item. `any(test, unix)` is
 * not — the item still compiles on unix without `cfg(test)` — and neither is
 * `not(test)`, which is the production half of a pair, nor `feature =
 * "test-utils"`, which is an ordinary feature gate. Anything this cannot read
 * is treated as production, so an unknown spelling keeps code visible to a
 * census rather than hiding it.
 *
 * The Rust twin of this function is `cfg_predicate_is_test_only` in
 * `tests/support/rust_source.rs`, and the two views must agree. That sentence
 * used to be the whole contract, which is how the halves came to drift apart in
 * opposite directions at the same moment. They now answer one shared fixture
 * table, `tests/support/cfg-test-attribute-parity.txt`: this half runs it in
 * `scripts/cfg-test-reader-contract-check.mjs`, the Rust half in
 * `crates/rg-cli/src/cli.rs`, and the same check refuses a third reader.
 */
function cfgPredicateIsTestOnly(predicate) {
  const trimmed = predicate.trim();
  if (trimmed === 'test') return true;

  const all = cfgListArguments(trimmed, 'all');
  if (all !== null) return all.some((argument) => cfgPredicateIsTestOnly(argument));

  const any = cfgListArguments(trimmed, 'any');
  if (any !== null)
    return any.length > 0 && any.every((argument) => cfgPredicateIsTestOnly(argument));

  return false;
}

/**
 * Whether `attribute` gates the item that follows it to test builds.
 *
 * Reads the `cfg` predicate rather than grepping the attribute for the word
 * `test`: `#[cfg(not(test))]`, `#[cfg(any(test, unix))]` and `#[cfg(feature =
 * "test-utils")]` all compile without `cfg(test)`, and blanking them takes
 * real production code away from the 33 scripts that read this view
 * (card_fc3daaf07a4c).
 */
function isTestOnlyCfgAttribute(attribute) {
  const trimmed = attribute.trim();
  if (!trimmed.startsWith('#[') || !trimmed.endsWith(']')) return false;
  const args = cfgListArguments(trimmed.slice(2, -1), 'cfg');
  return args !== null && args.length === 1 && cfgPredicateIsTestOnly(args[0]);
}

/**
 * The `[start, end)` spans of every item whose `#[cfg(...)]` expression is
 * true only in a test build.
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
  // Every attribute opening a line is a candidate; the predicate decides,
  // and `attributeEnd` balances the brackets so a rustfmt-wrapped
  // `#[cfg(all(\n test,\n unix\n))]` is read whole rather than as an opener
  // naming nothing.
  const marker = /^[ \t]*#\[/gm;
  for (const match of source.matchAll(marker)) {
    if (match.index < cursor) continue;
    const attributeStart = match.index + match[0].lastIndexOf('#');
    const markerEnd = attributeEnd(source, attributeStart);
    if (!isTestOnlyCfgAttribute(source.slice(attributeStart, markerEnd))) continue;
    let itemStart = markerEnd;

    // Keep sibling attributes on the same item inside the removed span.
    while (true) {
      const next = /^[ \t\r\n]*#\[/.exec(source.slice(itemStart));
      if (!next) break;
      const siblingStart = itemStart + next[0].lastIndexOf('#');
      itemStart = attributeEnd(source, siblingStart);
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
 * The module a free function declared in `file` is reached through.
 *
 * `foo.rs` is module `foo`, `foo/mod.rs` is module `foo`, and `lib.rs` /
 * `main.rs` are the crate root — reached through the crate name, which is the
 * directory under `crates/` with its hyphens turned into underscores.
 */
export function declaringModule(file) {
  const base = path.basename(file);
  if (base === 'lib.rs' || base === 'main.rs') {
    const crate = /(?:^|[\\/])crates[\\/]([^\\/]+)[\\/]src[\\/]/.exec(file);
    return crate === null ? null : crate[1].replace(/-/g, '_');
  }
  if (base === 'mod.rs') return path.basename(path.dirname(file));
  return base.slice(0, -'.rs'.length);
}

/** Every leaf name a `use` tree brings into scope, plus the globbed modules. */
function useFacts(source) {
  const imported = new Set();
  const globbedModules = new Set();
  const reexported = new Set();
  // `pub use service::*;` in `pull_request/mod.rs` — every name of `service`
  // is then also reached as `pull_request::name(`, and that spelling is the
  // one the rest of the workspace uses.
  const globReexported = new Set();
  // `use rg_core::auth::webauthn as wa;` — the call site then spells the
  // module `wa`, which names this module and no other. Eleven live WebAuthn
  // entry points read as orphans until the aliases were followed.
  const moduleAliases = new Map();
  for (const statement of source.matchAll(/\b(pub\s+)?use\s+([^;]+);/g)) {
    const tree = statement[2].replace(/\s+/g, ' ');
    for (const glob of tree.matchAll(/(\w+)\s*::\s*\*/g)) {
      globbedModules.add(glob[1]);
      if (statement[1]) globReexported.add(glob[1]);
    }
    for (const alias of tree.matchAll(/(\w+)\s+as\s+(\w+)/g)) {
      moduleAliases.set(alias[2], alias[1]);
    }
    for (const token of tree.matchAll(/(\w+)(\s*::)?/g)) {
      // A segment followed by `::` is a path step; anything else is a leaf, and
      // a leaf is what the file may then call by its bare name. `x as y` adds
      // both, which is the lenient direction on purpose.
      if (token[2]) continue;
      imported.add(token[1]);
      if (statement[1]) reexported.add(token[1]);
    }
  }
  return { imported, globbedModules, reexported, globReexported, moduleAliases };
}

/**
 * Inventory public free functions below `scannedDirs` and find the ones no
 * production file calls **through the module that declares them**.
 *
 * The bare name is not the key. `consumerCalls` used to count `whatever::name(`
 * whatever `whatever` was, so a namesake in an unrelated module answered for a
 * dead function: `pr_review_ops::count_approvals` was reported alive by
 * `ci_environment_ops::count_approvals(` — a different table, a different
 * feature, the same word. A gate that only reddens at zero cannot notice that,
 * and 78 names in this inventory are declared more than once, so the cover was
 * not a freak (card_2e1763c24075).
 *
 * What each file is allowed to answer with is therefore decided per file:
 *
 *   - the declaring file may call it by its bare name — a sibling in the same
 *     module is a real consumer, and the arc being short does not make it
 *     absent;
 *   - any file may call it as `<declaring module>::name(`, or through a module
 *     that `pub use`s the name (a re-export is a second true spelling);
 *   - a file that imported the name — `use …::name;`, or a glob of the
 *     declaring module — may call it bare;
 *   - everything else is somebody else's function with the same name.
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
      declarations.push({ name: match[1], file, module: declaringModule(file) });
    }
  }

  // One pass over the workspace for the two things the per-declaration loop
  // below has to ask of every file: what it imported, and what it re-exports.
  const facts = new Map();
  const reexportQualifiers = new Map();
  const globReexportQualifiers = new Map();
  for (const [file, source] of production) {
    const fileFacts = useFacts(source);
    facts.set(file, fileFacts);
    const module = declaringModule(file);
    if (module === null) continue;
    for (const name of fileFacts.reexported) {
      if (!reexportQualifiers.has(name)) reexportQualifiers.set(name, new Set());
      reexportQualifiers.get(name).add(module);
    }
    for (const source of fileFacts.globReexported) {
      if (!globReexportQualifiers.has(source)) globReexportQualifiers.set(source, new Set());
      globReexportQualifiers.get(source).add(module);
    }
  }
  const emptyFacts = {
    imported: new Set(),
    globbedModules: new Set(),
    reexported: new Set(),
    globReexported: new Set(),
    moduleAliases: new Map(),
  };

  const orphans = [];
  for (const { name, file, module } of declarations) {
    let consumers = 0;
    for (const [candidate, source] of production) {
      consumers += consumerCalls(source, name, {
        module,
        declaringFile: candidate === file,
        facts: facts.get(candidate) ?? emptyFacts,
        reexportedBy: reexportQualifiers.get(name) ?? null,
        globReexportedBy: globReexportQualifiers.get(module) ?? null,
      });
      if (consumers > 0) break;
    }
    if (consumers === 0) orphans.push({ name, file, module });
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
 * dropped without resolving a single type. `scope` carries the rest of the
 * question, which the name alone cannot answer: which module declares this
 * function, and what the file being scanned is allowed to reach it with.
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
 * Spelling by spelling:
 *
 *   - `<declaring module>::name(` reaches this function and nothing else. Any
 *     OTHER lowercase qualifier reaches somebody else's function of the same
 *     name, and used to be counted — see `findPublicFunctionOrphans`.
 *   - a qualifier a module `pub use`s the name through is a second true
 *     spelling of the same function, so it counts as well — and so does the
 *     alias a file gave the declaring module in `use … as …`, and the module
 *     that re-exports the declaring one wholesale with `pub use mod::*;`.
 *   - bare `name(` reaches this function from the declaring file, from a file
 *     that imported the name, and from a file that globbed the declaring
 *     module. Anywhere else it is a different symbol with the same word.
 *   - `Type::name(` and `Self::name(` reach the method of a type. No type
 *     declares this name at column zero, so such a caller is a phantom.
 *   - `receiver.name(` reaches a method, never a free function.
 *   - `<T as Trait>::name(`, and the `crate::` / `self::` / `super::`
 *     positions, name no module this reader can weigh, so they are COUNTED: a
 *     spelling the lock does not parse must not manufacture an accusation.
 *   - `fn name(` is the declaration itself, in whichever file it lives. Asking
 *     the shape rather than the filename also stops a same-named method's own
 *     declaration from counting as a consumer of this one.
 *
 * Callers may select generic arguments explicitly — `unseal_state::<RegState>(…)`
 * — and counting only `name(` once made a live passkey boundary look orphaned,
 * so that form stays part of the call shape.
 */
function consumerCalls(source, name, scope) {
  const call = new RegExp(
    `(?<![A-Za-z0-9_])${name}\\s*(?:(?:::\\s*)?<[^;{}()]*>)?\\s*\\(`,
    'g',
  );
  let count = 0;
  for (let m = call.exec(source); m !== null; m = call.exec(source)) {
    const before = source.slice(0, m.index);
    if (/\bfn\s+$/.test(before)) continue;
    if (/\.\s*$/.test(before)) continue;
    // `<T as Trait>::name(` — no segment this reader can weigh, so it counts.
    if (/>\s*::\s*$/.test(before)) {
      count += 1;
      continue;
    }
    const qualifier = /(\w+)\s*::\s*$/.exec(before);
    if (qualifier !== null) {
      const segment = qualifier[1];
      if (/^[A-Z]/.test(segment)) continue;
      // `crate::` / `self::` / `super::` name a position rather than a module,
      // so they are counted for the same reason the qualified trait path is.
      if (segment === 'crate' || segment === 'self' || segment === 'super') {
        count += 1;
        continue;
      }
      const resolved = scope.facts.moduleAliases.get(segment) ?? segment;
      if (
        resolved !== scope.module
        && segment !== scope.module
        && !(scope.reexportedBy?.has(segment) ?? false)
        && !(scope.globReexportedBy?.has(segment) ?? false)
      ) {
        continue;
      }
      count += 1;
      continue;
    }
    if (
      !scope.declaringFile
      && !scope.facts.imported.has(name)
      && !scope.facts.globbedModules.has(scope.module)
    ) {
      continue;
    }
    count += 1;
  }
  return count;
}
