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
// Two languages are guarded, and they are guarded for the same reason rather
// than by analogy. Both halves of a frontend/backend contract assert the same
// fact about the same wire, so a hole in one half is a hole in the contract:
// commenting out the line in `web/src/lib/api/packages.ts` that sets
// `Content-Disposition` left `package-publish-contract-check.mjs` green over a
// header the client no longer sends, months after the Rust half of that very
// check had been hardened (card_a54b6a2db9f2). The `LANGUAGES` table below is
// what keeps the two halves in step: adding a language is a row, not a fork.
//
// The subject is a GLOB over `scripts/**/*.mjs` — the checks, the stands and
// the shared libraries alike. A hand-written subject list would be the same
// defect one level up.
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
// Truth boundary, stated because it decides how to read a green run. This is a
// lexical reader, not a JS interpreter: a path assembled at runtime from pieces
// no literal spells out (a directory walk filtered on `.endsWith('.svelte')` is
// the real case) is not recognised as one, and a check built that way is not
// covered — `card_2a23d37a583c` tracks that gap. The per-language `minReads`
// floors below keep it from widening in silence: if a refactor moves the paths
// out of reach of this reader, the recognised-read count collapses and the
// check goes red rather than passing over a corpus it can no longer see.

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
    seeds: ['productionRustCode', 'productionRustSource'],
    // The floor: 39 reads are recognised on `main` today. Raise it when the
    // corpus grows; never lower it to make a red run go away.
    minReads: 35,
    skipped: 'commented-out and `#[cfg(test)]` code the server never ships',
    remedy: '   Read the file through `scripts/lib/rust-source.mjs` instead — `productionRustSource()` for a\n'
      + '   whole-file view, `rustFnBlock()` / `rustStructBody()` / `parseRouteTable()` for one declaration.',
  },
  {
    name: 'TypeScript',
    extensions: ['.ts', '.svelte'],
    seeds: ['productionTsCode', 'productionTsSource'],
    // 92 reads are recognised on `main` today.
    minReads: 83,
    skipped: 'commented-out code the browser never runs',
    remedy: '   Read the file through `scripts/lib/ts-source.mjs` instead — `productionTsSource()` for a\n'
      + '   whole-file view, `tsInterfaceBody()` / `tsFunctionBody()` for one declaration.',
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
    `(?:^|[\\s;{}()])(?:(?:const|let|var)\\s+)?(${IDENT})\\s*(?<![=!<>+\\-*/%&|^])=(?!=)`,
    'g',
  );
  for (let m = assign.exec(code); m !== null; m = assign.exec(code)) {
    const start = m.index + m[0].length;
    const end = statementEnd(code, start);
    found.push({ name: m[1], start, end, code: code.slice(start, end), text: text.slice(start, end) });
  }
  const fn = new RegExp(`(?:^|[\\s;{}()])(?:export\\s+)?(?:async\\s+)?function\\s+(${IDENT})\\s*\\(`, 'g');
  for (let m = fn.exec(code); m !== null; m = fn.exec(code)) {
    const paren = code.indexOf('(', m.index + m[0].length - 1);
    const brace = code.indexOf('{', closingBracket(code, paren));
    if (brace < 0) continue;
    const end = closingBracket(code, brace);
    found.push({ name: m[1], start: m.index, end, code: code.slice(m.index, end), text: text.slice(m.index, end), callable: true });
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

function analyse(file, source, lang, normalizers, libFunctions) {
  const hasPathLiteral = literalPredicate(lang);
  const code = jsCodeView(source);
  const text = jsTextView(source);
  const decls = bindings(code, text);
  const problems = [];

  // 1. Which names hold, or lead to, a path to a file of this language.
  //
  // A path propagates only through path arithmetic (`join`, `resolve`). It must
  // not propagate through a read: `readFileSync(backendPath, …)` yields the
  // file's *contents*, and treating those as another path would make every
  // later binding that mentions them look like a source file of its own.
  const guardedPaths = new Set();
  const guardedMembers = new Map(); // object name -> Set of keys holding such a path
  const mentions = (name, haystack) => new RegExp(`\\b${name.replace(/\./g, '\\s*\\.\\s*')}\\b`).test(haystack);
  for (let pass = 0; pass < 6; pass += 1) {
    let grew = false;
    for (const decl of decls) {
      if (guardedPaths.has(decl.name)) continue;
      const isObject = /^\s*\{/.test(decl.code);
      if (hasPathLiteral(decl.text) && isObject) {
        // Only the matching properties are paths; the object as a whole is a
        // mixed bag of client, page and backend files, and each language's pass
        // picks out its own.
        for (const { key, value } of objectProperties(decl.code, decl.text)) {
          if (!hasPathLiteral(value) || guardedPaths.has(`${decl.name}.${key}`)) continue;
          guardedPaths.add(`${decl.name}.${key}`);
          if (!guardedMembers.has(decl.name)) guardedMembers.set(decl.name, new Set());
          guardedMembers.get(decl.name).add(key);
          grew = true;
        }
        continue;
      }
      const derived = [...guardedPaths].some((name) => mentions(name, decl.code)) && pathArithmeticOnly(decl.code);
      if (!hasPathLiteral(decl.text) && !derived) continue;
      guardedPaths.add(decl.name);
      grew = true;
    }
    if (!grew) break;
  }

  const normalized = (text) => [...normalizers].some((name) => new RegExp(`\\b${name}\\s*\\(`).test(text));

  // 2. Local helpers that do nothing but hand back raw bytes.
  const rawReaders = new Set();
  for (const decl of decls) {
    if (!/\breadFileSync\s*\(/.test(decl.code)) continue;
    if (!normalized(decl.code) && !guardedPaths.has(decl.name)) rawReaders.add(decl.name);
  }

  // 3. Every read of such a file, and whether its bytes reach a binding raw.
  const readCallee = new RegExp(`\\b(readFileSync|${[...rawReaders].join('|') || '\\0'})\\s*\\(`, 'g');
  const tainted = new Set();
  let guardedReads = 0;
  for (let m = readCallee.exec(code); m !== null; m = readCallee.exec(code)) {
    const open = m.index + m[0].length - 1;
    const argsCode = code.slice(open + 1, closingBracket(code, open) - 1);
    const argsText = text.slice(open + 1, closingBracket(code, open) - 1);
    const guarded = hasPathLiteral(argsText)
      || [...guardedPaths].some((name) => new RegExp(`\\b${name.replace(/\./g, '\\s*\\.\\s*')}\\b`).test(argsCode));
    if (!guarded) continue;
    guardedReads += 1;

    const enclosing = enclosingCalls(code, m.index);
    if (enclosing.some((name) => name !== null && normalizers.has(name.split('.').pop()))) continue;

    // The binding the raw bytes land in, reached through any number of
    // non-normalizing call wrappers.
    const prefix = code.slice(0, m.index);
    const bound = new RegExp(
      `(?:(?:const|let|var)\\s+)?(${IDENT})\\s*(?<![=!<>+\\-*/%&|^])=\\s*(?:${IDENT}(?:\\s*\\.\\s*${IDENT})*\\s*\\(\\s*)*$`,
    ).exec(prefix);
    if (bound) {
      tainted.add(bound[1]);
      continue;
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
        tainted.add(`${decl.name}.${key}`);
        guardedReads += 1;
      }
    }
  }

  // 4. Taint follows a plain re-binding: `const body = backend.slice(…)`.
  for (let pass = 0; pass < 6; pass += 1) {
    let grew = false;
    for (const decl of decls) {
      if (tainted.has(decl.name)) continue;
      if (normalized(decl.code)) continue;
      const direct = [...tainted].some((name) => new RegExp(
        `^\\s*${name.replace(/\./g, '\\s*\\.\\s*')}\\b`,
      ).test(decl.code));
      if (direct) {
        tainted.add(decl.name);
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
  for (const name of tainted) {
    const pattern = name.replace(/\./g, '\\s*\\.\\s*');
    const method = new RegExp(`\\b${pattern}\\s*\\.\\s*(${RAW_STRING_METHODS.join('|')})\\s*\\(`, 'g');
    for (let m = method.exec(code); m !== null; m = method.exec(code)) {
      report(m.index, `\`${name}\` holds the raw bytes of a ${lang.name} source file; \`.${m[1]}(…)\` asserts about text that is not necessarily part of the program`);
    }
    const applied = new RegExp(`\\.\\s*(test|exec)\\s*\\(\\s*${pattern}\\s*[,)]`, 'g');
    for (let m = applied.exec(code); m !== null; m = applied.exec(code)) {
      report(m.index, `\`${name}\` holds the raw bytes of a ${lang.name} source file; a regex \`.${m[1]}()\` over it matches ${lang.skipped}`);
    }
    if (nonNormalizing.length > 0) {
      const passed = new RegExp(`\\b(${nonNormalizing.join('|')})\\s*\\(\\s*${pattern}\\s*[,)]`, 'g');
      for (let m = passed.exec(code); m !== null; m = passed.exec(code)) {
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

const libFunctions = new Set();
for (const file of libFiles) {
  const source = readFileSync(file, 'utf8');
  const code = jsCodeView(source);
  const exported = new RegExp(`\\bexport\\s+(?:async\\s+)?(?:function\\s+|const\\s+)(${IDENT})`, 'g');
  for (let m = exported.exec(code); m !== null; m = exported.exec(code)) libFunctions.add(m[1]);
}

const subjects = listScripts(subjectDir).map((file) => [file, readFileSync(file, 'utf8')]);

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
    const result = analyse(file, source, lang, normalizers, libFunctions);
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
