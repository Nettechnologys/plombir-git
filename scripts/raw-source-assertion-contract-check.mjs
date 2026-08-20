#!/usr/bin/env node

// Asserts that no contract check reads a guarded source file and then makes a
// textual assertion about the bytes it got back.
//
// Why this exists: raw bytes are not the program. A construct that was merely
// commented out, or that lives inside a `#[cfg(test)]` fixture, still satisfies
// `backend.includes('path = "…"')` — so the gate stays green over code that
// never runs, which is the one failure mode a passing check cannot tell you
// about. `scripts/lib/rust-source.mjs` and `scripts/lib/ts-source.mjs` exist to
// remove that: every reader there anchors in a production view and slices out
// of its string-bearing twin, so "commented out" fails like "deleted".
//
// The class was then closed by hand, one check at a time, fourteen times over
// four cards (card_64b6ede78939 and its predecessors). Each round found the
// remaining offenders with a fresh manual sweep, because nothing in the
// repository objected to a fifteenth. That is the ratchet this file is: a check
// added tomorrow that greps a guarded file raw goes red the same day, with no
// list for anybody to forget to extend.
//
// Three languages are guarded, and they are guarded for the same reason rather
// than by analogy. Both halves of a frontend/backend contract assert the same
// fact about the same wire, so a hole in one half is a hole in the contract:
// commenting out the line in `web/src/lib/api/packages.ts` that sets
// `Content-Disposition` left `package-publish-contract-check.mjs` green over a
// header the client no longer sends, months after the Rust half of that very
// check had been hardened (card_a54b6a2db9f2). YAML came third and cost the
// most: a single `#` in front of `run: node scripts/run-contract-checks.mjs` —
// the step that executes every check in this repository — left four gates
// green, `local-gate-coverage-contract-check.mjs` among them, whose entire job
// is proving that each check mechanism is executed by a job of `regression.yml`
// (card_fad8ad0ef007). The `LANGUAGES` table below is what keeps the halves in
// step: adding a language is a row, not a fork.
//
// The subject is a GLOB over `scripts/**/*.mjs` — the checks and the shared
// libraries alike. Its other half lives in
// `rust-source-view-contract-check.mjs`, which asks the same question of the
// guards written IN Rust under `crates/**` and `tests/support/**`: this file
// cannot see them, and by its own argument nothing objecting to them is how
// that half came to be closed by hand nine times. A hand-written subject list would be the same defect one
// level up. The one family held out is the mutation stands, for the reason
// given where the glob is taken.
//
// How it reads a script, and what that buys:
//   - a *guarded path* is a binding whose initializer carries a string literal
//     ending in one of the language's extensions (`crates/rg-http/src/api/
//     repos.rs`, `web/src/lib/api/packages.ts`), transitively through other
//     bindings and through object properties (`files.backend`);
//   - a *raw read* is `readFileSync(<guarded path>, …)`, or a call to a local
//     helper that does nothing but that, whose result is not handed straight to
//     a normalizer;
//   - a *normalizer* is discovered, not listed: seeded with that language's
//     production view builders in `scripts/lib/`, then closed over every
//     function whose own body calls one. `rustFnBlock` qualifies because it
//     calls `productionRustCode`, and `tsInterfaceBody` because it calls
//     `productionTsCode`; `requireBlock` does not, because it matches whatever
//     view its caller hands it — which is exactly the distinction that matters
//     here. The closure is per language, so a Rust view over TypeScript bytes
//     counts as raw, which is what it is;
//   - an *assertion* is a string method or a regex applied to the bytes, or the
//     bytes being handed to a function that will do one of those — a library
//     helper or one the check declares itself. The local half matters more than
//     it sounds: a check's own `expect(source, pattern, message)` is where all
//     of its assertions go, so covering only the imported helpers left every
//     check written that way invisible, `password-reset-contract-check.mjs`
//     among them with six raw reads of `password.rs`.
//
// Each seed set holds that language's *production* views and nothing else. The
// Rust set used to also carry the comment-only builders (`stripRustComments`,
// `stripRustNonCode`), which made a view that blanks comments but leaves
// `#[cfg(test)]` items standing count as normalized. It is not: a test double
// declared at column 0 satisfies an assertion written about the handler the
// server ships, so the gate stays green over an endpoint that may have left the
// binary — proved by renaming `list_runners_admin` and re-declaring it inside a
// `#[cfg(test)]` module (card_04cdbcb8d553).
//
// A directory walk is read too, and it took a second card to get there. A path
// that comes out of one is spelled by no literal on the way to the read — `for
// (const name of readdirSync(dir))` names only the directory — so the reader,
// which follows literals because that is all a lexical reader can do, walked
// straight past it and counted nothing. The floor could not catch that: a new
// walk-shaped check adds zero to the recognised count, so the corpus never
// shrinks and the floor never bites, and a fixture built that way ran the
// ratchet to `0 .rs read(s)` and exit 0 (card_2a23d37a583c). `walksLanguage`
// closes it by taking the extension from wherever the walk states it — the
// walker's own body, its call site, or the loop body that filters the entries.
//
// An element bound by a callback parameter is read the same way, which took a
// third card. `for (const file of paths)` and `paths.map((file) => …)` say the
// same thing about the same array, and the reader knew only the first, so
// `collaborators-contract-check.mjs` read its `.ts` client inside a `.map` and
// the read was not counted at all — seven regexes over raw bytes, and the floor
// none the wiser for the same reason a new walk never moved it (card_
// 690786ca30ab). What the callback hands BACK matters as much as what it takes:
// a pair `[file, readFileSync(file, 'utf8')]` puts the path in one slot and the
// program in the other, so the taint waits for the destructuring that names the
// slot — `for (const [file, source] of clients)` — and lands on `source` alone.
// Accusing `file` would put the raw-bytes verdict on the name the check's own
// failure messages quote, and a ratchet nobody can keep green is one somebody
// deletes. When the callback hands the bytes back whole instead, there is no
// slot to wait for and the binding the chain lands in is raw from then on.
//
// Truth boundary, stated because it decides how to read a green run. This is
// still a lexical reader, not a JS interpreter. It follows literals, path
// arithmetic and directory walks; a path that is none of those — assembled from
// a config value, or returned by an import this file cannot see — is not
// recognised, and a check built that way is not covered. The same holds one
// construct over: a callback passed by NAME (`paths.map(readOne)`) declares no
// parameter here to bind, and `.reduce` is deliberately not an element method
// because its first parameter is the accumulator — the element is the second,
// and no reader in this tree is written that way today. The per-language
// `minReads` floors below keep the recognised corpus from shrinking in silence:
// if a refactor moves the paths out of reach of this reader, the count
// collapses and the check goes red rather than passing over a corpus it can no
// longer see. What the floor cannot do is notice a corpus that never joined, so
// widening the reader is the only way that half gets covered.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { jsCodeView, jsTextView } from './lib/js-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptsDir, '..');

// The mutation stand points this at a fixture tree. Everything below is
// relative to it, so the stand exercises the real reader, not a copy of it.
const override = process.env.FORGEKEEP_RAW_SOURCE_ASSERT_ROOT;
const root = override ? resolve(override) : repoRoot;
const subjectDir = join(root, 'scripts');
const libDir = join(subjectDir, 'lib');

// The guarded languages. Each row names the extensions whose bytes must not be
// asserted over raw, the production view builders that make them safe, and the
// floor of recognised reads below which the reader is considered blind.
//
// `seeds` are that language's *production* views and nothing weaker — a view
// that blanks comments but leaves test doubles standing does not qualify; see
// the header. Anything calling a seed, directly or transitively, is discovered
// as a normalizer.
const LANGUAGES = [
  {
    name: 'Rust',
    extensions: ['.rs'],
    seeds: ['productionRustCode', 'productionRustSource', 'testInclusiveRustCode', 'testInclusiveRustSource'],
    // The floor: 48 reads are recognised on `main` today. Raise it when the
    // corpus grows; never lower it to make a red run go away.
    minReads: 43,
    skipped: 'commented-out and `#[cfg(test)]` code the server never ships',
    remedy: '   Read the file through `scripts/lib/rust-source.mjs` instead — `productionRustSource()` for a\n'
      + '   whole-file view, `rustFnBlock()` / `rustStructBody()` / `parseRouteTable()` for one declaration.\n'
      + '   A sweep that HUNTS test fixtures says so with `testInclusiveRustCode()` / `testInclusiveRustSource()`;\n'
      + '   a sweep that requires a construct to be present may not — a fixture declaring it is a false green.',
  },
  {
    name: 'TypeScript',
    extensions: ['.ts', '.svelte'],
    seeds: ['productionTsCode', 'productionTsSource'],
    // 96 reads are recognised on `main` today.
    minReads: 84,
    skipped: 'commented-out code the browser never runs',
    remedy: '   Read the file through `scripts/lib/ts-source.mjs` instead — `productionTsSource()` for a\n'
      + '   whole-file view, `tsInterfaceBody()` / `tsFunctionBody()` for one declaration.',
  },
  {
    name: 'YAML',
    extensions: ['.yml', '.yaml'],
    seeds: ['productionYamlSource'],
    // 3 reads are recognised on `main` today. The number is small because the
    // right answer for most YAML claims is the *parsed* document rather than
    // any text view, and a check that reads a document instead of its bytes has
    // nothing here to count — see the remedy. The floor therefore sits one
    // below: converting one of the three to the parser is the improvement this
    // gate asks for, and must not read as the reader going blind.
    minReads: 2,
    skipped: 'commented-out configuration no parser ever loads',
    remedy: '   Read the parsed document instead — `scripts/lib/workflow.mjs` for a workflow job graph,\n'
      + '   `parseYamlFile()` for anything else — or, when the claim is genuinely textual,\n'
      + '   `productionYamlSource()` / `yamlAnnotatedLines()` from `scripts/lib/yaml-source.mjs`.',
  },
];

// Methods that turn a string into an assertion or into a slice of itself.
// Reading any of them off raw source bytes is the defect.
const RAW_STRING_METHODS = [
  'includes', 'match', 'matchAll', 'indexOf', 'lastIndexOf', 'search',
  'split', 'startsWith', 'endsWith', 'replace', 'replaceAll',
  'slice', 'substring', 'substr',
];

const IDENT = '[A-Za-z_$][A-Za-z0-9_$]*';

// Array methods whose callback is handed one ELEMENT of the receiver. A
// parameter bound by one of them is the loop variable of a `for … of` written
// the other way round, and is read as one. `reduce` is not here: its first
// parameter is the accumulator.
const ELEMENT_METHODS = ['map', 'flatMap', 'filter', 'forEach', 'find', 'findLast', 'some', 'every'];

function listScripts(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listScripts(full));
    else if (entry.isFile() && entry.name.endsWith('.mjs')) out.push(full);
  }
  return out;
}

/** Index just past the `)`/`}`/`]` closing the bracket at `open`. */
function closingBracket(code, open) {
  const pairs = { '(': ')', '{': '}', '[': ']' };
  const close = pairs[code[open]];
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    const ch = code[i];
    if (ch === code[open]) depth += 1;
    else if (ch === close) {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
  }
  return code.length;
}

/** Index of the `(`/`[`/`{` that the bracket closing at `close` was opened by. */
function openingBracket(code, close) {
  const pairs = { ')': '(', ']': '[', '}': '{' };
  const open = pairs[code[close]];
  let depth = 0;
  for (let i = close; i >= 0; i -= 1) {
    const ch = code[i];
    if (ch === code[close]) depth += 1;
    else if (ch === open) {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return 0;
}

/**
 * Start of the expression the `.` at `dot` is a member access on: `clientPaths`
 * in `clientPaths.map(…)`, the whole `readdirSync(dir).filter(…)` chain in
 * `readdirSync(dir).filter(…).map(…)`.
 *
 * Walking backwards rather than matching an identifier forwards is what keeps
 * the chained shape in reach — `openapi-route-coverage-contract-check.mjs`
 * reads every `.rs` file of `rg-http` off a walk it has already filtered, and a
 * receiver rule that stopped at the first `)` would call that array nothing.
 */
function receiverStart(code, dot) {
  let i = dot - 1;
  for (;;) {
    while (i >= 0 && /\s/.test(code[i])) i -= 1;
    if (i < 0) return 0;
    const ch = code[i];
    if (ch === ')' || ch === ']') {
      i = openingBracket(code, i) - 1;
      continue;
    }
    if (!/[A-Za-z0-9_$]/.test(ch)) return i + 1;
    while (i >= 0 && /[A-Za-z0-9_$]/.test(code[i])) i -= 1;
    let j = i;
    while (j >= 0 && /\s/.test(code[j])) j -= 1;
    if (j >= 0 && code[j] === '.') {
      i = j - 1;
      continue;
    }
    return i + 1;
  }
}

/**
 * The names of a flat array pattern, by slot: `file, source` → `['file',
 * 'source']`, a hole or a rest element → `null` in that slot.
 *
 * A nested pattern gives back `null` for the whole thing rather than a guess:
 * which slot of `[a, [b, c]]` a name sits in is not something this reader can
 * answer, and a wrong slot is a wrong accusation.
 */
function arrayPatternNames(pattern) {
  if (/[[\]{}]/.test(pattern)) return null;
  return pattern.split(',').map((part) => {
    const name = part.trim();
    return new RegExp(`^${IDENT}$`).test(name) ? name : null;
  });
}

/** Index just past the `;` (or newline) that ends the initializer at `from`. */
function statementEnd(code, from) {
  let depth = 0;
  for (let i = from; i < code.length; i += 1) {
    const ch = code[i];
    if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') {
      if (depth === 0) return i;
      depth -= 1;
    } else if (ch === ';' && depth === 0) return i;
  }
  return code.length;
}

/**
 * Every top-level-ish binding of the file: `const|let|var NAME = <init>` and
 * `function NAME(…) { … }`, with the code and text spans of what it binds.
 */
function bindings(code, text) {
  const found = [];
  // `const x = …`, `let x = …` and the bare re-assignment `x = …` that a
  // `let x = ''; try { x = readFileSync(…) }` opens with. Leaving the bare form
  // out would lose the taint exactly where the read is hardest to see.
  //
  // The match ends AT the `=`, never past the whitespace after it. In the code
  // view a string literal is blanked *including its quotes*, so a trailing
  // `\s*` here would swallow the entire initializer of `const p = 'a/b.svelte'`
  // and hand back an empty span — the binding then carries no path, the read of
  // it is not counted, and the file drops out of the sweep in silence. Nine
  // checks sat in that blind spot, `commit-links-contract-check.mjs` among
  // them, and the Rust half had it too: any path written as a bare literal
  // rather than assembled with `path.join(…)` was invisible.
  const assign = new RegExp(
    `(?:^|[\\s;{}()])(?:(const|let|var)\\s+)?(${IDENT})\\s*(?<![=!<>+\\-*/%&|^])=(?!=)`,
    'g',
  );
  for (let m = assign.exec(code); m !== null; m = assign.exec(code)) {
    const start = m.index + m[0].length;
    const end = statementEnd(code, start);
    found.push({
      name: m[2],
      start,
      end,
      code: code.slice(start, end),
      text: text.slice(start, end),
      declared: Boolean(m[1]),
    });
  }
  // `for (const file of ['docker-compose.yml', …])` binds a path without an
  // `=` anywhere, so the scanner above walks straight past it: the loop
  // variable carries no path, the read of it is not counted, and the file
  // leaves the sweep in silence. `deploy-config-concurrency-contract-check.mjs`
  // sat in exactly that blind spot, asserting `compose.includes('- ${…}')` over
  // raw bytes of two compose files, and `repo-actions-contract-check.mjs` in
  // the same one over three `.ts` clients.
  //
  // Marked `iterated`, because what may be iterated is wider than what may be a
  // path: see the `pathArithmeticOnly` guard in `analyse`.
  const iterate = new RegExp(`\\bfor\\s*(?:await\\s*)?\\(\\s*(?:const|let|var)\\s+(${IDENT})\\s+(?:of|in)\\s+`, 'g');
  for (let m = iterate.exec(code); m !== null; m = iterate.exec(code)) {
    const start = m.index + m[0].length;
    const afterHead = closingBracket(code, code.indexOf('(', m.index));
    const end = Math.max(start, afterHead - 1);
    // The loop body travels with the binding, because that is where a
    // directory walk usually says which files it means: `for (const name of
    // readdirSync(dir)) { if (!name.endsWith('.rs')) continue; … }` spells the
    // extension nowhere else. See `walksLanguage`.
    let bodyStart = afterHead;
    while (bodyStart < code.length && /\s/.test(code[bodyStart])) bodyStart += 1;
    const bodyEnd = code[bodyStart] === '{'
      ? closingBracket(code, bodyStart)
      : Math.min(code.length, statementEnd(code, bodyStart) + 1);
    found.push({
      name: m[1],
      start,
      end,
      code: code.slice(start, end),
      text: text.slice(start, end),
      iterated: true,
      declared: true,
      spanStart: m.index,
      spanEnd: bodyEnd,
      bodyCode: code.slice(bodyStart, bodyEnd),
      bodyText: text.slice(bodyStart, bodyEnd),
    });
  }
  // `for (const [file, source] of clients)` — the same loop, destructured. The
  // scanner above wants an identifier where the pattern stands, so this shape
  // bound nothing at all and the bytes a producer left in one of its slots
  // reached their assertion under a name the reader had never heard of.
  //
  // Each name is bound to its SLOT rather than to the tuple, because the slot
  // is the only thing that separates them: in `[file, readFileSync(file,
  // 'utf8')]` slot 0 is a path the failure messages quote and slot 1 is the
  // program. A reader that tainted both would accuse `path.relative(root,
  // file)` of asserting over source bytes, and a ratchet nobody can keep green
  // is one somebody deletes.
  const iterateTuple = new RegExp(
    `\\bfor\\s*(?:await\\s*)?\\(\\s*(?:const|let|var)\\s+\\[([^\\]\\[{}]*)\\]\\s+(?:of|in)\\s+`,
    'g',
  );
  for (let m = iterateTuple.exec(code); m !== null; m = iterateTuple.exec(code)) {
    const start = m.index + m[0].length;
    const afterHead = closingBracket(code, code.indexOf('(', m.index));
    const end = Math.max(start, afterHead - 1);
    let bodyStart = afterHead;
    while (bodyStart < code.length && /\s/.test(code[bodyStart])) bodyStart += 1;
    const bodyEnd = code[bodyStart] === '{'
      ? closingBracket(code, bodyStart)
      : Math.min(code.length, statementEnd(code, bodyStart) + 1);
    (arrayPatternNames(m[1]) ?? []).forEach((name, slot) => {
      if (name === null) return;
      found.push({
        name,
        start,
        end,
        code: code.slice(start, end),
        text: text.slice(start, end),
        iterated: true,
        declared: true,
        slot,
        spanStart: m.index,
        spanEnd: bodyEnd,
        bodyCode: code.slice(bodyStart, bodyEnd),
        bodyText: text.slice(bodyStart, bodyEnd),
      });
    });
  }
  // `clientPaths.map((file) => …)` — an element bound by a callback parameter.
  // `for … of` and `.map` say the same thing about the same array and the
  // reader knew only one of them, so `collaborators-contract-check.mjs` read
  // its `.ts` client inside a `.map` and the read was not counted at all. That
  // is the half the floor cannot cover: a read nobody recognises adds zero to
  // the corpus, so the count never shrinks and nothing objects while seven
  // regexes run over raw bytes (card_690786ca30ab).
  //
  // `reduce` is deliberately absent: its first parameter is the accumulator,
  // not the element, and binding it would hand the array's guardedness to a
  // value that never held one of its members.
  const callback = new RegExp(`\\.\\s*(?:${ELEMENT_METHODS.join('|')})\\s*\\(`, 'g');
  for (let m = callback.exec(code); m !== null; m = callback.exec(code)) {
    const open = m.index + m[0].length - 1;
    const close = closingBracket(code, open);
    const args = code.slice(open + 1, close - 1);
    const head = new RegExp(
      `^\\s*(?:async\\s+)?(?:\\(\\s*(\\[[^\\]\\[{}]*\\]|${IDENT})\\s*(?:,[^)]*)?\\)|(${IDENT}))\\s*=>`,
    ).exec(args);
    // A callback passed by name (`files.map(readOne)`) declares no parameter
    // here, so there is nothing to bind and nothing to pretend about.
    if (head === null) continue;
    const spanStart = receiverStart(code, m.index);
    const bodyStart = open + 1 + head[0].length;
    const base = {
      start: open + 1,
      end: Math.max(open + 1, close - 1),
      code: code.slice(spanStart, m.index),
      text: text.slice(spanStart, m.index),
      iterated: true,
      declared: true,
      spanStart,
      spanEnd: close,
      bodyCode: code.slice(bodyStart, close - 1),
      bodyText: text.slice(bodyStart, close - 1),
    };
    const first = head[1] ?? head[2];
    if (!first.startsWith('[')) {
      found.push({ ...base, name: first });
      continue;
    }
    (arrayPatternNames(first.slice(1, -1)) ?? []).forEach((name, slot) => {
      if (name !== null) found.push({ ...base, name, slot });
    });
  }
  const fn = new RegExp(`(?:^|[\\s;{}()])(?:export\\s+)?(?:async\\s+)?function\\s+(${IDENT})\\s*\\(`, 'g');
  for (let m = fn.exec(code); m !== null; m = fn.exec(code)) {
    const paren = code.indexOf('(', m.index + m[0].length - 1);
    const brace = code.indexOf('{', closingBracket(code, paren));
    if (brace < 0) continue;
    const end = closingBracket(code, brace);
    found.push({
      name: m[1],
      start: m.index,
      end,
      code: code.slice(m.index, end),
      text: text.slice(m.index, end),
      callable: true,
      declared: true,
    });
  }
  return found;
}

/**
 * Names in `decls` that a call site can hand a source string to: `function
 * f(…)` declarations and `const f = (…) => …` bindings alike.
 *
 * These matter because a check's own assertion helper is where a whole file's
 * assertions go. `expect(page, /pattern/, 'message')` reads exactly like the
 * library helpers already covered, and it launders the bytes just as
 * thoroughly — nine checks funnelled every one of their assertions through a
 * local `expect` or `check` and were invisible for it.
 */
function localCallables(decls) {
  const names = new Set();
  for (const decl of decls) {
    if (decl.callable) names.add(decl.name);
    else if (/^\s*(?:async\s+)?(?:\([^)]*\)|[A-Za-z_$][A-Za-z0-9_$]*)\s*=>/.test(decl.code)) names.add(decl.name);
    else if (/^\s*(?:async\s+)?function\b/.test(decl.code)) names.add(decl.name);
  }
  return names;
}

/** Names of functions in `files` whose body reaches one of `seeds`. */
function discoverNormalizers(files, seeds) {
  const normalizers = new Set(seeds);
  const bodies = [];
  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    for (const binding of bindings(jsCodeView(source), jsTextView(source))) {
      bodies.push(binding);
    }
  }
  for (let pass = 0; pass < 8; pass += 1) {
    let grew = false;
    for (const body of bodies) {
      if (normalizers.has(body.name)) continue;
      for (const known of normalizers) {
        if (new RegExp(`\\b${known}\\s*\\(`).test(body.code)) {
          normalizers.add(body.name);
          grew = true;
          break;
        }
      }
    }
    if (!grew) break;
  }
  return normalizers;
}

/** Call names whose `(` is still open at `index`, outermost first. */
function enclosingCalls(code, index) {
  const stack = [];
  for (let i = 0; i < index; i += 1) {
    const ch = code[i];
    if (ch === '(') {
      const before = code.slice(Math.max(0, i - 80), i);
      const callee = new RegExp(`(${IDENT}(?:\\s*\\.\\s*${IDENT})*)\\s*$`).exec(before);
      stack.push(callee ? callee[1].replace(/\s+/g, '') : null);
    } else if (ch === ')') {
      stack.pop();
    }
  }
  return stack;
}

/**
 * The `key: value` pairs of an object literal, each value taken up to the comma
 * that closes it at depth zero.
 *
 * The value must not be cut at the first comma. `backend: path.join(root,
 * 'api/repos.rs')` carries its `.rs` literal *after* one, so a comma-terminated
 * read declares the property path-free and the whole file drops out of the
 * sweep — which is how `org-repo-create-contract-check.mjs` asserted over a
 * comment-only view without ever appearing in an offender list. Matching runs
 * over the code view, so a comma inside a string literal cannot end a value.
 */
function objectProperties(code, text) {
  const out = [];
  const key = new RegExp(`(${IDENT})\\s*:`, 'g');
  for (let m = key.exec(code); m !== null; m = key.exec(code)) {
    const start = m.index + m[0].length;
    let depth = 0;
    let end = start;
    for (; end < code.length; end += 1) {
      const ch = code[end];
      if ('([{'.includes(ch)) depth += 1;
      else if (')]}'.includes(ch)) {
        if (depth === 0) break;
        depth -= 1;
      } else if (ch === ',' && depth === 0) break;
    }
    out.push({ key: m[1], value: text.slice(start, end) });
    key.lastIndex = end;
  }
  return out;
}

/**
 * The array literal the expression at `index` sits directly inside, and which
 * slot of it — `{ open, slot: 1 }` for the `readFileSync(…)` in `[file,
 * readFileSync(file, 'utf8')]`. `null` when the innermost bracket still open at
 * `index` is not an array literal.
 *
 * This is the JS half of what `tupleSlot` does for the Rust reader, and it
 * exists for the same reason: a producer that hands back a pair puts the path
 * in one slot and the program in the other, and only one of them may be
 * accused. Matching runs over the code view, so a bracket or a comma inside a
 * string literal cannot move a slot.
 */
function arraySlot(code, index) {
  const stack = [];
  for (let i = 0; i < index; i += 1) {
    const ch = code[i];
    if ('([{'.includes(ch)) stack.push({ ch, open: i, slot: 0 });
    else if (')]}'.includes(ch)) stack.pop();
    else if (ch === ',' && stack.length > 0) stack[stack.length - 1].slot += 1;
  }
  const top = stack[stack.length - 1];
  return top !== undefined && top.ch === '[' ? { open: top.open, slot: top.slot } : null;
}

/**
 * A predicate for "this text spells out a path to a file of `lang`".
 *
 * The extension has to sit at the very end of the literal, so `'.ts'` used as
 * an extension *test* (`name.endsWith('.ts')`) still reads as a path — which is
 * deliberate: a directory walk that filters on the extension is the one shape
 * the reader cannot follow, and treating it as a path keeps the read counted
 * rather than silently dropped.
 */
function literalPredicate(lang) {
  const alternatives = lang.extensions.map((ext) => ext.replace('.', '\\.')).join('|');
  const re = new RegExp(`['"\`][^'"\`\\n]*(?:${alternatives})['"\`]`);
  return (text) => re.test(text);
}

// Path helpers a path may travel through and still be a path.
const PATH_CALLS = new Set(['join', 'resolve', 'normalize', 'relative', 'dirname', 'String']);

/** True when every call in `code` merely assembles a path. */
function pathArithmeticOnly(code) {
  const call = new RegExp(`(${IDENT}(?:\\s*\\.\\s*${IDENT})*)\\s*\\(`, 'g');
  for (let m = call.exec(code); m !== null; m = call.exec(code)) {
    if (!PATH_CALLS.has(m[1].replace(/\s+/g, '').split('.').pop())) return false;
  }
  return true;
}

// Calls that enumerate a directory. A path built out of one is spelled by no
// literal anywhere on the way to the read — `for (const name of
// readdirSync(dir))` names only the directory — so following literals, which is
// all a lexical reader can do, walks straight past it. Four readers in this
// tree already build paths that way; until this was recognised none of their
// reads were counted, and a check written in that shape could grep raw bytes
// and stay green with the floor none the wiser (card_2a23d37a583c).
const DIRECTORY_READS = ['readdirSync', 'readdir', 'opendirSync', 'globSync', 'glob'];
const DIRECTORY_READ = new RegExp(`\\b(?:${DIRECTORY_READS.join('|')})\\s*\\(`);

/**
 * Declared functions whose body enumerates a directory, mapped to the text of
 * that body — `rustFiles`, `sourceFiles`, `listScripts`.
 *
 * Discovered the way the normalizers are, and for the same reason: a wrapper
 * around a walk is a walk, and a hand-written list of walker names would be the
 * defect this file exists to close, one level up. `inherited` seeds the sweep
 * with the walkers of `scripts/lib/`, so a check that imports `rustFiles`
 * rather than declaring its own is read the same way.
 */
function discoverWalkers(decls, inherited = new Map()) {
  const walkers = new Map(inherited);
  const callables = localCallables(decls);
  for (let pass = 0; pass < 4; pass += 1) {
    let grew = false;
    for (const decl of decls) {
      if (walkers.has(decl.name) || !callables.has(decl.name)) continue;
      const reaches = DIRECTORY_READ.test(decl.code)
        || [...walkers.keys()].some((name) => new RegExp(`\\b${name}\\s*\\(`).test(decl.code));
      if (!reaches) continue;
      walkers.set(decl.name, decl.text);
      grew = true;
    }
    if (!grew) break;
  }
  return walkers;
}

/**
 * True when `body` filters `name` down to files of `lang` — the extension test
 * a directory walk keeps in the loop body rather than in the expression it
 * iterates. `entry.name.endsWith('.rs')` counts for the loop variable `entry`,
 * so a `Dirent` walk reads the same as a string one.
 */
function filtersOnExtension(body, name, lang) {
  if (!body) return false;
  const who = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const exts = lang.extensions.map((ext) => ext.replace('.', '\\.')).join('|');
  return new RegExp(`\\b${who}\\s*(?:\\.\\s*${IDENT}\\s*)*\\.\\s*endsWith\\s*\\(\\s*['"\`][^'"\`\n]*(?:${exts})['"\`]`).test(body)
    || new RegExp(`\\bextname\\s*\\(\\s*${who}[^)\n]*\\)\\s*===?\\s*['"\`](?:${exts})['"\`]`).test(body)
    || new RegExp(`/[^/\n]*(?:${exts})\\$?/[gimsuy]*\\s*\\.\\s*test\\s*\\(\\s*${who}\\b`).test(body);
}

/**
 * True when `decl` binds a path to a file of `lang` that came out of a
 * directory walk. Three shapes, because the extension can be written in three
 * places and only one of them is a literal on the binding itself:
 *   - inside the walker (`rustFiles(cratesDir)` filters on `.rs` in its body);
 *   - at the call site (`sourceFiles(join(root, 'crates'), ['.rs'])`, whose
 *     sibling call passes `['.sh', '.py']` and must stay out of the Rust pass);
 *   - in the loop body (`for (const name of readdirSync(dir)) { if
 *     (!name.endsWith('.rs')) continue; … }`).
 *
 * A bare directory read with no extension named anywhere is deliberately not a
 * path of any language: `listScripts(dir)` walking `.mjs` must not make every
 * read of its result look like a read of Rust.
 */
function walksLanguage(decl, walkers, hasPathLiteral, lang) {
  const called = [];
  const call = new RegExp(`(${IDENT}(?:\\s*\\.\\s*${IDENT})*)\\s*\\(`, 'g');
  for (let m = call.exec(decl.code); m !== null; m = call.exec(decl.code)) {
    called.push(m[1].replace(/\s+/g, '').split('.').pop());
  }
  const walkerCalls = called.filter((name) => walkers.has(name));
  if (walkerCalls.some((name) => hasPathLiteral(walkers.get(name)))) return true;
  if (walkerCalls.length === 0 && !DIRECTORY_READ.test(decl.code)) return false;
  return hasPathLiteral(decl.text) || filtersOnExtension(decl.bodyText, decl.name, lang);
}

function analyse(file, source, lang, normalizers, libFunctions, libWalkers) {
  const hasPathLiteral = literalPredicate(lang);
  const code = jsCodeView(source);
  const text = jsTextView(source);
  const decls = bindings(code, text);
  const walkers = discoverWalkers(decls, libWalkers);
  const problems = [];

  // A *region* is the narrowest span in which a name can be trusted to mean one
  // value: the loop that declares it, or the function that does. Names are not
  // unique in a file, and reading one across the whole file merges values that
  // have nothing to do with each other. `released-port-contract-check.mjs`
  // walks `crates/` for `.rs` and then `scripts/` for `.sh` with a loop
  // variable called `file` both times, so the shell sweep read as a raw read of
  // Rust; `scripts/lib/rust-source.mjs` gives a dozen view builders a parameter
  // called `source`, so every one of them read as the local that
  // `loadUtoipaPaths` binds its read to. Both only became reachable once the
  // walks themselves did.
  const isArrow = (decl) => /^\s*(?:async\s+)?(?:\([^)]*\)|[A-Za-z_$][A-Za-z0-9_$]*)\s*=>/.test(decl.code);
  const scopes = decls.filter((decl) => decl.callable || isArrow(decl));
  const regions = [
    ...scopes.map((decl) => ({ start: decl.start, end: decl.end })),
    ...decls.filter((decl) => decl.iterated).map((decl) => ({ start: decl.spanStart, end: decl.spanEnd })),
  ];
  const wholeFile = { start: 0, end: code.length };
  const regionAt = (index) => {
    let best = null;
    for (const region of regions) {
      if (index < region.start || index >= region.end) continue;
      if (best === null || region.end - region.start < best.end - best.start) best = region;
    }
    return best ?? wholeFile;
  };
  // A binding introduced with `const`/`let`/`var` belongs to its region. A bare
  // re-assignment does not: `let src = ''; try { src = readFileSync(…) }` is the
  // *outer* name being filled in, and narrowing it to the block would drop the
  // taint exactly where the read is hardest to see.
  const regionOf = (decl) => (decl.declared === false ? wholeFile : regionAt(decl.start));
  const within = (ranges, index) => (ranges ?? []).some((r) => index >= r.start && index < r.end);
  const record = (map, name, range) => {
    const ranges = map.get(name) ?? [];
    if (!ranges.some((r) => r.start === range.start && r.end === range.end)) ranges.push(range);
    map.set(name, ranges);
  };

  // A function that takes `raw` as a parameter is not holding the `raw` an
  // outer scope bound a read to, whatever the call site passes it.
  const parameters = new Map();
  const declares = (scope, name) => {
    if (!parameters.has(scope)) {
      const open = scope.code.indexOf('(');
      const close = open < 0 ? -1 : closingBracket(scope.code, open);
      const list = open < 0 ? scope.code.slice(0, scope.code.indexOf('=>')) : scope.code.slice(open + 1, close - 1);
      parameters.set(scope, new Set(list.match(new RegExp(IDENT, 'g')) ?? []));
    }
    return parameters.get(scope).has(name);
  };
  const shadowed = (name, index) => scopes.some(
    (scope) => index >= scope.start && index < scope.end && declares(scope, name),
  );

  // 1. Which names hold, or lead to, a path to a file of this language.
  //
  // A path propagates only through path arithmetic (`join`, `resolve`). It must
  // not propagate through a read: `readFileSync(backendPath, …)` yields the
  // file's *contents*, and treating those as another path would make every
  // later binding that mentions them look like a source file of its own.
  const guardedPaths = new Map(); // name -> ranges in which it names such a path
  const guardedMembers = new Map(); // object name -> Set of keys holding such a path
  const mentions = (name, haystack) => new RegExp(`\\b${name.replace(/\./g, '\\s*\\.\\s*')}\\b`).test(haystack);
  const guardedAt = (name, index) => within(guardedPaths.get(name), index);
  for (let pass = 0; pass < 6; pass += 1) {
    let grew = false;
    for (const decl of decls) {
      if (guardedAt(decl.name, decl.start)) continue;
      // A slot of a tuple is not the tuple. Which slot of `[key, file]` holds a
      // path is not something this reader is told, so a destructured name does
      // not inherit the iterable's guardedness — guessing would make the key of
      // an entry pair read as a source file.
      if (decl.slot !== undefined) continue;
      // What a loop may walk is wider than what may be a path, so the loop
      // variable is held to the same rule the derived branch applies: a path
      // travels through path arithmetic, not through an arbitrary call. The one
      // exception is the arbitrary call that *is* a path source — a directory
      // walk filtered on this language's extension, which spells its files with
      // no literal a reader could follow (card_2a23d37a583c).
      const walked = walksLanguage(decl, walkers, hasPathLiteral, lang);
      if (decl.iterated && !walked && !pathArithmeticOnly(decl.code)) continue;
      const isObject = /^\s*\{/.test(decl.code);
      if (hasPathLiteral(decl.text) && isObject) {
        // Only the matching properties are paths; the object as a whole is a
        // mixed bag of client, page and backend files, and each language's pass
        // picks out its own.
        for (const { key, value } of objectProperties(decl.code, decl.text)) {
          if (!hasPathLiteral(value) || guardedAt(`${decl.name}.${key}`, decl.start)) continue;
          record(guardedPaths, `${decl.name}.${key}`, regionOf(decl));
          if (!guardedMembers.has(decl.name)) guardedMembers.set(decl.name, new Set());
          guardedMembers.get(decl.name).add(key);
          grew = true;
        }
        continue;
      }
      const derived = [...guardedPaths.keys()].some(
        (name) => guardedAt(name, decl.start) && mentions(name, decl.code),
      ) && pathArithmeticOnly(decl.code);
      // The other half of the walk shape: one that ACCUMULATES instead of
      // returning. `const files = []`, filled by `files.push(relative)` from
      // inside the walk, carries every path the walk found and not one literal
      // — `loadUtoipaPaths` in `scripts/lib/rust-source.mjs` is built that way.
      // Guardedness is asked at the PUSH, because that is where the walk
      // variable is in scope; the array itself is a path from then on.
      let collects = false;
      if (!derived && /^\s*\[\s*\]\s*$/.test(decl.code)) {
        const push = new RegExp(`\\b${decl.name}\\s*\\.\\s*push\\s*\\(`, 'g');
        for (let m = push.exec(code); m !== null && !collects; m = push.exec(code)) {
          const open = m.index + m[0].length - 1;
          const pushed = code.slice(open + 1, closingBracket(code, open) - 1).replace(/^\s*\.\.\./, '');
          collects = pathArithmeticOnly(pushed)
            && [...guardedPaths.keys()].some((name) => guardedAt(name, open) && mentions(name, pushed));
        }
      }
      if (!hasPathLiteral(decl.text) && !derived && !walked && !collects) continue;
      record(guardedPaths, decl.name, regionOf(decl));
      grew = true;
    }
    if (!grew) break;
  }

  const normalized = (text) => [...normalizers].some((name) => new RegExp(`\\b${name}\\s*\\(`).test(text));

  // Taint travels by name too, so it is held to the same regions.
  const taint = (name, range) => record(tainted, name, range);
  const taintedAt = (name, index) => !shadowed(name, index) && within(tainted.get(name), index);

  // 2. Local helpers that do nothing but hand back raw bytes.
  const rawReaders = new Set();
  for (const decl of decls) {
    if (!/\breadFileSync\s*\(/.test(decl.code)) continue;
    if (!normalized(decl.code) && !guardedPaths.has(decl.name)) rawReaders.add(decl.name);
  }

  // 3. Every read of such a file, and whether its bytes reach a binding raw.
  const readCallee = new RegExp(`\\b(readFileSync|${[...rawReaders].join('|') || '\\0'})\\s*\\(`, 'g');
  const tainted = new Map();
  const tupleSlots = new Map(); // `<owner>\u0000<slot>` -> ranges in which that slot holds raw bytes
  let guardedReads = 0;
  for (let m = readCallee.exec(code); m !== null; m = readCallee.exec(code)) {
    const open = m.index + m[0].length - 1;
    const argsCode = code.slice(open + 1, closingBracket(code, open) - 1);
    const argsText = text.slice(open + 1, closingBracket(code, open) - 1);
    const guarded = hasPathLiteral(argsText)
      || [...guardedPaths.keys()].some(
        (name) => guardedAt(name, m.index)
          && new RegExp(`\\b${name.replace(/\./g, '\\s*\\.\\s*')}\\b`).test(argsCode),
      );
    if (!guarded) continue;
    guardedReads += 1;

    const enclosing = enclosingCalls(code, m.index);
    if (enclosing.some((name) => name !== null && normalizers.has(name.split('.').pop()))) continue;

    // The binding the raw bytes land in, reached through any number of
    // non-normalizing call wrappers.
    const prefix = code.slice(0, m.index);
    const bound = new RegExp(
      `(?:(const|let|var)\\s+)?(${IDENT})\\s*(?<![=!<>+\\-*/%&|^])=\\s*(?:${IDENT}(?:\\s*\\.\\s*${IDENT})*\\s*\\(\\s*)*$`,
    ).exec(prefix);
    if (bound) {
      taint(bound[2], bound[1] ? regionAt(m.index) : wholeFile);
      continue;
    }

    // The bytes land in a SLOT of an array literal instead of in a name:
    // `clientPaths.map((file) => [file, readFileSync(file, 'utf8')])` hands
    // back a pair whose second element is the program and whose first is the
    // path the failure messages quote. Nothing is bound here, so the taint has
    // to wait for whatever destructuring names that slot — and arrive on that
    // name alone.
    const ownerAt = (index) => decls
      .filter((d) => !d.iterated && !d.callable && !isArrow(d) && d.start <= index && index < d.end)
      .sort((a, b) => (a.end - a.start) - (b.end - b.start))[0];
    const slot = arraySlot(code, m.index);
    if (slot !== null) {
      const owner = ownerAt(slot.open);
      if (owner !== undefined) record(tupleSlots, `${owner.name}\u0000${slot.slot}`, regionOf(owner));
    }

    // The callback hands the bytes back whole rather than in a slot, so the
    // ARRAY is raw and so is whatever the chain makes of it: `readdirSync(dir)
    // .filter(…).map((name) => readFileSync(join(dir, name), 'utf8')).join('\\n')`
    // is one binding holding every guarded file in a directory, and not one
    // step of it spells a name this reader would otherwise connect to a read.
    // `openapi-route-coverage-contract-check.mjs` is written in exactly that
    // shape — through `productionRustCode`, which is why it is green, and why
    // nothing would have objected had it not been.
    //
    // A read that landed in a slot is NOT this shape: there the pair carries a
    // path in its other half, and tainting the array would put the raw-bytes
    // accusation on the name a failure message quotes.
    const innermost = enclosing[enclosing.length - 1];
    if (slot === null && innermost !== null && innermost !== undefined
      && ELEMENT_METHODS.includes(innermost.split('.').pop())) {
      const owner = ownerAt(m.index);
      if (owner !== undefined) taint(owner.name, regionOf(owner));
    }

    // Nothing bound the bytes, so no name can carry the taint into step 5 —
    // but a chain can assert on the spot: `stripRustComments(readFileSync(
    // metrics, 'utf8')).split(';')` reads, un-normalizes and greps in one
    // expression. Walk out through the closing parens of the wrappers and see
    // whether the value is asserted over right there. `observability-contract-
    // check.mjs` sat in this blind spot: a metric declared inside a
    // `#[cfg(test)]` module counted as an exported one.
    let after = closingBracket(code, open);
    while (after < code.length && /[\s)]/.test(code[after])) after += 1;
    const chained = new RegExp(`^\\.\\s*(${RAW_STRING_METHODS.join('|')})\\s*\\(`).exec(code.slice(after));
    if (chained) {
      problems.push(
        `${relative(root, file)}:${code.slice(0, m.index).split('\n').length}: the bytes of a `
          + `${lang.name} source file are read and \`.${chained[1]}(…)\` is applied to them without `
          + 'passing through a production view',
      );
    }
  }

  // 3b. A whole map of files read at once —
  // `Object.entries(files).map(([key, file]) => [key, readFileSync(file, 'utf8')])`.
  // Which path an entry came from is not visible at the read, so the taint
  // travels by key instead: `files.backend` names a `.rs` file, therefore so
  // does `source.backend` — and on the TypeScript pass `files.client` names a
  // `.ts` one, so the same map is read twice and each key is tainted by the
  // language it actually belongs to.
  for (const decl of decls) {
    if (!/\breadFileSync\s*\(/.test(decl.code) || normalized(decl.code)) continue;
    for (const [obj, keys] of guardedMembers) {
      if (!mentions(obj, decl.code)) continue;
      for (const key of keys) {
        taint(`${decl.name}.${key}`, regionOf(decl));
        guardedReads += 1;
      }
    }
  }

  // 3c. The slot resolved by the destructuring that names it. `for (const
  // [file, source] of clients)` and `clients.map(([file, source]) => …)` name
  // the same slots of the same tuples, so both arrive here as slot-bound
  // declarations, and only the slot a read actually left bytes in is tainted.
  for (const decl of decls) {
    if (decl.slot === undefined) continue;
    for (const [key, ranges] of tupleSlots) {
      const [owner, slot] = key.split('\u0000');
      if (Number(slot) !== decl.slot || !mentions(owner, decl.code)) continue;
      if (!within(ranges, decl.start)) continue;
      taint(decl.name, regionOf(decl));
    }
  }

  // 4. Taint follows a plain re-binding: `const body = backend.slice(…)`.
  for (let pass = 0; pass < 6; pass += 1) {
    let grew = false;
    for (const decl of decls) {
      if (taintedAt(decl.name, decl.start)) continue;
      if (normalized(decl.code)) continue;
      const direct = [...tainted.keys()].some((name) => taintedAt(name, decl.start) && new RegExp(
        `^\\s*${name.replace(/\./g, '\\s*\\.\\s*')}\\b`,
      ).test(decl.code));
      if (direct) {
        taint(decl.name, regionOf(decl));
        grew = true;
      }
    }
    if (!grew) break;
  }

  // 5. Textual assertions over the raw bytes.
  const lineOf = (index) => code.slice(0, index).split('\n').length;
  const report = (index, message) => {
    problems.push(`${relative(root, file)}:${lineOf(index)}: ${message}`);
  };
  // A helper the check declares itself is as much an assertion sink as one it
  // imports — unless it normalizes, which is the same test applied one level
  // closer to home.
  const localSinks = [...localCallables(decls)].filter(
    (name) => !normalizers.has(name) && !decls.some((d) => d.name === name && normalized(d.code)),
  );
  const nonNormalizing = [...new Set([...libFunctions, ...localSinks])].filter((name) => !normalizers.has(name));
  for (const name of tainted.keys()) {
    // `\b` alone would read `entry.name.startsWith('.')` as an assertion over a
    // tainted `name`: the word boundary sits happily after the dot. A tainted
    // bare name is never a property of something else.
    const pattern = `(?<![.\\w$])${name.replace(/\./g, '\\s*\\.\\s*')}`;
    const method = new RegExp(`${pattern}\\s*\\.\\s*(${RAW_STRING_METHODS.join('|')})\\s*\\(`, 'g');
    for (let m = method.exec(code); m !== null; m = method.exec(code)) {
      if (!taintedAt(name, m.index)) continue;
      report(m.index, `\`${name}\` holds the raw bytes of a ${lang.name} source file; \`.${m[1]}(…)\` asserts about text that is not necessarily part of the program`);
    }
    const applied = new RegExp(`\\.\\s*(test|exec)\\s*\\(\\s*${pattern}\\s*[,)]`, 'g');
    for (let m = applied.exec(code); m !== null; m = applied.exec(code)) {
      if (!taintedAt(name, m.index)) continue;
      report(m.index, `\`${name}\` holds the raw bytes of a ${lang.name} source file; a regex \`.${m[1]}()\` over it matches ${lang.skipped}`);
    }
    if (nonNormalizing.length > 0) {
      const passed = new RegExp(`\\b(${nonNormalizing.join('|')})\\s*\\(\\s*${pattern}\\s*[,)]`, 'g');
      for (let m = passed.exec(code); m !== null; m = passed.exec(code)) {
        if (!taintedAt(name, m.index)) continue;
        report(m.index, `\`${name}\` holds the raw bytes of a ${lang.name} source file and is handed to \`${m[1]}()\`, which asserts over whatever view it is given`);
      }
    }
  }

  return { problems, guardedReads };
}

const libFiles = (() => {
  try {
    return statSync(libDir).isDirectory() ? listScripts(libDir) : [];
  } catch {
    return [];
  }
})();

if (libFiles.length === 0) {
  console.error(`❌ raw source assertions: no ${relative(root, libDir)}/*.mjs — the normalizer set cannot be derived, so every read would look raw.`);
  process.exit(1);
}

const libDecls = [];
for (const file of libFiles) {
  const source = readFileSync(file, 'utf8');
  libDecls.push(...bindings(jsCodeView(source), jsTextView(source)));
}
const libWalkers = discoverWalkers(libDecls);

const libFunctions = new Set();
for (const file of libFiles) {
  const source = readFileSync(file, 'utf8');
  const code = jsCodeView(source);
  const exported = new RegExp(`\\bexport\\s+(?:async\\s+)?(?:function\\s+|const\\s+)(${IDENT})`, 'g');
  for (let m = exported.exec(code); m !== null; m = exported.exec(code)) libFunctions.add(m[1]);
}

// The mutation stands are the one family that reads a guarded file raw on
// purpose, and the one family where raw bytes cannot manufacture a false green.
// A stand copies the repository into a fixture, edits the bytes there —
// `observability-contract-check-regression.mjs` anchors an insertion on the
// YAML *comment* `      # Slow requests`, which no production view can offer —
// and then asserts on the exit code of the real check it drives, in both
// directions. If the anchor drifts, the stand throws; if the mutation lands
// somewhere the program never reads, the check under test stays green where the
// stand demands red, and the stand fails. Both failure modes are red, so there
// is nothing here for this ratchet to protect.
//
// The exemption is by suffix rather than by name, so it cannot become a list
// somebody parks a check in: `run-contract-checks.mjs` decides what a stand is
// with the same suffix.
const STAND_SUFFIX = '-contract-check-regression.mjs';
const subjects = listScripts(subjectDir)
  .filter((file) => !file.endsWith(STAND_SUFFIX))
  .map((file) => [file, readFileSync(file, 'utf8')]);

// The stand drives a fixture with a handful of files, so it sets its own floor
// — per language, because a fixture written to exercise one of them contains no
// reads of the other, and a single shared number would redden every such case
// for the wrong reason.
const standFloor = (lang) => (
  override ? Number(process.env[`FORGEKEEP_RAW_SOURCE_ASSERT_MIN_${lang.name.toUpperCase()}`] ?? 0) : lang.minReads
);

const summary = [];
let red = false;

for (const lang of LANGUAGES) {
  const normalizers = discoverNormalizers(libFiles, lang.seeds);
  const failures = [];
  let guardedReads = 0;
  for (const [file, source] of subjects) {
    const result = analyse(file, source, lang, normalizers, libFunctions, libWalkers);
    failures.push(...result.problems);
    guardedReads += result.guardedReads;
  }

  if (failures.length > 0) {
    console.error(`❌ contract checks assert over raw ${lang.name} source:`);
    for (const failure of failures) console.error(`   - ${failure}`);
    console.error(`\n${lang.remedy}\n   A raw grep is satisfied by ${lang.skipped}.`);
    red = true;
    continue;
  }

  const minReads = standFloor(lang);
  if (guardedReads < minReads) {
    console.error(
      `❌ raw ${lang.name} assertions: only ${guardedReads} read(s) of a ${lang.extensions.join(' / ')} file `
        + `were recognised across scripts/ (expected at least ${minReads}).\n`
        + '   The reader has gone blind to the corpus it guards — fix the path recognition in\n'
        + '   scripts/raw-source-assertion-contract-check.mjs rather than lowering the floor.',
    );
    red = true;
    continue;
  }

  summary.push(`${guardedReads} ${lang.extensions.join(' / ')}`);
}

if (red) process.exit(1);

console.log(`✅ raw source assertions: ${summary.join(' and ')} read(s) across scripts/ all reach an assertion through a production view`);
