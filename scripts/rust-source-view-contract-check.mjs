#!/usr/bin/env node

// Asserts that no Rust source guard reads a `.rs` file and then makes a
// textual assertion about the bytes it got back.
//
// Why this exists, and why it is a second file rather than a row in
// `raw-source-assertion-contract-check.mjs`: that ratchet takes
// `scripts/**/*.mjs` as its subject. It reads JavaScript checks and asks what
// they do with the Rust they load. The guards that live *in* the workspace —
// `crates/**/tests/integration/*_guard.rs`, the `#[cfg(test)]` censuses inside
// `crates/**/src/**`, the shared readers under `tests/support/` — are outside
// it entirely, and they are the larger half of the corpus.
//
// The argument that ratchet makes for JavaScript was still unanswered here. The
// same class has been closed by hand on the Rust side at least nine times —
// `da8f8a5` (rg-git, rg-cli), `0c2e49a` (rg-mcp), `b10e155` (source_scan),
// `42ea422` (audit_writer_guard), `bb71636` (foreign_gate_guard), `6b5bad8`
// (three censuses across rg-core / rg-ssh / rg-db), `f4cf30f` (rg-ssh
// integration), `c4f4cb6` (rg-core issue_template), `78320b4` (rg-ci
// gitea_actions) — each round found the remaining offenders with a fresh manual
// sweep, because nothing in the repository objected to a tenth. Four recurring
// gotcha cards carry counts of 14x, 5x, 5x and 4x for it. This is the ratchet
// (card_cc3c58b1098b).
//
// What a raw read buys the reader, stated as the failure it produces: a
// construct that was commented out, or that lives inside a `#[cfg(test)]`
// fixture, or that is spelled inside a string literal, still satisfies
// `source.contains("audit_log::ActiveModel")`. The guard goes green over code
// that never runs, or red over code that does not exist. Both directions have
// been live in this tree, and the quiet one is the one a passing check cannot
// tell you about.
//
// How it reads a Rust file:
//   - a *raw read* is `include_str!("….rs")`, or `fs::read_to_string(…)` in a
//     function that also names a `.rs` path, calls a discovered `.rs` walker,
//     or IS one — a census that spells its own `read_dir` loop rather than
//     calling a helper names Rust just as plainly, and keying on the call alone
//     left the whole read unrecognised. The binding it sits under may be a
//     tuple element rather than the `=` itself, which is how one gate's read
//     went uncounted and therefore unheld by the floor as well;
//   - a *normalizer* is discovered, not listed: seeded with the named views of
//     `tests/support/rust_source.rs`, then closed over every `fn` whose own
//     body calls one. `call_site_contains` qualifies because it calls
//     `production_rust_code_only`; `signed_limit_field_lines` because it calls
//     it on the way to its inner reader. A hand-written list of laundering
//     helpers would be the defect this file exists to close, one level up;
//   - an *assertion* is a text method applied to the bytes (`contains`,
//     `lines`, `find`, `match_indices`, `split*`, `starts_with`, `strip_*`), or
//     the bytes being handed to a `fn` of the corpus that is not a normalizer —
//     the local half matters most, because a guard's own `contract(source)` is
//     where all of its assertions go.
//
// Two seed sets, and the split is the same one card_04cdbcb8d553 paid for on
// the JavaScript side. `production_rust_*` blanks complete `#[cfg(test)]`
// items; `rust_code_only` deliberately does not. A sweep that *requires* a
// construct to be present is fooled by a test double declaring it and must use
// a production view. A sweep that *reports* what it finds cannot be — the worst
// a fixture can do there is ask for a look. Both are named views with the rule
// in their docstring, so the test-inclusive half is an intent someone spelled
// rather than a name parked on an exclusion list.
//
// This check reads its own subject the way it demands, and it reaches for the
// test-inclusive half of the pair on purpose: the guards it audits live inside
// `#[cfg(test)] mod tests`, so a production view would blank the entire
// subject. That is the intent `testInclusiveRustCode` / `testInclusiveRustSource`
// exist to state — this sweep only REPORTS what it finds, so the worst a fixture
// can do is ask for a look. Constructs are located in the code-only view and
// the path literal behind an `include_str!` is read out of the string-bearing
// twin at the same byte offset.
//
// Three shapes a read is reached through after it is bound, each one hop and no
// more. A *derivation* is what a view handed BACK about the bytes; a *rename* is
// the bytes themselves under a second name (`let production = source.to_owned()`
// asks nothing, so what it binds is still the read); a *tuple slot* is what a
// consumer unpacks out of a `const` that carries a path and its bytes together
// (`const ALIAS: (&str, &str) = ("crates/…/cli.rs", include_str!(…))`). The slot
// travels with the read for the same reason `tupleSlot` exists for a producer:
// slot 0 there is a PATH a guard quotes in its own failure message, and
// accusing `ALIAS.0` of a raw source assertion is a ratchet nobody keeps green.
// The pattern is resolved inside the function that spells it, never module-wide
// — `source` is a name half a guard file uses.
//
// One derivation forward, which is the reach a single-hop reader used to stop
// short of. A guard that binds bytes, hands them to a named view, and then
// greps something the view HANDED BACK was out of reach: `functions(&text)`
// launders `text`, and the `function.body` it returns is the ORIGINAL bytes of
// that function. The distinction that makes following it safe is what the view
// gives back. A *view alias* — a `fn` whose body is a named view applied and
// returned, `production_source()` being the shape this tree writes — hands back
// the view itself, so everything derived from it is clean and is not followed.
// Every other derived normalizer hands back something ABOUT the bytes, and a
// binding taken off one is asked the same question its source was. Exactly one
// hop: a second would tail the whole program off a single read and a lexical
// reader has no way to stop.
//
// What it does NOT see, stated because a ratchet with an unrecorded blind spot
// is how this class survives:
//   - the second derivation — `handlers(source)` then `handler.body[sig..]` is
//     two hops, and one is where this reader stops;
//   - the second axis of the seed split. `production_rust_source` blanks test
//     items but KEEPS comments and literals on purpose, so a binding taken off
//     it is still text a comment can fool — and this reader treats it as a view
//     alias, i.e. as finished (card_2f5905fd48bd);
//   - any laundering a guard spells in a shape this reader has not been taught.
// It errs toward silence there on purpose, because a ratchet nobody can keep
// green is one somebody deletes. The floor is what covers the half that silence
// cannot.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { testInclusiveRustCode, testInclusiveRustSource } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));

// The mutation stand points this at a fixture tree, so it drives the real sweep
// rather than a copy of it.
const override = process.env.FORGEKEEP_RUST_VIEW_ROOT;
const root = override ? resolve(override) : resolve(scriptsDir, '..');

/** Where Rust that reads Rust lives. */
const SUBJECT_DIRS = ['crates', 'tests'];

/**
 * The views of `tests/support/rust_source.rs` a reader may anchor in.
 *
 * `production` blanks complete `#[cfg(test)]` items as well as comments and
 * literals. `testInclusive` blanks comments and literals only, and is the right
 * view for a sweep whose subject includes fixtures — `released-port` hunts a
 * listener dropped inside a `#[cfg(test)]` item, and the production view would
 * hide exactly what it came for.
 */
const VIEW_SEEDS = {
  production: [
    'production_rust_code_only',
    'production_rust_source',
    'production_rust_code_with_doc_comments',
  ],
  testInclusive: ['rust_code_only'],
};

const IDENT = '[A-Za-z_][A-Za-z0-9_]*';

/**
 * The shims that hand the same value on: `?`, the unwrapping pair and the
 * ownership conversions. None of them asks anything about the bytes, so a value
 * that passes through one is still the same bytes under whatever name it lands
 * under.
 */
const SHIM = String.raw`\?|\.\s*(?:unwrap|to_owned|to_string|into|clone|as_str|as_ref)\s*\(\s*\)|\.\s*expect\s*\([^()]*\)`;

/**
 * Text methods that turn bytes into a verdict.
 *
 * Deliberately the searching half of `str` and not all of it: `len()` or
 * `to_owned()` on raw bytes says nothing about the program, while `contains`
 * is the whole defect.
 */
const ASSERTIONS = [
  'contains',
  'lines',
  'find',
  'rfind',
  'match_indices',
  'matches',
  'split',
  'splitn',
  'rsplit',
  'split_once',
  'rsplit_once',
  'split_terminator',
  'split_whitespace',
  'starts_with',
  'ends_with',
  'strip_prefix',
  'strip_suffix',
];

/**
 * The floor. A reader that has stopped recognising reads reports a clean corpus
 * over files it never understood, and nobody investigates green. The count is a
 * lower bound on the `include_str!("….rs")` and walk-fed reads this tree
 * carries; it is allowed to grow and is not allowed to quietly collapse.
 */
const MIN_READS = 50;
const minReads = override ? Number(process.env.FORGEKEEP_RUST_VIEW_MIN ?? 0) : MIN_READS;

/** Every `.rs` file below `dir`, recursively. */
function rustFiles(dir) {
  const found = [];
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return found;
  }
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name.startsWith('.') || entry.name === 'target') continue;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) found.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) found.push(full);
  }
  return found;
}

/** The index just past the `}` closing the block that opens at `open`. */
function blockEnd(code, open) {
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === '{') depth += 1;
    else if (code[i] === '}') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return code.length;
}

/** The index just past the `)` closing the group that opens at `open`. */
function parenEnd(code, open) {
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === '(') depth += 1;
    else if (code[i] === ')') {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return code.length;
}

/** A directory enumeration. */
const ENUMERATES = /\bread_dir\s*\(|\bWalkDir\s*::/;

/**
 * Both spellings of the extension: `ends_with(".rs")` carries the dot,
 * `path.extension() == "rs"` does not, and the one walker `tests/support/`
 * ships is written the second way.
 */
const NAMES_RUST = /\.rs["']|["']rs["']/;

/**
 * Whether `code` walks a directory and `text` says the walk is about Rust.
 *
 * The extension is a LITERAL, so it is read out of the string-bearing twin —
 * the code-only view the calls are found in has already blanked it, which is
 * how this reader first failed to see the one walker the tree ships. A bare
 * directory read that names no extension is a walk of something else:
 * `read_dir` over `crates/` looking for `Cargo.toml` must not make every read
 * of its result look like a read of Rust.
 */
function enumeratesRust(code, text) {
  return ENUMERATES.test(code) && NAMES_RUST.test(text);
}

/**
 * Every `fn` declared in `code`, with the byte range of its body.
 *
 * The body opens at the first `{` after the declaration, which in the code-only
 * view is the body's own brace: a `{` in a where-clause bound or a default
 * argument would be a struct literal, and Rust signatures do not carry one.
 */
function rustFunctions(code) {
  const out = [];
  const declaration = new RegExp(`\\bfn\\s+(${IDENT})\\s*[(<]`, 'g');
  for (let m = declaration.exec(code); m !== null; m = declaration.exec(code)) {
    const open = code.indexOf('{', m.index);
    if (open < 0) continue;
    out.push({ name: m[1], start: m.index, open, end: blockEnd(code, open) });
  }
  return out;
}

/**
 * The `fn` names of the corpus whose body reaches one of `seeds`.
 *
 * Closed over transitively, the way the JavaScript ratchet discovers its own:
 * a wrapper around a production view is a production view, and a list of them
 * kept by hand is a list somebody forgets to extend.
 */
function discoverNormalizers(functions, seeds) {
  const normalizers = new Set(seeds);
  for (let pass = 0; pass < 8; pass += 1) {
    let grew = false;
    for (const fn of functions) {
      if (normalizers.has(fn.name)) continue;
      for (const known of normalizers) {
        if (new RegExp(`\\b${known}\\s*\\(`).test(fn.body)) {
          normalizers.add(fn.name);
          grew = true;
          break;
        }
      }
    }
    if (!grew) break;
  }
  return normalizers;
}

/**
 * The normalizers that hand the VIEW ITSELF back, seeds included.
 *
 * This is the line between a derivation worth following and one that is already
 * clean. `production_source()` is `production_rust_source(include_str!(…))` and
 * nothing else, so what it returns IS the view and a `.contains(…)` on it is
 * the idiom rather than the defect. `functions(&text)` also reaches a view —
 * that is how it is recognised as a normalizer at all — but what it hands back
 * are the ORIGINAL bytes of each function body, and a grep of those is exactly
 * the class this file exists to close. A lexical reader cannot know a return
 * type, but it can read whether the body is a view applied and returned.
 */
function discoverViewAliases(functions, seeds) {
  const aliases = new Set(seeds);
  // `?` and the ownership shims are how the same value is handed on, so they
  // do not make the result something other than the view.
  const passthrough = /^(?:\?|\.\s*(?:to_owned|to_string|into|clone|as_str)\s*\(\s*\))*$/;
  for (let pass = 0; pass < 8; pass += 1) {
    let grew = false;
    for (const fn of functions) {
      if (aliases.has(fn.name)) continue;
      // The body without its braces: a `fn` that returns a view spells that
      // view as its whole tail expression.
      const inner = fn.body.slice(1, -1).trim();
      for (const known of aliases) {
        const head = new RegExp(`^(?:${IDENT}\\s*::\\s*)*${known}\\s*\\(`).exec(inner);
        if (!head) continue;
        const close = parenEnd(inner, head[0].length - 1);
        if (passthrough.test(inner.slice(close).trim())) {
          aliases.add(fn.name);
          grew = true;
        }
        break;
      }
    }
    if (!grew) break;
  }
  return aliases;
}

/**
 * The names of `fn`s that enumerate a directory, closed over their wrappers.
 *
 * A path assembled by a walk is spelled by no literal on the way to the read —
 * `for file in files` names only the vector — so following literals, which is
 * all a lexical reader can do, walks straight past it. The JavaScript half of
 * this ratchet went blind to four readers for exactly that reason
 * (card_2a23d37a583c); recognising the shape here is the same fix.
 */
function discoverWalkers(functions) {
  const walkers = new Set();
  for (let pass = 0; pass < 4; pass += 1) {
    let grew = false;
    for (const fn of functions) {
      if (walkers.has(fn.name)) continue;
      const wrapped = [...walkers].some((name) => new RegExp(`\\b${name}\\s*\\(`).test(fn.body));
      const reaches = enumeratesRust(fn.body, fn.bodyText) || (wrapped && NAMES_RUST.test(fn.bodyText));
      if (!reaches) continue;
      walkers.add(fn.name);
      grew = true;
    }
    if (!grew) break;
  }
  return walkers;
}

/** Names of calls whose `(` is still open at `index`, innermost last. */
function enclosingCalls(code, index) {
  const open = [];
  const stack = [];
  for (let i = 0; i < index; i += 1) {
    const ch = code[i];
    if (ch === '(') {
      const before = code.slice(Math.max(0, i - 200), i);
      const name = new RegExp(`(${IDENT})\\s*$`).exec(before);
      stack.push(name ? name[1] : null);
    } else if (ch === ')') stack.pop();
  }
  for (const name of stack) if (name) open.push(name);
  return open;
}

/**
 * Bindings in `scope` that hold the bytes of a `.rs` file.
 *
 * Two shapes, because the path can be written in two places. `include_str!`
 * carries the literal itself. A `read_to_string` is fed by a walk that names
 * the extension somewhere else in the same function, so the function is what
 * has to name Rust — a bare `read_to_string(path)` in a function that mentions
 * no `.rs` and calls no `.rs` walker is a read of something else.
 */
function rawReads(scope, text, walkers, normalizers, moduleLevelOnly, producers = new Map(), fileNamesRust = false) {
  const reads = [];

  // Bytes a *helper* hands back. The dominant shape in this tree is a function
  // that reads and a different function that greps — `workspace_sources()`
  // collects `(path, text)` pairs, `production_source()` returns one file — so
  // a reader with no cross-function step is decorative on exactly the guards it
  // names. Five of these were mutated back to raw text and only one reddened
  // before this existed.
  if (!moduleLevelOnly) {
    for (const [producer, slot] of producers) {
      const bound = new RegExp(
        `\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*(?:${IDENT}\\s*::\\s*)*${producer}\\s*\\(`,
        'g',
      );
      for (let m = bound.exec(scope); m !== null; m = bound.exec(scope)) {
        const statement = scope.indexOf(';', m.index);
        reads.push({
          name: m[1],
          at: m.index,
          declaredAt: m.index,
          end: statement < 0 ? m.index + m[0].length : statement + 1,
          laundered: false,
        });
      }
      // `for (path, source) in workspace_sources()` binds through a pattern,
      // which is how the corpus walks are consumed.
      const destructured = new RegExp(
        `\\bfor\\s+([^\\n]*?)\\s+in\\s+(?:&\\s*)?(?:${IDENT}\\s*::\\s*)*${producer}\\s*\\(`,
        'g',
      );
      for (let m = destructured.exec(scope); m !== null; m = destructured.exec(scope)) {
        const names = (m[1].match(new RegExp(IDENT, 'g')) ?? []).filter(
          (name) => name !== 'mut' && name !== 'ref',
        );
        const bytes = slot === null ? names : names.slice(slot, slot + 1);
        for (const name of bytes) {
          reads.push({
            name,
            at: m.index,
            declaredAt: m.index,
            end: m.index + m[0].length,
            laundered: false,
          });
        }
      }
    }
  }

  // A census that spells its own `read_dir` loop instead of calling a helper
  // walks Rust just as plainly, and a walker set keyed on NAMES cannot say so:
  // the function IS the walker, so it calls none. `gitea_actions`'s pipeline
  // event census is written that way, and the read at the bottom of its loop
  // was not merely unreported — it was never recognised, so it did not even
  // hold up the floor. Asked of a function body only: at module level the
  // scope is the whole file, and one walk anywhere would taint every read.
  const walksRust =
    [...walkers].some((name) => new RegExp(`\\b${name}\\s*\\(`).test(scope)) ||
    (!moduleLevelOnly && enumeratesRust(scope, text));
  // The module path between `=` and the read is part of the binding, not of a
  // wrapping call: `let text = std::fs::read_to_string(file)` binds `text`, and
  // a reader that insisted the read follow the `=` directly read it as unbound
  // and then had nothing to follow.
  const binding = new RegExp(
    `\\b(let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*[&*\\s]*(?:${IDENT}\\s*::\\s*)*$`,
  );

  // The unit is the READ, not the binding. `include_str!("x.rs").contains(…)`
  // binds nothing and is the exact spelling one recurring gotcha carries five
  // times over (card_653892ab454b); keying on `let` would walk straight past it.
  const call = /\binclude_str\s*!|\bread_to_string\s*\(/g;
  for (let m = call.exec(scope); m !== null; m = call.exec(scope)) {
    // The argument as the file spells it — the code-only view has already
    // blanked the path, so the literal is read out of the byte-aligned twin.
    const window = text.slice(m.index, m.index + 400);
    const close = window.indexOf(')');
    const argument = close < 0 ? window : window.slice(0, close + 1);
    // `read_to_string` is fed by a path this reader cannot follow, so the read
    // or its scope has to name Rust: a literal on the call itself, or a walker
    // that filters on `.rs`. A bare `read_to_string(path)` in a function naming
    // no `.rs` is a read of something else — `/proc`, a README, a fixture.
    const namesRust = /\.rs["']/.test(argument);
    // A path the read does not spell. `module_source` reads
    // `src_root().join(module)` — the `.rs` lives in a table of module names
    // three hundred lines away — so a reader that insists on a literal at the
    // call site sees nothing, and everything downstream of it stays invisible.
    // What says these bytes are Rust is the corpus itself: `fileNamesRust` is
    // set only for a function whose result some caller hands to a named view.
    // The converse is what keeps `/proc/<pid>/stat` and `load_config_file` out:
    // a read that DOES spell its path is taken at its word, and nothing feeds a
    // TOML error string to a Rust view.
    const opaquePath = !/["'][^"'\n]*["']/.test(argument);
    if (
      m[0].startsWith('include_str')
        ? !namesRust
        : !(namesRust || walksRust || (opaquePath && fileNamesRust))
    ) {
      continue;
    }

    // Laundered at birth: `production_rust_code_only(include_str!("x.rs"))`
    // never lets raw bytes out. Such a read is still COUNTED — the floor asks
    // whether this reader can still see the corpus it guards, and a corpus that
    // is entirely well behaved is the state it defends, not evidence of
    // blindness.
    const laundered = enclosingCalls(scope, m.index).some((name) => normalizers.has(name));

    const head = scope.slice(Math.max(0, m.index - 200), m.index);
    const bound = binding.exec(head);
    // A read the binding does not sit directly on because a TUPLE stands in
    // between: `const ALIAS: (&str, &str) = ("crates/…/cli.rs", include_str!(…))`
    // pins the file by path and by bytes at once, and the anchor the binding
    // regex needs is broken by the opening parenthesis. The slot travels with
    // the read, because slot 0 there is a path and not bytes.
    const holder = bound
      ? {
          kind: bound[1],
          name: bound[2],
          declaredAt: m.index - head.length + bound.index,
          slot: null,
        }
      : tupleBinding(scope, m.index, moduleLevelOnly ? 'const|static' : 'let|const|static');
    // A module-level scope owns only `const` / `static`: a `let` belongs to the
    // function that declares it, and resolving it file-wide is how one guard's
    // `text` came to answer for another guard's four hundred lines away.
    if (moduleLevelOnly && (holder === null || holder.kind === 'let')) continue;

    const statement = scope.indexOf(';', m.index);
    reads.push({
      name: holder ? holder.name : null,
      at: m.index,
      declaredAt: holder ? holder.declaredAt : m.index,
      end: statement < 0 ? m.index + m[0].length : statement + 1,
      laundered,
      slot: holder ? holder.slot : null,
    });
  }
  return reads;
}

/**
 * The first method called on `after`, reached across plain field access.
 *
 * `function.body.lines()` is a grep of `body`, and a matcher that demanded the
 * method sit directly on the name read it as an access of `body` and stopped.
 * The chain is lazy, so `text.trim().contains(…)` still answers `trim` — the
 * first call is the one that decides what the rest sees.
 */
const METHOD_AFTER = new RegExp(`^(?:\\s*\\.\\s*${IDENT})*?\\s*\\.\\s*(${IDENT})\\s*\\(`);

/** The index just past the `;` ending the statement that contains `at`. */
function statementEnd(code, at) {
  const semi = code.indexOf(';', at);
  return semi < 0 ? code.length : semi + 1;
}

/**
 * Whether the binding takes the call's own return value rather than a
 * transform of it.
 *
 * The shims that hand the same value on (`?`, `unwrap`, `expect`, the ownership
 * conversions) keep it; iteration keeps its elements, which is what a `for`
 * binds. Anything else — `map`, `find`, `filter`, `collect` — produces a
 * different value, and this reader abstains there.
 */
function returnsValueOf(tail, kind) {
  const iteration = String.raw`\.\s*(?:iter|into_iter)\s*\(\s*\)`;
  if (kind === 'for') return new RegExp(`^\\s*(?:${iteration}|${SHIM})*\\s*\\{`).test(tail);
  return new RegExp(`^\\s*(?:${SHIM})*\\s*;?\\s*$`).test(tail);
}

/**
 * The bytes under a second name.
 *
 * `let production = source.to_owned();` derives nothing — nothing was asked
 * about the bytes, they were merely handed on — so what it binds IS the read,
 * and the grep that follows is the same defect one rename along. This is
 * exactly the vocabulary `returnsValueOf` already treats as value-preserving,
 * asked of a binding instead of of a tail; anything outside it (`map`, `find`,
 * even `trim`) makes a different value and is not followed.
 */
function shimReads(scope, read) {
  if (read.name === null) return [];
  const out = [];
  const shape = new RegExp(
    `\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*[&*\\s]*` +
      `(?<![.\\w])${read.name}\\b(?:\\s*(?:${SHIM}))*\\s*;`,
    'g',
  );
  for (let m = shape.exec(scope); m !== null; m = shape.exec(scope)) {
    out.push({
      name: m[1],
      aliasOf: read.name,
      at: m.index,
      declaredAt: m.index,
      end: m.index + m[0].length,
      laundered: false,
      slot: null,
      scope: read.scope,
      scopeFrom: read.scopeFrom,
    });
  }
  return out;
}

/**
 * What a consumer takes out of a tuple `const`, one slot at a time.
 *
 * `let (name, source) = ALIAS;` is how the byte-carrying half is reached, and
 * the slot recorded on the read is what says `source` is bytes while `name` is
 * a path. Guessing instead is the false positive `tupleSlot` was written to
 * prevent, one construct over.
 *
 * The binding is resolved inside the SMALLEST function that spells the pattern
 * rather than across the module scope the `const` owns. A module-level read's
 * scope is the whole file, and `source` is a name half the file uses — resolving
 * it file-wide is the collision trap this reader has already paid for once
 * (sol_706e02368e50).
 */
function tupleReads(scope, read, functions) {
  if (read.name === null || read.slot === null || read.slot === undefined) return [];
  const out = [];
  const destructure = new RegExp(
    `\\b(?:let|const|static)\\s+\\(([^()]*)\\)\\s*(?::[^=;{}]*)?=\\s*[&*\\s]*` +
      `(?:${IDENT}\\s*::\\s*)*(?<![.\\w])${read.name}\\b\\s*;`,
    'g',
  );
  for (let m = destructure.exec(scope); m !== null; m = destructure.exec(scope)) {
    const names = (m[1].match(new RegExp(IDENT, 'g')) ?? []).filter(
      (name) => name !== 'mut' && name !== 'ref',
    );
    const name = names[read.slot];
    if (name === undefined || name === '_') continue;
    const owner = functions
      .filter((fn) => m.index > fn.open && m.index < fn.end)
      .sort((a, b) => b.open - a.open)[0];
    const from = owner ? owner.open : 0;
    out.push({
      name,
      tupleOf: read.name,
      slotOf: read.slot,
      at: m.index - from,
      declaredAt: m.index - from,
      end: m.index + m[0].length - from,
      laundered: false,
      slot: null,
      scope: owner ? scope.slice(owner.open, owner.end) : undefined,
      scopeFrom: from,
    });
  }
  return out;
}

/**
 * Bindings that hold what a derived view HANDED BACK about these bytes.
 *
 * One hop, and only off a normalizer that is not a view alias: `for function in
 * functions(&text)` binds the original bytes of each function body under a new
 * name, and everything the guard actually greps lives there. A view alias is
 * skipped because its result is the cleaned view, which is the idiom this file
 * refuses to report. A second hop is not taken — it would tail the whole
 * program off one read, and a lexical reader has no way to stop.
 */
function derivedReads(scope, read, normalizers, viewAliases) {
  if (read.name === null) return [];
  const out = [];
  const mentions = new RegExp(`(?<![.\\w])${read.name}\\b`);
  const call = `(?:${IDENT}\\s*::\\s*)*(${IDENT})\\s*\\(`;
  const shapes = [
    { kind: 'let', re: new RegExp(`\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*[&*\\s]*${call}`, 'g') },
    { kind: 'for', re: new RegExp(`\\bfor\\s+([^\\n]*?)\\s+in\\s+(?:&\\s*)?${call}`, 'g') },
  ];
  for (const shape of shapes) {
    for (let m = shape.re.exec(scope); m !== null; m = shape.re.exec(scope)) {
      const view = m[2];
      if (!normalizers.has(view) || viewAliases.has(view)) continue;
      const open = m.index + m[0].length - 1;
      const close = parenEnd(scope, open);
      // The bytes have to be what the view was asked about. A view called on
      // something else in the same scope binds a name this read knows nothing
      // of.
      if (!mentions.test(scope.slice(open, close))) continue;
      // The binding has to BE what the view returned. A chain past the call
      // makes it something else, and something else is not this reader's to
      // answer for: `functions(source).into_iter().map(|f| f.name).collect()`
      // binds a set of NAMES, and `production_function_call_sites(…).find(…)`
      // binds a `CallSite` of offsets. Both were reported as raw greps the
      // first time this hop was taken, and both are honest code — a ratchet
      // that accuses them is a ratchet somebody turns off.
      const tail = scope.slice(close, shape.kind === 'let' ? statementEnd(scope, close) : close + 200);
      if (!returnsValueOf(tail, shape.kind)) continue;
      // A pattern binding more than one name cannot say which half carries the
      // bytes — the same question `tupleSlot` answers for a producer, and the
      // same false positive (`path.starts_with(…)`) if it is guessed.
      const names = (m[1].match(new RegExp(IDENT, 'g')) ?? []).filter(
        (name) => name !== 'mut' && name !== 'ref',
      );
      if (names.length !== 1) continue;
      const semi = scope.indexOf(';', close);
      out.push({
        name: names[0],
        via: view,
        at: m.index,
        declaredAt: m.index,
        end: shape.kind === 'let' && semi >= 0 ? semi + 1 : close,
        laundered: false,
      });
    }
  }
  return out;
}

/**
 * What `scope` does with the bytes bound to `name`, as a list of problems.
 *
 * Every mention is one of three things: laundered (an argument of a normalizer
 * call), asserted over (a text method, or an argument of a `fn` of this corpus
 * that is not a normalizer), or neither — carried into a message, dropped into
 * `let _ =`, handed to something outside the corpus. Only the second is
 * reported: this reader errs toward silence on shapes it has not been taught,
 * because a ratchet nobody can keep green is a ratchet somebody deletes.
 */
function usesOf(scope, read, normalizers, corpusFunctions) {
  const problems = [];
  // An unbound read is used where it is written, so the read itself is the only
  // site there is: `include_str!("x.rs").contains(…)` never names anything.
  const mention = read.name === null ? null : new RegExp(`(?<![.\\w])${read.name}\\b`, 'g');
  if (mention === null) {
    const close = scope.indexOf(')', read.at);
    const at = close < 0 ? read.end : close + 1;
    const method = METHOD_AFTER.exec(scope.slice(at));
    if (method && ASSERTIONS.includes(method[1])) {
      problems.push({ how: `\`.${method[1]}(\` straight off the bytes`, at: read.at });
      return problems;
    }
    const handedTo = enclosingCalls(scope, read.at).filter(
      (name) => corpusFunctions.has(name) && !normalizers.has(name),
    );
    if (handedTo.length > 0) {
      problems.push({
        how: `handed to \`${handedTo[handedTo.length - 1]}\``,
        at: read.at,
      });
    }
    return problems;
  }
  const sites = [];
  for (let m = mention.exec(scope); m !== null; m = mention.exec(scope)) {
    if (m.index >= read.declaredAt && m.index < read.end) continue;
    sites.push(m.index);
  }

  // Reaching a normalizer once launders the binding, not just that mention.
  // The byte-aligned two-view idiom is written exactly that way — the decision
  // is taken on `production_rust_code_only(text)` and the ORIGINAL line is
  // zipped alongside so the diagnostic quotes the file as written. Reporting
  // the second half would demand that guards stop quoting themselves.
  for (const at of sites) {
    const enclosing = enclosingCalls(scope.slice(0, at + read.name.length), at);
    if (enclosing.some((name) => normalizers.has(name))) return problems;
  }

  for (const at of sites) {
    let after = scope.slice(at + read.name.length);
    // A tuple `const` is read one slot at a time, and only the slot the read
    // sits in carries bytes: `ALIAS.0` is the PATH this guard quotes in its own
    // failure message, and accusing it of a raw source assertion is the exact
    // false positive `tupleSlot` exists to prevent one construct over.
    if (read.slot !== null && read.slot !== undefined) {
      const field = /^\s*\.\s*(\d+)/.exec(after);
      if (field) {
        if (Number(field[1]) !== read.slot) continue;
        after = after.slice(field[0].length);
      }
    }
    const method = METHOD_AFTER.exec(after);
    if (method && ASSERTIONS.includes(method[1])) {
      problems.push({ how: `\`.${method[1]}(\` straight off the bytes`, at });
      continue;
    }

    const enclosing = enclosingCalls(scope.slice(0, at + read.name.length), at);
    const handedTo = enclosing.filter(
      (name) => corpusFunctions.has(name) && !normalizers.has(name),
    );
    if (handedTo.length > 0) {
      problems.push({ how: `handed to \`${handedTo[handedTo.length - 1]}\``, at });
    }
  }
  return problems;
}

/** The 1-based line `at` falls on. */
function lineOf(text, at) {
  return text.slice(0, at).split('\n').length;
}

const subjects = [];
for (const dir of SUBJECT_DIRS) {
  for (const file of rustFiles(join(root, dir))) {
    const raw = readFileSync(file, 'utf8');
    subjects.push({
      file: relative(root, file).split('\\').join('/'),
      code: testInclusiveRustCode(raw),
      text: testInclusiveRustSource(raw),
    });
  }
}

const support = join(root, 'tests/support/rust_source.rs');
let hasSupport = false;
try {
  hasSupport = statSync(support).isFile();
} catch {
  hasSupport = false;
}
if (!hasSupport) {
  console.error(
    `❌ rust source views: ${relative(root, support)} is missing — the named views cannot be ` +
      'seeded, so every read would look laundered and this check would pass over anything.',
  );
  process.exit(1);
}

/**
 * A file whose functions other files call.
 *
 * Structural rather than a list of names: everything under `tests/support/` is
 * `include!`d into the crates that need it, and a `common/` module is what an
 * integration test tree imports its shared readers from. Everything else is its own
 * scope — closing the normalizer set over all 700-odd files instead let a `fn`
 * named `new` in one crate launder bytes in another, and the set grew to five
 * thousand names, which is the same collision trap the JavaScript half of this
 * ratchet paid for one level down (sol_706e02368e50).
 */
function isShared(file) {
  return file.startsWith('tests/support/') || file.includes('/common/');
}

const declared = subjects.map((subject) => ({
  file: subject.file,
  functions: rustFunctions(subject.code).map((fn) => ({
    ...fn,
    body: subject.code.slice(fn.open, fn.end),
    bodyText: subject.text.slice(fn.open, fn.end),
  })),
}));

const shared = declared.filter((entry) => isShared(entry.file)).flatMap((entry) => entry.functions);
if (shared.length === 0) {
  console.error(
    '❌ rust source views: no shared reader module was found under tests/support/ or a common/ ' +
      'directory — the normalizer set cannot be derived, so every read would look raw.',
  );
  process.exit(1);
}

const seeds = [...VIEW_SEEDS.production, ...VIEW_SEEDS.testInclusive];
const sharedNormalizers = discoverNormalizers(shared, seeds);
const sharedViewAliases = discoverViewAliases(shared, seeds);
const sharedNames = new Set(shared.map((fn) => fn.name));
const walkers = discoverWalkers(shared);

/**
 * Functions that hand raw `.rs` bytes back to their caller.
 *
 * A function carrying an unlaundered read is one: `workspace_sources()` returns
 * `(path, text)` pairs it read itself, `production_source()` returns the file it
 * included. Deliberately not closed over transitively — one hop covers every
 * shape this tree writes, and each extra hop widens the taint faster than it
 * finds anything, which is how a lexical reader starts reporting its own noise.
 */
function discoverProducers(entries, normalizers, walkers) {
  const producers = new Map();
  for (const entry of entries) {
    for (const fn of entry.functions) {
      if (normalizers.has(fn.name) || producers.has(fn.name)) continue;
      const read = rawReads(
        fn.body,
        fn.bodyText,
        walkers,
        normalizers,
        false,
        new Map(),
        entry.feedsAView(fn.name),
      ).find((candidate) => !candidate.laundered);
      if (read) producers.set(fn.name, tupleSlot(fn.body, read.at, read.name));
    }
  }
  return producers;
}

/**
 * Which element of a returned tuple carries the read, or `null` for a bare
 * value.
 *
 * `workspace_sources()` hands back `(path, text)` pairs and only the second
 * half is bytes, so tainting the whole pattern accused `path.starts_with(…)` —
 * a string operation on a directory name — of being a raw source assertion.
 * Read off the producer's own body rather than off a naming convention: the
 * caller may spell the pattern however it likes, but the position the read sits
 * in is a fact about the producer.
 */
function tupleSlot(body, at, boundName) {
  const direct = tupleSlotAt(body, at);
  if (direct !== null) return direct;
  // One hop, because the read is as often bound first and put in the tuple a
  // line later: `let text = read_to_string(file)?; (file, text)`. Without it
  // the whole pattern is tainted again and `path.starts_with(…)` is accused.
  if (!boundName) return null;
  const tuple = new RegExp(`\\(([^()]*\\b${boundName}\\b[^()]*)\\)`).exec(body);
  if (!tuple) return null;
  const elements = tuple[1].split(',');
  if (elements.length < 2) return null;
  const slot = elements.findIndex((element) => new RegExp(`\\b${boundName}\\b`).test(element));
  return slot < 0 ? null : slot;
}

/** The tuple slot the expression at `at` sits in directly, or `null`. */
function tupleSlotAt(body, at) {
  const open = enclosingTupleOpen(body, at);
  return open < 0 ? null : tupleSlotFrom(body, open, at);
}

/**
 * The `(` of the tuple that directly contains `at`, or `-1`.
 *
 * Walking backwards is what makes it a fact about the expression rather than
 * about the shape of the statement around it — a producer's `(file, text)` and
 * a `const`'s `("crates/…/cli.rs", include_str!(…))` are the same construct
 * read the same way. A `(` preceded by an identifier opens an argument list,
 * not a tuple, and that one distinction is the whole guard against reading
 * every call's argument position as a slot.
 */
function enclosingTupleOpen(body, at) {
  let depth = 0;
  let open = -1;
  for (let i = at - 1; i >= 0; i -= 1) {
    if (body[i] === ')') depth += 1;
    else if (body[i] === '(') {
      if (depth === 0) {
        open = i;
        break;
      }
      depth -= 1;
    }
  }
  if (open < 0) return -1;
  if (new RegExp(`${IDENT}\\s*!?\\s*$`).test(body.slice(Math.max(0, open - 80), open))) return -1;
  return open;
}

/** Which comma-separated slot of the group opening at `open` holds `at`. */
function tupleSlotFrom(body, open, at) {
  let slot = 0;
  let nesting = 0;
  for (let i = open + 1; i < at; i += 1) {
    const ch = body[i];
    if (ch === '(' || ch === '[' || ch === '{') nesting += 1;
    else if (ch === ')' || ch === ']' || ch === '}') nesting -= 1;
    else if (ch === ',' && nesting === 0) slot += 1;
  }
  return slot;
}

/**
 * The `let` / `const` / `static` a read sits under when it is spelled inside a
 * TUPLE, with the slot it occupies.
 *
 * `const ALIAS: (&str, &str) = ("crates/rg-cli/src/cli.rs", include_str!(…));`
 * is how this tree pins a file it reads by path: the path and the bytes travel
 * together so the diagnostic can name the file. The binding regex asks the read
 * to follow the `=` directly, so the tuple's opening parenthesis walked it past
 * the read entirely — the read was not merely unreported, it was never counted,
 * so the floor did not hold it either (card_7c2d24ce98b3).
 *
 * The slot is the same answer `tupleSlot` gives for a producer, and it is
 * needed for the same reason: slot 0 is a PATH, and tainting the whole
 * declaration accuses `ALIAS.0` — a string operation on a file name — of being
 * a raw source assertion. A group with no top-level comma is a parenthesised
 * expression rather than a tuple, so it binds with no slot at all.
 */
function tupleBinding(scope, at, kinds) {
  const open = enclosingTupleOpen(scope, at);
  if (open < 0) return null;
  const head = scope.slice(Math.max(0, open - 200), open);
  const declared = new RegExp(`\\b(${kinds})\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*$`).exec(head);
  if (!declared) return null;
  const close = parenEnd(scope, open);
  const slots = tupleSlotFrom(scope, open, close - 1);
  return {
    kind: declared[1],
    name: declared[2],
    declaredAt: open - head.length + declared.index,
    slot: slots > 0 ? tupleSlotFrom(scope, open, at) : null,
  };
}

const failures = [];
let guardedReads = 0;
let normalizerNames = new Set(sharedNormalizers);

for (const subject of subjects) {
  const own = declared.find((entry) => entry.file === subject.file).functions;
  // The file's own helpers are closed over on top of the shared set, and a
  // file's own walkers count too — `workspace_sources` is declared beside the
  // guard that uses it, not in a common module.
  const normalizers = discoverNormalizers(own, sharedNormalizers);
  const viewAliases = discoverViewAliases(own, sharedViewAliases);
  const fileWalkers = discoverWalkers([...own, ...shared]);
  const corpusFunctions = new Set([...own.map((fn) => fn.name), ...sharedNames]);
  // A helper whose result some caller hands straight to a named view is
  // producing Rust, whatever its path expression looks like. This is the
  // corpus declaring what the bytes are, which is the only thing that can
  // answer for a read whose path is assembled out of a table three hundred
  // lines away — and it is silent about a helper nobody feeds to a view, which
  // is what keeps every TOML and YAML reader in this workspace out.
  const feedsAView = (name) =>
    [...normalizers].some((view) =>
      new RegExp(`\\b${view}\\s*\\(\\s*&?\\s*(?:${IDENT}\\s*::\\s*)*${name}\\s*\\(`).test(subject.code),
    );
  const producers = discoverProducers(
    [
      { functions: own, feedsAView },
      { functions: shared, feedsAView: () => false },
    ],
    normalizers,
    fileWalkers,
  );
  for (const name of normalizers) normalizerNames.add(name);

  const functions = rustFunctions(subject.code);
  // A binding belongs to the SMALLEST function that contains it. Resolving a
  // name file-wide instead is what made this reader answer about `text` in one
  // guard using the `text` another guard binds four hundred lines away — the
  // collision trap the JavaScript half of this ratchet paid for
  // (sol_706e02368e50).
  const inner = (at) => functions.some((fn) => at > fn.open && at < fn.end);

  const scopes = functions.map((fn) => ({
    from: fn.open,
    code: subject.code.slice(fn.open, fn.end),
    text: subject.text.slice(fn.open, fn.end),
    moduleLevelOnly: false,
    // The scope IS a function, so a binding taken out of a tuple inside it
    // needs no further narrowing.
    functions: [],
    // A read nested one function deeper is that function's, not this one's.
    owns: (at) =>
      !functions.some(
        (other) =>
          other.open > fn.open && other.end <= fn.end && at > other.open && at < other.end,
      ),
  }));
  // A module-level `const` is read where it is used, which is another function
  // entirely, so the whole file is its scope.
  scopes.push({
    from: 0,
    code: subject.code,
    text: subject.text,
    moduleLevelOnly: true,
    // The whole file, so a consumer that destructures the `const` has to be
    // resolved back down to the function that spells the pattern.
    functions,
    owns: (at) => !inner(at),
  });

  for (const scope of scopes) {
    for (const read of rawReads(
      scope.code,
      scope.text,
      fileWalkers,
      normalizers,
      scope.moduleLevelOnly,
      producers,
      false,
    )) {
      if (!scope.owns(scope.from + read.declaredAt)) continue;
      guardedReads += 1;
      if (read.laundered) continue;
      // The read itself, and then one derivation: what a non-alias view handed
      // back about these bytes is asked the same question the bytes were.
      // `text` reaching `functions(&text)` is laundered and stays laundered —
      // the byte-aligned two-view idiom depends on it — but the `function` it
      // binds carries the original bytes of a body, and that is where the
      // grep this file is about has been hiding.
      // …and what a consumer takes out of a tuple `const`, which is the only
      // way the byte-carrying slot is ever reached: `let (name, source) = ALIAS`
      // hands the bytes on under a name of the caller's choosing.
      const derived = [
        read,
        ...derivedReads(scope.code, read, normalizers, viewAliases),
        ...tupleReads(scope.code, read, scope.functions),
      ];
      // A rename is not a derivation. `let production = source.to_owned();`
      // asks nothing about the bytes, so whatever it binds is still the read —
      // and a reader that stopped at the rename let the grep one line later go
      // unanswered.
      const followed = derived.flatMap((step) => [
        step,
        ...shimReads(step.scope ?? scope.code, step),
      ]);
      for (const step of followed) {
        const code = step.scope ?? scope.code;
        const from = step.scopeFrom ?? 0;
        for (const problem of usesOf(code, step, normalizers, corpusFunctions)) {
          let held = 'holds the bytes of a `.rs` file and is';
          if (step.via !== undefined) {
            held = `holds what \`${step.via}\` handed back about the bytes of a \`.rs\` file and is`;
          } else if (step.tupleOf !== undefined) {
            held = `holds slot ${step.slotOf} of \`${step.tupleOf}\`, the bytes of a \`.rs\` file, and is`;
          } else if (step.aliasOf !== undefined) {
            held = `holds the same \`.rs\` bytes as \`${step.aliasOf}\` and is`;
          }
          failures.push(
            `${subject.file}:${lineOf(subject.code, scope.from + from + problem.at)} — ` +
              `${step.name === null ? 'the bytes of a `.rs` file are' : `\`${step.name}\` ${held}`} ` +
              `${problem.how}`,
          );
        }
      }
    }
  }
}

if (failures.length > 0) {
  console.error('❌ Rust source guards assert over raw bytes:');
  for (const failure of [...new Set(failures)].sort()) console.error(`   - ${failure}`);
  console.error(
    '\n   Read the file through a named view of `tests/support/rust_source.rs` first:\n' +
      '   `production_rust_code_only` / `production_rust_source` for a sweep that requires a\n' +
      '   construct to be present, `rust_code_only` for one that only reports what it finds.\n' +
      '   A raw grep is satisfied by a commented-out construct, a `#[cfg(test)]` fixture and a\n' +
      '   string literal alike.',
  );
  process.exit(1);
}

if (guardedReads < minReads) {
  console.error(
    `❌ rust source views: only ${guardedReads} read(s) of a \`.rs\` file were recognised across ` +
      `${subjects.length} file(s) (expected at least ${minReads}).\n` +
      '   The reader has gone blind to the corpus it guards — fix the read recognition in\n' +
      '   scripts/rust-source-view-contract-check.mjs rather than lowering the floor.',
  );
  process.exit(1);
}

console.log(
  `rust source views: ${guardedReads} read(s) of a \`.rs\` file across ${subjects.length} source ` +
    `file(s), every one of them reaching its assertion through one of ${normalizerNames.size} named ` +
    'or derived views',
);
