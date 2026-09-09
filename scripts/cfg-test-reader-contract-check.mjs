#!/usr/bin/env node

// Asserts that this repository has exactly TWO readers of a test-gating
// `#[cfg(…)]` attribute — one per language — and that they answer the same
// fixture table the same way.
//
// Why this exists: the question "is the item under this attribute invisible to
// a production build?" decides what ~43 contract checks and every Rust
// workspace guard consider production code. Answer it wrong in one direction
// and a `#[cfg(all(test, unix))]` fixture enters every census as shipping code
// — a false red where a census forbids a construct, and the quieter false
// green where a census demands one and a test double answers for it. Answer it
// wrong in the other and `#[cfg(not(test))]` production code disappears from
// the same censuses.
//
// Both have happened, and the class was closed by hand seven times over seven
// cards (card_d67b6f433341, card_998f43a2dcb1, card_435d0a1c6e6e,
// card_d59ec1b23c68, card_38d725506ec6, card_fc3daaf07a4c, card_0a6ec0937f91).
// Every round found one more local copy of the reader with a fresh manual
// sweep, and every round fixed the copies it happened to find: the last one
// landed on two of four readers, because `crates/rg-http`'s
// `common/source_scan.rs` and one of its test files each carried their own
// literal comparison. Nothing in the repository objected to a fourth copy, and
// nothing objected to the two halves disagreeing — the contract between them
// was a sentence of prose in `scripts/lib/rust-consumer-contract.mjs` saying
// "the two views must agree", which is a comment, not a gate.
//
// So this file asserts the two facts that prose asserted:
//
//   1. UNIQUENESS. No file under `crates/`, `tests/` or `scripts/` decides
//      test-ness by matching the attribute against text of its own. The two
//      canonical readers are exempt by path, and everything else that wants the
//      answer calls one of them.
//
//   2. PARITY. `tests/support/cfg-test-attribute-parity.txt` holds one fixture
//      table, and this check runs every row of it through the JavaScript half.
//      The Rust half runs the same rows from the same file in
//      `crates/rg-cli/src/cli.rs`, so a row the two answer differently is red
//      on one side or the other. That the Rust consumer is still wired to the
//      table is checked here too — a table nobody reads is the same defect one
//      level down.
//
// HOW UNIQUENESS IS READ, and what it can miss. The key is deliberately not a
// function name: three of the four copies were spelled differently and one had
// no name of its own at all. What every copy did share is a *literal* — a
// string or a regex spelling a `cfg` attribute with the `test` atom in it —
// used as a PATTERN: compared with `==`, handed to `contains` / `starts_with` /
// `includes`, or written as a regex body. Fixture text that merely contains
// `#[cfg(test)]` is not that; a fixture is data, and the whole tree is full of
// it. A second, narrower sign covers the JS half's historical bug: matching the
// bare atom `test` against something whose name says it is a cfg attribute or
// predicate.
//
// The literals are read out of the string-bearing view and the pattern position
// out of the byte-aligned code view, so a commented-out reader and a reader
// spelled inside a doc comment are both invisible here — they are not part of
// the program either.
//
// Truth boundary, stated because it decides how to read a green run: a copy
// that recognises a test item WITHOUT any literal — by walking tokens and
// comparing identifiers it built up itself — is not visible to this reader.
// That shape has never been written in this tree, and writing it is a great
// deal more work than calling the shared reader, but a green run here is not a
// proof that it is absent. The parity half covers the other risk: a third
// reader that answers correctly today drifts silently only if it is outside the
// table, and the table is what the two canonical halves are held to.

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { jsCodeView, jsTextView } from './lib/js-source.mjs';
import {
  productionRustCode,
  testInclusiveRustCode,
  testInclusiveRustSource,
} from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));

// The mutation stand points this at a fixture tree, so it drives the real sweep
// rather than a copy of it.
const override = process.env.FORGEKEEP_CFG_TEST_READER_ROOT;
const root = override ? resolve(override) : resolve(scriptsDir, '..');

/** Where code that reads Rust lives — the Rust guards and the JS checks alike. */
const SUBJECT_DIRS = ['crates', 'tests', 'scripts'];

const SKIPPED_DIRS = new Set(['target', 'node_modules', '.git', 'dist', 'build']);

/**
 * The two readers, one per language.
 *
 * Exempt by PATH rather than by name: what makes them canonical is that
 * everything else delegates to them, and a path is what the delegation is
 * spelled against.
 */
const CANONICAL = new Map([
  ['tests/support/rust_source.rs', 'the Rust half — `is_test_only_cfg_attribute`'],
  ['scripts/lib/rust-consumer-contract.mjs', 'the JavaScript half — `cfgTestItemRanges`'],
]);

/** The shared fixture table, and the Rust file that must still be running it. */
const TABLE = 'tests/support/cfg-test-attribute-parity.txt';

const failures = [];

/* ------------------------------------------------------------------ *
 * Subjects
 * ------------------------------------------------------------------ */

function subdirectories(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  return entries
    .filter((entry) => !entry.name.startsWith('.'))
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((entry) => ({ name: entry.name, full: join(dir, entry.name), dir: entry.isDirectory() }));
}

/** Every `.rs` file under `dir`. */
function rustFiles(dir, found = []) {
  for (const entry of subdirectories(dir)) {
    if (entry.dir) {
      if (!SKIPPED_DIRS.has(entry.name)) rustFiles(entry.full, found);
    } else if (entry.name.endsWith('.rs')) {
      found.push(entry.full);
    }
  }
  return found;
}

/** Every `.mjs` file under `dir`. A separate walker from the one above so that
 * no reader — this repository's own raw-source ratchet included — has to infer
 * which extension a shared walk was called with. */
function scriptFiles(dir, found = []) {
  for (const entry of subdirectories(dir)) {
    if (entry.dir) {
      if (!SKIPPED_DIRS.has(entry.name)) scriptFiles(entry.full, found);
    } else if (entry.name.endsWith('.mjs')) {
      found.push(entry.full);
    }
  }
  return found;
}

function repoPath(file) {
  return relative(root, file).split('\\').join('/');
}

// One walk per language, and each file's bytes reach that language's
// normalizer and no other. Written as two loops rather than one loop with a
// ternary on purpose: the second shape hands `.rs` bytes to a name that also
// takes `.mjs` bytes, which is a raw read of Rust source by any reader that
// follows the value rather than the branch — including
// `raw-source-assertion-contract-check.mjs`, which is right to say so.
//
// Both views are TEST-INCLUSIVE. This sweep hunts a copy of the reader, and a
// copy declared inside a `#[cfg(test)]` module is exactly the thing being
// hunted; the production view would hide it. The rule that permits this is that
// a test item matching can only make the check RED here — it reports what it
// finds and requires nothing to be present, so a fixture cannot answer for a
// construct that has left the tree.
const subjects = [];
for (const dir of SUBJECT_DIRS) {
  for (const file of rustFiles(join(root, dir))) {
    const rust = readFileSync(file, 'utf8');
    subjects.push({
      file: repoPath(file),
      code: testInclusiveRustCode(rust),
      text: testInclusiveRustSource(rust),
      rust: true,
    });
  }
  for (const file of scriptFiles(join(root, dir))) {
    const script = readFileSync(file, 'utf8');
    subjects.push({
      file: repoPath(file),
      code: jsCodeView(script),
      text: jsTextView(script),
      rust: false,
    });
  }
}
subjects.sort((a, b) => a.file.localeCompare(b.file));

if (subjects.length === 0) {
  console.error(
    `❌ cfg-test reader: no .rs or .mjs file was found under ${SUBJECT_DIRS.join('/, ')}/ in ` +
      `${root} — the sweep has no corpus, so it would pass over anything.`,
  );
  process.exit(1);
}

for (const [path, role] of CANONICAL) {
  if (!subjects.some((subject) => subject.file === path)) {
    console.error(
      `❌ cfg-test reader: ${path} (${role}) is missing — the canonical reader cannot be exempted, ` +
        'and every caller of it would be read as a copy.',
    );
    process.exit(1);
  }
}

/* ------------------------------------------------------------------ *
 * 1. Uniqueness
 * ------------------------------------------------------------------ */

/**
 * The spans a literal occupies, taken as the difference between the two views.
 *
 * Comments are blanked in BOTH, so what is left is exactly the string, char and
 * regex bodies: the text view carries them and the code view does not. Reading
 * them off the diff rather than re-lexing keeps this check on the same lexer
 * the production views use, so a raw string or a template substitution is
 * understood here the same way it is understood there.
 */
function literalSpans(subject) {
  const spans = [];
  let start = null;
  for (let i = 0; i < subject.code.length; i += 1) {
    const blanked = subject.code[i] === ' ' && subject.text[i] !== ' ' && subject.text[i] !== '\n';
    if (blanked && start === null) start = i;
    else if (!blanked && start !== null) {
      spans.push([start, i]);
      start = null;
    }
  }
  if (start !== null) spans.push([start, subject.code.length]);
  return spans;
}

// A literal is about a cfg attribute when, with regex escapes and whitespace
// removed, it opens a `cfg` call and names the `test` atom. Assembled from
// pieces on purpose: spelling the whole marker here would make this file its
// own first offender.
const CFG_OPEN = `cfg${'('}`;
const TEST_ATOM = 'test';

function namesACfgTestAttribute(content) {
  const bare = content.replace(/\\/g, '').replace(/\s+/g, '');
  return bare.includes(CFG_OPEN) && bare.includes(TEST_ATOM);
}

// Text operations that turn a literal into a verdict about other bytes. The
// searching half of each language's string API and nothing else: `len()` on a
// literal says nothing about the program, while `contains` is the whole defect.
//
// The rewriting half — `replace` and its neighbours — is deliberately out. It
// produces text, not an answer, and no copy of the reader has ever been written
// that way; what IS written that way is a mutation stand deriving one fixture
// from another (`FREE_STANDING_TEST_ITEM.replace('#[cfg(test)]\n', '')` in
// `authz-gate-dialect-contract-check-regression.mjs`). Accusing that is how a
// ratchet becomes one nobody can keep green, which is how it gets deleted.
const RUST_PATTERN_CALL = new RegExp(
  String.raw`\.\s*(?:contains|starts_with|ends_with|find|rfind|split|splitn|rsplit|` +
    String.raw`strip_prefix|strip_suffix|matches|match_indices|eq|eq_ignore_ascii_case|` +
    String.raw`trim_start_matches|trim_end_matches|trim_matches)\s*\(\s*$`,
);
const RUST_REGEX_CALL = /(?:Regex|RegexBuilder)::new\s*\(\s*$/;
const RUST_COMPARISON = /(?:==|!=)\s*$/;

const JS_PATTERN_CALL = new RegExp(
  String.raw`\.\s*(?:includes|startsWith|endsWith|indexOf|lastIndexOf|split|` +
    String.raw`match|matchAll|search|localeCompare)\s*\(\s*$`,
);
const JS_COMPARISON = /(?:===|!==|==|!=)\s*$/;

/**
 * Whether the literal ending the code before `before` is used as a pattern.
 *
 * `after` catches the mirrored spelling — `"#[cfg(test)]" == line` reads the
 * same way round the other way — and a JS regex is a pattern by construction,
 * so its delimiter is enough.
 */
function patternPosition(subject, before, after) {
  if (subject.rust) {
    if (RUST_COMPARISON.test(before)) return 'compared with a Rust literal';
    if (RUST_PATTERN_CALL.test(before)) return 'searched for with a Rust literal';
    if (RUST_REGEX_CALL.test(before)) return 'compiled into a regex';
    if (/^\s*(?:==|!=)/.test(after)) return 'compared with a Rust literal';
    return null;
  }
  if (/\/$/.test(before)) return 'written as a regex';
  if (JS_COMPARISON.test(before)) return 'compared with a JavaScript literal';
  if (JS_PATTERN_CALL.test(before)) return 'searched for with a JavaScript literal';
  if (/^\s*(?:===|!==|==|!=)/.test(after)) return 'compared with a JavaScript literal';
  return null;
}

/**
 * The narrower second sign: the bare `test` atom matched against something
 * whose own name says it holds a cfg attribute or predicate.
 *
 * This is the exact shape the JS half shipped for as long as it existed —
 * `attribute.includes('test')` — and it carries no `cfg(` in the literal, so
 * the first sign cannot see it (card_fc3daaf07a4c).
 */
const CFG_SUBJECT = /(?:^|[^A-Za-z0-9_])((?:[A-Za-z0-9_]*(?:cfg|attribute|attr|predicate)[A-Za-z0-9_]*))\s*$/i;

/**
 * A literal's payload, with the delimiters the span carries taken off.
 *
 * A string span runs from its opening quote to its closing one — `r#"…"#`
 * included — while a JS regex span is already the body alone, because the code
 * view keeps the `/` delimiters so a call site still reads as one.
 */
function literalBody(content) {
  return content.replace(/^(?:[A-Za-z]*#*)?["'`]/, '').replace(/["'`]#*$/, '');
}

function bareAtomAgainstACfgSubject(subject, before, after, content) {
  // Word boundaries and anchors are how a regex spells the same bare atom.
  const bare = literalBody(content)
    .replace(/\\b/g, '')
    .replace(/[\^$]/g, '')
    .replace(/\\/g, '')
    .replace(/\s+/g, '');
  if (bare !== TEST_ATOM) return null;
  const found = 'matched the bare `test` atom against a cfg attribute';

  // `/test/.test(attribute)` puts the subject on the other side of the literal,
  // so the regex spelling is read forwards.
  if (!subject.rust && /\/$/.test(before)) {
    return /^\/[a-z]*\s*\.\s*(?:test|exec)\s*\(\s*[A-Za-z0-9_.]*(?:cfg|attribute|attr|predicate)/i.test(after)
      ? found
      : null;
  }

  const call = subject.rust ? RUST_PATTERN_CALL : JS_PATTERN_CALL;
  const match = call.exec(before);
  if (!match) return null;
  const receiver = before.slice(0, match.index);
  return CFG_SUBJECT.test(receiver) ? found : null;
}

function lineOf(subject, offset) {
  let line = 1;
  for (let i = 0; i < offset && i < subject.code.length; i += 1) {
    if (subject.code[i] === '\n') line += 1;
  }
  return line;
}

const copies = [];

for (const subject of subjects) {
  if (CANONICAL.has(subject.file)) continue;
  for (const [start, end] of literalSpans(subject)) {
    const content = subject.text.slice(start, end);
    const before = subject.code.slice(Math.max(0, start - 160), start);
    const after = subject.code.slice(end, end + 48);

    let why = null;
    if (namesACfgTestAttribute(content)) why = patternPosition(subject, before, after);
    if (why === null) why = bareAtomAgainstACfgSubject(subject, before, after, content);
    if (why === null) continue;

    copies.push({
      file: subject.file,
      line: lineOf(subject, start),
      why,
      literal: content.length > 60 ? `${content.slice(0, 57)}…` : content,
    });
  }
}

for (const copy of copies) {
  failures.push(
    `${copy.file}:${copy.line} ${copy.why} (${copy.literal}) — this is a second reader of ` +
      'a test-gating `#[cfg(…)]` attribute. Call `is_test_only_cfg_attribute` from ' +
      '`tests/support/rust_source.rs` (Rust) or `cfgTestItemRanges` from ' +
      '`scripts/lib/rust-consumer-contract.mjs` (JavaScript) instead: a copy is the reader ' +
      'minus whichever fix it was written before.',
  );
}

/* ------------------------------------------------------------------ *
 * 2. Parity — the JavaScript half against the shared table
 * ------------------------------------------------------------------ */

/**
 * The fixture table, parsed the way the Rust half parses it.
 *
 * Records are separated by a blank line; the first non-comment line of a record
 * is the verdict and the rest is the attribute, verbatim, so a rustfmt-wrapped
 * predicate survives the round trip.
 *
 * Kept byte-for-byte equivalent to `parity_rows` in `crates/rg-cli/src/cli.rs`
 * on purpose — a table read differently by the two halves would put the
 * disagreement in the parsers, which is the one place the whole arrangement is
 * meant to rule out.
 */
function parseTable(text) {
  const rows = [];
  for (const block of text.replace(/\r\n/g, '\n').split('\n\n')) {
    const lines = block
      .split('\n')
      .filter((line) => !line.trimStart().startsWith('//') && line.trim() !== '');
    if (lines.length === 0) continue;
    rows.push({ verdict: lines[0].trim(), attribute: lines.slice(1).join('\n') });
  }
  return rows;
}

const tablePath = join(root, TABLE);
let table = [];
try {
  table = parseTable(readFileSync(tablePath, 'utf8'));
} catch {
  failures.push(
    `${TABLE} is missing — the two readers have no shared fixture set, which is the state that ` +
      'let them drift apart in opposite directions without either test noticing.',
  );
}

// A floor rather than an exact count: the table is meant to grow. What it
// refuses is a table emptied or truncated into vacuous agreement.
const MIN_ROWS = 16;

if (table.length > 0 && table.length < MIN_ROWS) {
  failures.push(
    `${TABLE} holds ${table.length} row(s), fewer than the ${MIN_ROWS} the two halves are held ` +
      'to — a shrinking table agrees about less and less while staying green.',
  );
}

// The marker the probe item carries. Present in the production view means the
// item survived a non-test build; blanked means the reader called it test-only.
const PROBE = 'probe_item_marker';

for (const [index, row] of table.entries()) {
  if (row.verdict !== 'test-only' && row.verdict !== 'production') {
    failures.push(
      `${TABLE} row ${index + 1} states the verdict "${row.verdict}", which is neither ` +
        '`test-only` nor `production` — the Rust half rejects it too, so the row asserts nothing.',
    );
    continue;
  }
  if (row.attribute.trim() === '') {
    failures.push(`${TABLE} row ${index + 1} states a verdict with no attribute under it.`);
    continue;
  }

  const probe = `${row.attribute}\nmod ${PROBE} {}\n`;
  const blanked = !productionRustCode(probe).includes(PROBE);
  const answered = blanked ? 'test-only' : 'production';
  if (answered !== row.verdict) {
    failures.push(
      `${TABLE} row ${index + 1} — the JavaScript half reads ${JSON.stringify(row.attribute)} as ` +
        `${answered}, the table says ${row.verdict}. The Rust half answers the same rows in ` +
        'crates/rg-cli/src/cli.rs, so the two views no longer agree about what a production ' +
        'build compiles.',
    );
  }
}

/* ------------------------------------------------------------------ *
 * 3. The Rust half is still wired to the same table
 * ------------------------------------------------------------------ */

// Structural, and deliberately so: a Node check cannot execute Rust, and the
// alternative — letting the Rust half keep private fixtures — is exactly the
// arrangement that hid the last two drifts. What is provable here is that one
// Rust test reads THIS file and hands its rows to the canonical reader; that
// the rows then pass is `cargo nextest`'s answer, on the same bytes.
const rustConsumers = subjects.filter(
  (subject) => subject.rust && subject.text.includes('cfg-test-attribute-parity.txt'),
);

if (rustConsumers.length === 0) {
  failures.push(
    `no Rust file under ${SUBJECT_DIRS.join('/, ')}/ reads ${TABLE} — the table is answered by ` +
      'the JavaScript half alone, so the halves are back to one fixture set each and a drift ' +
      'between them is invisible again.',
  );
}

for (const consumer of rustConsumers) {
  const reads = /include_str!\s*\(/.test(consumer.code);
  const usesReader = /production_rust_code_only|is_test_only_cfg_attribute/.test(consumer.code);
  const asserts = /assert(?:_eq)?!/.test(consumer.code);
  if (reads && usesReader && asserts) continue;
  failures.push(
    `${consumer.file} names ${TABLE} but does not run it: ` +
      `${reads ? '' : 'no `include_str!` of it, '}` +
      `${usesReader ? '' : 'no call into the canonical Rust reader, '}` +
      `${asserts ? '' : 'no assertion on the verdict, '}` +
      'so the Rust half is wired to the table on paper only.',
  );
}

/* ------------------------------------------------------------------ *
 * Verdict
 * ------------------------------------------------------------------ */

if (failures.length > 0) {
  console.error('❌ cfg-test reader contract:');
  for (const failure of failures) console.error(`   - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ cfg-test reader contract: ${CANONICAL.size} canonical readers, no copy across ` +
    `${subjects.length} files; ${table.length} shared fixture rows answered by the JavaScript ` +
    `half and read by ${rustConsumers.length} Rust consumer(s).`,
);
