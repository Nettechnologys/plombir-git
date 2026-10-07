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
//   - a *normalizer* is discovered, not listed: seeded with the CODE views of
//     `tests/support/rust_source.rs`, then closed over every `fn` whose own
//     body calls one. `call_site_contains` qualifies because it calls
//     `production_rust_code_only`; `signed_limit_field_lines` because it calls
//     it on the way to its inner reader. A hand-written list of laundering
//     helpers would be the defect this file exists to close, one level up. The
//     closure reads a `::` qualifier: a capital one is a TYPE, so `Vec::new()`
//     is not a call to a `fn new` this workspace declares — one such `new` in a
//     `common/` module had otherwise made a normalizer of every builder in the
//     corpus, and a mutated `serde_fields` stayed laundered through it. A type
//     the FILE implements is the exception, and it is read off the PAIR rather
//     than off the name: `ResolvingSource::new(…)` applies both views in its
//     own body, so a helper calling it launders, while `ActionTemplate::new(…)`
//     in the same file shares only the name and launders nothing;
//   - an *assertion* is a text method applied to the bytes (`contains`,
//     `lines`, `find`, `match_indices`, `split*`, `starts_with`, `strip_*`), or
//     the bytes being handed to a `fn` of the corpus that is not a normalizer —
//     the local half matters most, because a guard's own `contract(source)` is
//     where all of its assertions go.
//
// Two axes cross in the seed set, and only one of them used to be read.
//
// The first is test items, the split card_04cdbcb8d553 paid for on the
// JavaScript side. `production_rust_*` blanks complete `#[cfg(test)]` items;
// `rust_code_only` deliberately does not. A sweep that *requires* a construct
// to be present is fooled by a test double declaring it and must use a
// production view. A sweep that *reports* what it finds cannot be — the worst a
// fixture can do there is ask for a look. Both are named views with the rule in
// their docstring, so the test-inclusive half is an intent someone spelled
// rather than a name parked on an exclusion list.
//
// The second is comments and literals, and it decides whether the bytes are
// FINISHED. `production_rust_source` blanks test items and KEEPS comments and
// literals on purpose — a guard decodes a real Rust attribute out of them after
// the code view has bounded it — so what it hands back is still text a comment
// can fool. Reading it as a view alias made `let source = production_source();`
// not a read at all, and seven crates could have dropped the code view their
// consumers apply with nothing objecting (card_2f5905fd48bd). It now launders
// nothing: bytes may be handed INTO it without accusation, what comes out is
// followed, and such a read is judged mention by mention rather than laundered
// wholesale by the first view it reaches — it reached a view already, which is
// what makes it string-bearing, so the wholesale rule would answer the question
// with its own premise.
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
// A producer's collection is reached three ways, and the third took a card of
// its own. `let x = producer()` and `for pat in producer()` both key on the
// CALL, so a consumer that binds the collection under a name on one line and
// takes it apart on a later one — `let sources = production_sources();` …
// `for (_, text) in &sources` — was read by neither, and it fell silent for an
// honest reason: `sources.len()` is not an assertion and a `for` over a name is
// not a call. `crates/rg-mcp/src/lib.rs` is written that way, and with the shape
// unread its whole `const DEFAULT_*` census could be rebuilt on a comment-live
// view without the gate saying a word (card_ecda7101bf7d). The producer's slot
// travels to the element, and the shape is read ONLY when the producer spells
// that slot: without it the reader would have to guess which half of a `(path,
// source)` pair is the program, and `path.starts_with(…)` is the accusation
// `tupleSlot` was written to prevent. Locating the slot takes one relay hop —
// `let text = read_to_string(&path)?; let production = view(&text);
// sources.push((path, production))` names the tuple after the second binding,
// not the read — which is a hop about WHERE the bytes land rather than about
// how far the taint is followed, so erring toward `null` there errs toward
// silence.
//
// What it does NOT see, stated because a ratchet with an unrecorded blind spot
// is how this class survives:
//   - the second derivation — `handlers(source)` then `handler.body[sig..]` is
//     two hops, and one is where this reader stops;
//   - any laundering a guard spells in a shape this reader has not been taught;
//   - a constructor whose `impl` block lives in ANOTHER file: the pair a
//     qualified call is resolved against is read off the file that spells the
//     call, so `Views::new(…)` here and `impl Views` over there stay two facts
//     this reader never joins.
// It errs toward silence on the first two on purpose, because a ratchet nobody
// can keep green is one somebody deletes; the last one errs the other way — an
// unresolved constructor is reported rather than trusted, which is a red a
// person can act on. The floor is what covers the half that silence cannot.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { testInclusiveRustCode, testInclusiveRustSource } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));

// The mutation stand points this at a fixture tree, so it drives the real sweep
// rather than a copy of it.
const override = process.env.PLOMBIR_GIT_RUST_VIEW_ROOT;
const root = override ? resolve(override) : resolve(scriptsDir, '..');

/** Where Rust that reads Rust lives. */
const SUBJECT_DIRS = ['crates', 'tests'];

/**
 * The views of `tests/support/rust_source.rs` a reader may anchor in, split by
 * WHAT EACH ONE BLANKS.
 *
 * Two axes cross here and only one of them used to be read. The first is test
 * items: `production_rust_*` blanks complete `#[cfg(test)]` items, `rust_code_
 * only` deliberately does not, and which one a guard needs follows from whether
 * it requires a construct to be present (a fixture can supply one) or only
 * reports what it finds (the worst a fixture can do is ask for a look).
 *
 * The second axis is comments and literals, and it decides whether the bytes
 * are FINISHED:
 *   - `codeOnly` blanks every comment and every string-like literal, so a
 *     commented-out construct and a call-shaped string contribute nothing. The
 *     conversation ends there. `production_rust_code_with_doc_comments` belongs
 *     here: it blanks every literal and every ordinary comment, and the doc
 *     comments it keeps are the subject a `--help` guard came for — the same
 *     kind of stated intent the test-inclusive half of the first axis is;
 *   - `stringBearing` is `production_rust_source`, which blanks test items and
 *     KEEPS comments and literals on purpose, so a guard can decode a real Rust
 *     attribute after locating its boundary in a code view. What it hands back
 *     is still text a comment can fool, so it launders nothing — it is a
 *     passthrough that answers one question of the two (card_2f5905fd48bd).
 */
const VIEW_SEEDS = {
  codeOnly: [
    'rust_code_only',
    'production_rust_code_only',
    'production_rust_code_with_doc_comments',
  ],
  stringBearing: ['production_rust_source'],
};

const IDENT = '[A-Za-z_][A-Za-z0-9_]*';

/**
 * Where the named views are declared, relative to the root.
 *
 * A seed is a NAME, and a name is the cheapest thing in the world to write. The
 * check trusted it on sight: a `fn production_rust_code_only(text: &str) ->
 * String { text.to_owned() }` declared next to the guard laundered every read
 * it touched, and so did a call site that merely spelled the name with no such
 * function anywhere in the tree. Both are measured — the same body under the
 * name `some_local_helper` went red (card_e0f4ada65cee). So a seed name has to
 * RESOLVE before it launders, and this path is what it resolves to.
 */
const SUPPORT_FILE = 'tests/support/rust_source.rs';

/** Every seed name, whichever axis it seeds. */
const SEED_NAMES = new Set([...VIEW_SEEDS.codeOnly, ...VIEW_SEEDS.stringBearing]);

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
 * Methods that hand the SAME bytes back in a different shape.
 *
 * `trim()` returns a `&str` INTO the bytes it was given: the comments, the
 * `#[cfg(test)]` modules and the string literals are all still in there, so
 * what comes out the far end is the raw file with its ends clipped. Case
 * folding, one substring swapped for another, the pieces welded back into one
 * string — same story, RAW in and RAW out, and not one of them is a view. A
 * chain of them between a read and the grep therefore has to be as invisible to
 * this reader as a pair of parentheses.
 *
 * Left unlisted, one `.trim()` was enough to drop the assertion out of sight
 * while the read still counted towards the floor: the gate went green having
 * read the file and examined none of the claims made about it. Normalizers are
 * deliberately absent — they are named functions discovered from
 * `tests/support/rust_source.rs`, and a laundering step has to stay a
 * laundering step.
 */
const PASSTHROUGH = [
  'trim',
  'trim_start',
  'trim_end',
  'trim_matches',
  'trim_start_matches',
  'trim_end_matches',
  'to_lowercase',
  'to_uppercase',
  'to_ascii_lowercase',
  'to_ascii_uppercase',
  'to_owned',
  'to_string',
  'as_str',
  'as_ref',
  'replace',
  'replacen',
  'concat',
  'repeat',
  // The collection half: a walk binds a `Vec<String>` of files it read, and
  // `.join("\n")` is how a guard spells "all of them at once" before greping
  // the lot.
  'join',
];

/**
 * One PASSTHROUGH call, arguments and all.
 *
 * The argument is matched without nesting on purpose: these methods take a
 * pattern, a separator or nothing at all, and a call this reader cannot delimit
 * is one it declines to follow rather than one it guesses about.
 */
const PASSTHROUGH_CALL = String.raw`\.\s*(?:${PASSTHROUGH.join('|')})\s*\([^()]*\)`;

/**
 * The floor. A reader that has stopped recognising reads reports a clean corpus
 * over files it never understood, and nobody investigates green. The count is a
 * lower bound on the `include_str!("….rs")` and walk-fed reads this tree
 * carries; it is allowed to grow and is not allowed to quietly collapse.
 */
const MIN_READS = 60;
const minReads = override ? Number(process.env.PLOMBIR_GIT_RUST_VIEW_MIN ?? 0) : MIN_READS;

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
 * Whether `body` calls the corpus function `name` — and not a same-named method
 * of a foreign type.
 *
 * `Vec::new()` and `String::new()` are not calls to a `fn new` this workspace
 * declares, but `\bnew\s*\(` says they are, and one `new` in a `common/`
 * module reaching a view laundered every builder in seven hundred files. The
 * qualifier decides it: Rust spells a type in UpperCamelCase and a module in
 * snake_case, and the compiler's own default lints keep that true — so a `::`
 * qualifier starting with a capital is a TYPE and its method is somebody
 * else's, while `rust_source::production_rust_code_only(…)` is the corpus
 * function this reader means.
 *
 * `qualifiers` is what buys back the constructor the rejection also threw away.
 * A type this file IMPLEMENTS is not somebody else's: `ResolvingSource::new(…)`
 * names a `fn new` declared in an `impl ResolvingSource` block of this very
 * file, and that one applies both views to the bytes it is handed. Rejecting it
 * with `Vec::new()` made its caller a producer of raw bytes that launders — a
 * phantom whose consumers could not be followed at all (card_53a95b2b6217).
 * The set is built per `fn` and per name by `qualifiersFor`, so a spelling is
 * read only when the method behind it is declared HERE.
 */
function callsByName(body, name, qualifiers = null) {
  if (new RegExp(`(?<![.\\w])(?<![A-Z][A-Za-z0-9_]*::)${name}\\s*\\(`).test(body)) return true;
  if (qualifiers === null || qualifiers.size === 0) return false;
  return new RegExp(`(?<![.\\w])(?:${[...qualifiers].join('|')})\\s*::\\s*${name}\\s*\\(`).test(body);
}

/** `text` with every balanced `<…>` group removed. */
function stripGenerics(text) {
  let out = '';
  let depth = 0;
  for (const ch of text) {
    if (ch === '<') depth += 1;
    else if (ch === '>') depth = Math.max(0, depth - 1);
    else if (depth === 0) out += ch;
  }
  return out;
}

/**
 * The `impl` blocks `code` declares, with the type each one is about.
 *
 * The self-type is what follows `for` when the block implements a trait and
 * what follows `impl` otherwise; generic parameters and a `where` clause name
 * no type of their own, so both are dropped before the last path segment is
 * read. An `impl` in RETURN position (`-> impl Iterator<…>`) declares no block
 * at all, and is told apart by what precedes it on its line: a declaration
 * opens one, a return type is preceded by the signature it closes.
 */
function implBlocks(code) {
  const out = [];
  const keyword = /\bimpl\b/g;
  for (let m = keyword.exec(code); m !== null; m = keyword.exec(code)) {
    const lineStart = code.lastIndexOf('\n', m.index) + 1;
    if (!/^\s*(?:unsafe\s+)?$/.test(code.slice(lineStart, m.index))) continue;
    const open = code.indexOf('{', m.index);
    if (open < 0) continue;
    const header = stripGenerics(code.slice(m.index + 'impl'.length, open)).replace(
      /\bwhere\b[\s\S]*$/,
      '',
    );
    let target = header;
    const trait = /\bfor\b(?![A-Za-z0-9_])/g;
    for (let f = trait.exec(header); f !== null; f = trait.exec(header)) {
      target = header.slice(f.index + 'for'.length);
    }
    const segments = target.match(new RegExp(IDENT, 'g'));
    if (segments === null) continue;
    const type = segments[segments.length - 1];
    // A lowercase tail is a module path or a primitive, not a type this file
    // implements in the UpperCamelCase the qualifier rejection keys on.
    if (!/^[A-Z]/.test(type)) continue;
    out.push({ type, open, end: blockEnd(code, open) });
  }
  return out;
}

/**
 * Which locally implemented type owns each method name, and what `Self` means
 * inside each `fn`.
 *
 * Positional rather than by name: a `fn` belongs to the innermost `impl` block
 * that contains it, which is the same containment rule the read scopes use.
 */
function implMethods(code, functions) {
  const blocks = implBlocks(code);
  const owners = new Map();
  const implTypes = new Map();
  for (const fn of functions) {
    const block = blocks
      .filter((candidate) => fn.start > candidate.open && fn.start < candidate.end)
      .sort((a, b) => b.open - a.open)[0];
    if (block === undefined) continue;
    implTypes.set(fn.start, block.type);
    if (!owners.has(fn.name)) owners.set(fn.name, new Set());
    owners.get(fn.name).add(block.type);
  }
  return { owners, implTypes };
}

/**
 * The `::` qualifiers under which `fn` may spell a call to the normalizer
 * `name`.
 *
 * Keyed on the PAIR and not on the name, which is what keeps the collision
 * `sol_4d15ddc4996e` paid for shut. `Vec::new()` is out because nothing here
 * implements `Vec`; `ActionTemplate::new(…)` is out too, in a file where
 * `ResolvingSource::new` is the constructor that launders — the two share a
 * name and nothing else, and a name-keyed answer would have laundered both.
 * `qualified` carries the spellings that have been SHOWN to reach a view, so a
 * type-qualified call is read only when the method behind it is the one that
 * does.
 */
function qualifiersFor(fn, name, qualified) {
  const owners = fn.methodOwners === undefined ? undefined : fn.methodOwners.get(name);
  if (owners === undefined) return null;
  const allowed = new Set([...owners].filter((type) => qualified.has(`${type}::${name}`)));
  // Inside `impl T`, `Self` IS `T` — the spelling a constructor is most often
  // written in, and the same method behind it.
  if (fn.implType !== null && allowed.has(fn.implType)) allowed.add('Self');
  return allowed;
}

/**
 * The `fn` names of the corpus whose body reaches one of `seeds`.
 *
 * Closed over transitively, the way the JavaScript ratchet discovers its own:
 * a wrapper around a production view is a production view, and a list of them
 * kept by hand is a list somebody forgets to extend.
 */
function discoverNormalizers(functions, seeds, stringViews = new Set(), qualified = new Set()) {
  const normalizers = new Set(seeds);
  for (let pass = 0; pass < 8; pass += 1) {
    let grew = false;
    for (const fn of functions) {
      // A method is settled only once its `Type::method` spelling is settled
      // too: a second `fn new` on a different type carries the bare name into
      // the set without saying anything about ITS own body, and skipping on
      // the bare name alone would either launder that second constructor or
      // lose the first one, depending on which the loop reached first.
      const spelling = fn.implType === null ? null : `${fn.implType}::${fn.name}`;
      if (normalizers.has(fn.name) && (spelling === null || qualified.has(spelling))) continue;
      // A string-bearing view never launders, however it is spelled inside.
      // `production_rust_source` calls `rust_code_only` to find where the test
      // items END and then hands back the ORIGINAL bytes, so the closure that
      // says "a wrapper around a view is a view" walked it straight back in and
      // undid the split. Laundering is about what a `fn` HANDS BACK; the taint
      // may not propagate through one either, or `production_source()` — the
      // string view applied and returned — comes back as a normalizer one hop
      // later.
      if (stringViews.has(fn.name)) continue;
      for (const known of normalizers) {
        // A seed is a view only in a file that HAS the view. The name alone
        // used to be enough, so a local `fn` spelling it laundered its caller
        // one hop later too (card_e0f4ada65cee).
        if (SEED_NAMES.has(known) && fn.views !== undefined && !fn.views.has(known)) continue;
        if (callsByName(fn.body, known, qualifiersFor(fn, known, qualified))) {
          normalizers.add(fn.name);
          if (spelling !== null) qualified.add(spelling);
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
        if (SEED_NAMES.has(known) && fn.views !== undefined && !fn.views.has(known)) continue;
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
function rawReads(scope, text, walkers, normalizers, moduleLevelOnly, producers = new Map(), fileNamesRust = false, stringViews = new Set()) {
  const reads = [];

  // Bytes a *helper* hands back. The dominant shape in this tree is a function
  // that reads and a different function that greps — `workspace_sources()`
  // collects `(path, text)` pairs, `production_source()` returns one file — so
  // a reader with no cross-function step is decorative on exactly the guards it
  // names. Five of these were mutated back to raw text and only one reddened
  // before this existed.
  if (!moduleLevelOnly) {
    for (const [producer, slot] of producers) {
      // What the producer hands back: raw bytes nothing has looked at, or the
      // string-bearing view, which has answered the `#[cfg(test)]` question and
      // left the comment one open. The difference decides how far this read is
      // followed, not whether it is one.
      const stringBearing = stringViews.has(producer);
      const bound = new RegExp(
        `\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*(?:${IDENT}\\s*::\\s*)*${producer}\\s*\\(`,
        'g',
      );
      const boundNames = [];
      for (let m = bound.exec(scope); m !== null; m = bound.exec(scope)) {
        const statement = scope.indexOf(';', m.index);
        boundNames.push(m[1]);
        reads.push({
          name: m[1],
          at: m.index,
          declaredAt: m.index,
          end: statement < 0 ? m.index + m[0].length : statement + 1,
          laundered: false,
          stringBearing,
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
            stringBearing,
          });
        }
      }
      // The collection bound by a NAME on one line and taken apart on a later
      // one: `let sources = production_sources();` … `for (_, text) in
      // &sources`. Both shapes above key on the CALL, so the tag reached the
      // binding and stopped dead there — and it stopped SILENTLY, because
      // `sources.len()` is not an assertion and `for (_, text) in &sources` is
      // not a call. `crates/rg-mcp/src/lib.rs` is written that way, and with
      // this shape unread the crate's whole `const DEFAULT_*` census could be
      // rebuilt on a comment-live view without the gate saying a word
      // (card_ecda7101bf7d).
      //
      // Only when the producer SPELLS which slot of its element carries the
      // bytes. Without that the reader would have to guess which half of a
      // `(path, source)` pair is the program, and guessing wrong accuses
      // `path.starts_with(…)` — the exact false positive `tupleSlot` exists to
      // prevent. A producer that launders through a constructor of a type its
      // own file implements is no producer at all: the closure reads
      // `ResolvingSource::new(…)` as the call it is, so such a helper is a
      // normalizer and never reaches this loop (card_53a95b2b6217).
      for (const collection of slot === null ? [] : boundNames) {
        const overName = new RegExp(
          `\\bfor\\s+([^\\n]*?)\\s+in\\s+(?:&\\s*(?:mut\\s+)?)?(?<![.\\w])${collection}\\b`
            + `\\s*(?:\\.\\s*(?:iter|iter_mut|into_iter|values|values_mut)\\s*\\(\\s*\\))?\\s*\\{`,
          'g',
        );
        for (let m = overName.exec(scope); m !== null; m = overName.exec(scope)) {
          const names = (m[1].match(new RegExp(IDENT, 'g')) ?? []).filter(
            (name) => name !== 'mut' && name !== 'ref',
          );
          // The slot is picked by POSITION, so `_` has to stay in the list
          // until then; after it, a wildcard names nothing to follow.
          const bytes = names.slice(slot, slot + 1).filter((name) => name !== '_');
          for (const name of bytes) {
            reads.push({
              name,
              at: m.index,
              declaredAt: m.index,
              end: m.index + m[0].length,
              laundered: false,
              stringBearing,
            });
          }
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
 * The chain is lazy, so this answers the FIRST call — which is not always the
 * one that asks a question. Ask `assertionAfter` for that.
 */
const METHOD_AFTER = new RegExp(`^(?:\\s*\\.\\s*${IDENT})*?\\s*\\.\\s*(${IDENT})\\s*\\(`);

/** How many PASSTHROUGH hops are wound past before the chain is given up on. */
const PASSTHROUGH_HOPS = 8;

/**
 * The name of the first method called on `tail` that asks something about the
 * bytes, or `null`.
 *
 * The first call is not the deciding one when it hands the same bytes back:
 * `source.trim().contains(…)` answers `trim`, which is in no assertion list, so
 * the `contains` behind it was never examined and the guard went green having
 * greped the raw file. Every PASSTHROUGH call is wound past — arguments and all,
 * via `parenEnd`, so a separator holding its own parentheses is not mistaken for
 * the end of the chain.
 *
 * A chain longer than `PASSTHROUGH_HOPS`, or one whose parentheses do not close
 * inside `tail`, yields `null`: this reader declines to accuse what it cannot
 * follow rather than guessing at it.
 */
function assertionAfter(tail) {
  let rest = tail;
  for (let hop = 0; hop <= PASSTHROUGH_HOPS; hop += 1) {
    const method = METHOD_AFTER.exec(rest);
    if (method === null) return null;
    if (!PASSTHROUGH.includes(method[1])) return method[1];
    const close = parenEnd(rest, method[0].length - 1);
    if (close >= rest.length) return null;
    rest = rest.slice(close);
  }
  return null;
}

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
 * binds; a PASSTHROUGH transform keeps the bytes and changes only their shape,
 * so `production_rust_source(text).trim()` is still what the view handed back —
 * and a reader that stopped at the shims followed the view without the `.trim()`
 * and abstained with it, which is how a string-bearing read came to be greped
 * with nobody looking. Anything else — `map`, `find`, `filter`, `collect` —
 * produces a different value, and this reader abstains there.
 */
function returnsValueOf(tail, kind) {
  const iteration = String.raw`\.\s*(?:iter|into_iter)\s*\(\s*\)`;
  const keeps = `${iteration}|${SHIM}|${PASSTHROUGH_CALL}`;
  if (kind === 'for') return new RegExp(`^\\s*(?:${keeps})*\\s*\\{`).test(tail);
  return new RegExp(`^\\s*(?:${SHIM}|${PASSTHROUGH_CALL})*\\s*;?\\s*$`).test(tail);
}

/**
 * The bytes under a second name.
 *
 * `let production = source.to_owned();` derives nothing — nothing was asked
 * about the bytes, they were merely handed on — so what it binds IS the read,
 * and the grep that follows is the same defect one rename along. The vocabulary
 * is the shims `returnsValueOf` already treats as value-preserving, asked of a
 * binding instead of of a tail, PLUS the PASSTHROUGH transforms: `trim()` hands
 * back a `&str` into the same bytes, and a reader that stopped at the shims read
 * `let production = source.trim();` as a new value and let every grep of it go
 * unexamined — the chain hole `assertionAfter` closes, spelled as a rename.
 * Anything outside the two — `map`, `find`, `filter`, `collect` — makes a
 * different value and is not followed.
 */
function shimReads(scope, read) {
  if (read.name === null) return [];
  const out = [];
  const shape = new RegExp(
    `\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*[&*\\s]*` +
      `(?<![.\\w])${read.name}\\b(?:\\s*(?:${SHIM}|${PASSTHROUGH_CALL}))*\\s*;`,
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
function derivedReads(scope, read, normalizers, viewAliases, stringViews) {
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
      // A code view ends the conversation, so its result is not followed. Every
      // other view hands back something this reader still has to answer for:
      // a derived normalizer hands back something ABOUT the bytes, and a
      // string-bearing view hands back the bytes themselves with the comments
      // and literals still in them.
      const followable = stringViews.has(view) || (normalizers.has(view) && !viewAliases.has(view));
      if (!followable) continue;
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
        stringBearing: stringViews.has(view),
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
function usesOf(scope, read, normalizers, corpusFunctions, stringViews, impostors = new Set()) {
  const problems = [];
  // A seed name this file cannot resolve to the real view. Saying only "handed
  // to `production_rust_code_only`" sends the reader looking for a bug in the
  // view; the defect is that the name here is somebody else's `fn`.
  const handedHow = (name) =>
    impostors.has(name)
      ? `handed to \`${name}\`, which does not resolve to the view of \`${SUPPORT_FILE}\` here`
      : `handed to \`${name}\``;
  // An unbound read is used where it is written, so the read itself is the only
  // site there is: `include_str!("x.rs").contains(…)` never names anything.
  const mention = read.name === null ? null : new RegExp(`(?<![.\\w])${read.name}\\b`, 'g');
  if (mention === null) {
    const close = scope.indexOf(')', read.at);
    const at = close < 0 ? read.end : close + 1;
    const method = assertionAfter(scope.slice(at));
    if (method !== null && ASSERTIONS.includes(method)) {
      problems.push({ how: `\`.${method}(\` straight off the bytes`, at: read.at });
      return problems;
    }
    const handedTo = enclosingCalls(scope, read.at).filter(
      (name) => corpusFunctions.has(name) && !normalizers.has(name) && !stringViews.has(name),
    );
    if (handedTo.length > 0) {
      problems.push({
        how: handedHow(handedTo[handedTo.length - 1]),
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
  //
  // A STRING-BEARING read is judged mention by mention instead. It reached a
  // view already — that is what makes it string-bearing — so "a code view was
  // applied somewhere in this scope" is true of every one of them by
  // construction, and laundering the binding on it would answer the question
  // with its own premise. What such a guard has to show is that EACH helper it
  // hands the bytes to closes the comment question: `serde_fields` does
  // because it calls `production_rust_code_only`, and the same helper with
  // that call removed is a plain grep of a file where comments are still live.
  if (!read.stringBearing) {
    for (const at of sites) {
      const enclosing = enclosingCalls(scope.slice(0, at + read.name.length), at);
      if (enclosing.some((name) => normalizers.has(name))) return problems;
    }
  }

  for (const at of sites) {
    const enclosing = enclosingCalls(scope.slice(0, at + read.name.length), at);
    if (read.stringBearing && enclosing.some((name) => normalizers.has(name))) continue;
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
    const method = assertionAfter(after);
    if (method !== null && ASSERTIONS.includes(method)) {
      problems.push({ how: `\`.${method}(\` straight off the bytes`, at });
      continue;
    }

    // A string-bearing view is a view: handing bytes into it accuses nobody.
    // It launders nothing either — what it returns is followed by
    // `derivedReads`, which is where the question is actually answered.
    const handedTo = enclosing.filter(
      (name) => corpusFunctions.has(name) && !normalizers.has(name) && !stringViews.has(name),
    );
    if (handedTo.length > 0) {
      problems.push({ how: handedHow(handedTo[handedTo.length - 1]), at });
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

/** The innermost `mod` name containing `at`, or `null` at file level. */
function enclosingModule(code, at) {
  let name = null;
  let innermost = -1;
  const declaration = new RegExp(`\\bmod\\s+(${IDENT})\\s*\\{`, 'g');
  for (let m = declaration.exec(code); m !== null; m = declaration.exec(code)) {
    const open = code.indexOf('{', m.index);
    if (open < 0 || at <= open) continue;
    if (at >= blockEnd(code, open)) continue;
    if (open <= innermost) continue;
    innermost = open;
    name = m[1];
  }
  return name;
}

/**
 * How a file reaches the real views: the `mod`s it `include!`s them into.
 *
 * The path is a string literal, blanked in the code-only view, so the `include!`
 * is found in the code view — which is what keeps a comment quoting the path
 * from conjuring access — and the path itself is read off the byte-aligned twin
 * at the same offsets.
 */
function viewModules(subject) {
  const modules = new Set();
  let direct = false;
  const include = /\binclude\s*!\s*\(/g;
  for (let m = include.exec(subject.code); m !== null; m = include.exec(subject.code)) {
    const open = m.index + m[0].length - 1;
    if (!subject.text.slice(open, parenEnd(subject.code, open)).includes(SUPPORT_FILE)) continue;
    const owner = enclosingModule(subject.code, m.index);
    if (owner === null) direct = true;
    else modules.add(owner);
  }
  return { modules, direct };
}

/**
 * The module a `fn` delegates its own name to, or `null` if it does not.
 *
 * The honest wrapper this tree writes is one line — `rust_source::
 * production_rust_code_only(text)` — and nothing else: the same name, through a
 * path, applied and returned. `common/source_scan.rs` has three of them, and
 * they are the reason a rule spelled "declared in tests/support/ or nowhere"
 * would have reddened an honest tree.
 */
function delegationTarget(fn, name) {
  const inner = fn.body.slice(1, -1).trim();
  const head = new RegExp(`^((?:${IDENT}\\s*::\\s*)+)${name}\\s*\\(`).exec(inner);
  if (head === null) return null;
  const close = parenEnd(inner, head[0].length - 1);
  const tail = inner.slice(close).trim();
  if (!/^(?:\?|\.\s*(?:to_owned|to_string|into|clone|as_str)\s*\(\s*\))*$/.test(tail)) return null;
  const segments = head[1].match(new RegExp(IDENT, 'g'));
  return segments === null ? null : segments[segments.length - 1];
}

/**
 * The module a `use` in this file imports `name` from, or `null`.
 *
 * The segment before the leaf, or before the brace group holding it: `use
 * crate::common::source_scan::{production_rust_code_only, …}` says the name
 * comes from `source_scan`, and `use crate::common::fake_view::
 * production_rust_code_only` says — just as plainly — that it does not. A
 * rename (`use x::y as production_rust_code_only`) answers `y`, which is not a
 * delegate, so it stays untrusted.
 */
function importedFrom(code, name) {
  const use = /\buse\s+([^;]*);/g;
  const leaf = new RegExp(`(?<![.\\w])${name}(?![\\w])`);
  for (let m = use.exec(code); m !== null; m = use.exec(code)) {
    const at = m[1].search(leaf);
    if (at < 0) continue;
    const before = m[1].slice(0, at);
    const brace = before.lastIndexOf('{');
    const path = brace < 0 ? before : before.slice(0, brace);
    const segments = path.match(new RegExp(IDENT, 'g'));
    if (segments === null) continue;
    return segments[segments.length - 1];
  }
  return null;
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

const declared = subjects.map((subject) => {
  const functions = rustFunctions(subject.code);
  // Which type each `fn` is a method of, and which types this file implements a
  // method of each name on. Both are facts about THIS file, which is what lets
  // `ResolvingSource::new(…)` be read as a call while `Vec::new()` stays
  // somebody else's method.
  const { owners, implTypes } = implMethods(subject.code, functions);
  return {
    file: subject.file,
    code: subject.code,
    functions: functions.map((fn) => ({
      ...fn,
      body: subject.code.slice(fn.open, fn.end),
      bodyText: subject.text.slice(fn.open, fn.end),
      methodOwners: owners,
      implType: implTypes.get(fn.start) ?? null,
    })),
  };
});

/**
 * Which seed names are the REAL view in each file.
 *
 * A declaration is the view when it is the one in `tests/support/rust_source.rs`
 * or a wrapper that delegates there; the module stems of those wrappers are what
 * an import is resolved against. Everything else carrying the name is an
 * ordinary local `fn` and launders exactly as much as its own body earns —
 * which for `text.to_owned()` is nothing.
 *
 * Two hops, because the delegation graph is two deep: the support module, and
 * the `common/` wrappers that `include!` it. The loop settles rather than
 * assuming that depth.
 *
 * What a lexical reader still cannot do is resolve a Rust path: a `mod` named
 * after a delegate file, declared locally and holding a fake, would be read as
 * the delegate. Naming the impostor is the cost of not resolving imports, and
 * it is a far narrower opening than the bare name was.
 */
const viewAccess = new Map(subjects.map((subject) => [subject.file, viewModules(subject)]));
const stemOf = (file) => file.slice(file.lastIndexOf('/') + 1).replace(/\.rs$/, '');
const trustedDeclarations = new Set();
const trustedStems = new Set();
for (let pass = 0; pass < 4; pass += 1) {
  let grew = false;
  for (const entry of declared) {
    const access = viewAccess.get(entry.file);
    for (const fn of entry.functions) {
      if (!SEED_NAMES.has(fn.name)) continue;
      const key = `${entry.file}\u0000${fn.name}`;
      if (trustedDeclarations.has(key)) continue;
      let resolved = entry.file === SUPPORT_FILE;
      if (!resolved) {
        const target = delegationTarget(fn, fn.name);
        resolved = target !== null && (access.modules.has(target) || trustedStems.has(target));
      }
      if (!resolved) continue;
      trustedDeclarations.add(key);
      if (entry.file !== SUPPORT_FILE) trustedStems.add(stemOf(entry.file));
      grew = true;
    }
  }
  if (!grew) break;
}

/**
 * The seed names a call site in this file may be read as the view.
 *
 * Resolved in the order Rust itself resolves a bare call: a declaration in this
 * file wins, an explicit `use` decides next, and only a file that `include!`s
 * the support module is taken at its word for a qualified spelling. A file that
 * does none of the three names the view without having it, which is precisely
 * probe A of card_e0f4ada65cee.
 */
const trustedViews = new Map();
for (const entry of declared) {
  const access = viewAccess.get(entry.file);
  const own = new Set(entry.functions.map((fn) => fn.name));
  const trusted = new Set();
  for (const name of SEED_NAMES) {
    if (entry.file === SUPPORT_FILE) {
      trusted.add(name);
      continue;
    }
    if (own.has(name)) {
      if (trustedDeclarations.has(`${entry.file}\u0000${name}`)) trusted.add(name);
      continue;
    }
    const from = importedFrom(entry.code, name);
    if (from !== null) {
      if (trustedStems.has(from)) trusted.add(name);
      continue;
    }
    if (access.direct || access.modules.size > 0) trusted.add(name);
  }
  trustedViews.set(entry.file, trusted);
  // Carried on the `fn` so the pooled closure over the shared modules asks the
  // question per DECLARING FILE: a fake view in a `common/` directory must not
  // make its own caller a normalizer for the whole workspace.
  for (const fn of entry.functions) fn.views = trusted;
}

/**
 * The base set narrowed to what this file's call sites can actually mean.
 *
 * Two subtractions, both of them "a name is not a behaviour":
 *
 *   - a seed the file cannot resolve to the real view;
 *   - a DERIVED name the file declares itself. `isShared` makes every `fn` under
 *     `tests/support/` or any `common/` directory a workspace-wide laundering
 *     name, so `masked_body` reaching a view in one crate's `common/` module
 *     laundered an unrelated `fn masked_body` in another crate — measured
 *     paired: the same guard is red with that module absent and green with it
 *     present. Rust resolves a bare call to the `fn` this file declares, so the
 *     shared name is dropped and the file's own closure re-derives it from ITS
 *     body, which is where the question belongs. A seed is exempt because
 *     `trustedViews` has already answered it more precisely.
 */
const resolvedIn = (file, names, ownNames) => {
  const trusted = trustedViews.get(file);
  return new Set(
    [...names].filter((name) =>
      SEED_NAMES.has(name) ? trusted.has(name) : !ownNames.has(name),
    ),
  );
};

const shared = declared.filter((entry) => isShared(entry.file)).flatMap((entry) => entry.functions);
if (shared.length === 0) {
  console.error(
    '❌ rust source views: no shared reader module was found under tests/support/ or a common/ ' +
      'directory — the normalizer set cannot be derived, so every read would look raw.',
  );
  process.exit(1);
}

// Only a code view launders. A `fn` that reaches `production_rust_source` and
// nothing else has answered the `#[cfg(test)]` question and left the comment
// question open, so it cannot end the conversation the way `serde_fields` —
// which calls `production_rust_code_only` — does.
// The functions that hand STRING-BEARING bytes back: the view itself and every
// `fn` that is it applied and returned. `production_source()` is the shape this
// tree writes, once per crate. Handing bytes into one is not an assertion — it
// is a view — but what comes out is followed rather than trusted, so it is
// computed FIRST and held out of the normalizer closure.
const sharedStringViews = discoverViewAliases(shared, VIEW_SEEDS.stringBearing);
// The `Type::method` spellings of the shared modules that launder, carried into
// every file's own closure: a shared reader spelled as a method is reached the
// same way a local one is.
const sharedQualified = new Set();
const sharedNormalizers = discoverNormalizers(
  shared,
  VIEW_SEEDS.codeOnly,
  sharedStringViews,
  sharedQualified,
);
const sharedViewAliases = discoverViewAliases(shared, VIEW_SEEDS.codeOnly);
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
  const here = slotHolding(body, boundName);
  if (here !== null) return here;
  // The same hop taken one binding further, and for the same reason rather
  // than by analogy: `let text = read_to_string(&path)?; let production =
  // view(&text); sources.push((path, production))` is the walk this tree
  // writes, and the name that reaches the tuple is the second one. This is a
  // hop about WHERE the bytes land, not about how far the taint is followed —
  // getting it wrong costs a false accusation on the path half of the pair, so
  // erring toward `null` here is erring toward silence, not toward safety.
  const relay = new RegExp(
    `\\b(?:let|const|static)\\s+(?:mut\\s+)?(${IDENT})\\s*(?::[^=;{}]*)?=\\s*[^;]*(?<![.\\w])${boundName}\\b[^;]*;`,
  ).exec(body);
  return relay ? slotHolding(body, relay[1]) : null;
}

/** The slot of the first multi-element tuple in `body` that holds `name`. */
function slotHolding(body, name) {
  const tuple = new RegExp(`\\(([^()]*(?<![.\\w])${name}\\b[^()]*)\\)`).exec(body);
  if (!tuple) return null;
  const elements = tuple[1].split(',');
  if (elements.length < 2) return null;
  const slot = elements.findIndex((element) => new RegExp(`(?<![.\\w])${name}\\b`).test(element));
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
  // The shared sets carry every seed; this file keeps only the ones it can
  // actually reach. Filtered BEFORE the closure, so a local helper wrapping an
  // impostor is not laundered by the wrapper either.
  const ownNames = new Set(own.map((fn) => fn.name));
  const stringViews = discoverViewAliases(
    own,
    resolvedIn(subject.file, sharedStringViews, ownNames),
  );
  const normalizers = discoverNormalizers(
    own,
    resolvedIn(subject.file, sharedNormalizers, ownNames),
    stringViews,
    new Set(sharedQualified),
  );
  const viewAliases = discoverViewAliases(
    own,
    resolvedIn(subject.file, sharedViewAliases, ownNames),
  );
  const fileWalkers = discoverWalkers([...own, ...shared]);
  const corpusFunctions = new Set([...own.map((fn) => fn.name), ...sharedNames]);
  // The seed names this file spells but cannot reach — named so the diagnostic
  // says which of the two things went wrong.
  const impostorViews = new Set(
    [...SEED_NAMES].filter((name) => !trustedViews.get(subject.file).has(name)),
  );
  // A helper whose result some caller hands straight to a named view is
  // producing Rust, whatever its path expression looks like. This is the
  // corpus declaring what the bytes are, which is the only thing that can
  // answer for a read whose path is assembled out of a table three hundred
  // lines away — and it is silent about a helper nobody feeds to a view, which
  // is what keeps every TOML and YAML reader in this workspace out.
  //
  // Asked once per function in the file, so the file is read once for every
  // name a view is fed and the answer is a lookup. The tail is a lookahead so
  // that a view nested in another's argument — `view(other_view(helper(…)))` —
  // is still a match of its own rather than text the outer one consumed.
  let fedToAView = null;
  const feedsAView = (name) => {
    if (fedToAView === null) {
      const views = [...normalizers, ...stringViews];
      fedToAView = new Set();
      if (views.length > 0) {
        const call = new RegExp(
          `\\b(?:${views.join('|')})(?=\\s*\\(\\s*&?\\s*(?:${IDENT}\\s*::\\s*)*(${IDENT})\\s*\\()`,
          'g',
        );
        for (const match of subject.code.matchAll(call)) fedToAView.add(match[1]);
      }
    }
    return fedToAView.has(name);
  };
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
      stringViews,
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
      // The derivation hop is for a read NOTHING has looked at yet: `functions
      // (&text)` hands back the original bytes of a body and the grep the guard
      // performs lives there. A string-bearing read is a different question —
      // its own is whether anyone applied a code view to it, and the two-view
      // idiom answers that by handing it to one. Following what such a helper
      // returns accuses the idiom itself: `team_owner_permission_guard` bounds
      // a `matches!` argument list in the code view and hands back the ORIGINAL
      // slice precisely because the literals are what it came to read, and
      // `serde_fields` decodes a field's type text the same way.
      const derived = [
        read,
        ...(read.stringBearing ? [] : derivedReads(scope.code, read, normalizers, viewAliases, stringViews)),
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
        for (const problem of usesOf(
          code,
          step,
          normalizers,
          corpusFunctions,
          stringViews,
          impostorViews,
        )) {
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
