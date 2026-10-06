#!/usr/bin/env node

// A route that reads its body as a stream under a declared ceiling must answer
// `413` when that ceiling trips.
//
// `body-limit-declaration-contract-check.mjs` is the other half of this phase's
// contract and it stops one step short: it proves a ceiling was *declared* —
// the route is mounted through a `Wrap` that reaches `apply_body_limit` — and
// says nothing about what the client is told when the ceiling refuses a body.
// The two halves fail differently. A missing declaration is a route running on
// Axum's 2 MiB default; a missing verdict is a route whose own OpenAPI entry
// promises `413` while the handler answers `400` ("failed to read body") or
// `500`, so an upload that was refused for a knowable, documented reason looks
// to the client like a transport fault it should retry.
//
// The reason this file exists rather than a tenth hand-written test: every
// instance of the class so far was found by somebody reading handlers.
// `git_http.rs`, `attachments.rs`, `oci.rs`, `lfs.rs`, `releases.rs`,
// `packages.rs`, `runners.rs` and finally `artifacts.rs` (card_134c43fa6089) —
// nine of them, and `grep -rn '413\|payload_too_large' scripts/` matched
// nothing in this directory the whole time. The tenth would have arrived
// exactly as quietly, which is what "the criterion is met" meant here: met on
// the day somebody last looked.
//
// ── What it reads, and where the line is ───────────────────────────────────
//
// A *read site* is one of the three ways this tree pulls bytes out of a request
// body without buffering it whole: `Body::into_data_stream()`, and multipart's
// `next_field()` / `Field::chunk()`. Those are the calls that can hand back the
// `LengthLimitError` that `RequestBodyLimitLayer` raises, so they are exactly
// the places where a ceiling becomes an error somebody has to name. A buffered
// extractor (`Bytes`, `String`, `Multipart` as a whole) never reaches a handler
// at all when it trips — Axum's own rejection answers, and the declaration
// check is what covers those routes.
//
// A read site is *covered* when the function holding it classifies a body error
// through `crate::body_limit::is_length_limit_error` — directly, or through a
// helper it calls (`oci_body_error`, `lfs_body_error`, `attachment_body_error`,
// `package_multipart_error`). The helpers are discovered, not listed: the
// classifying set is seeded with `is_length_limit_error` and closed over every
// production function that calls something already in it. A list would be the
// same defect one level up — a tenth path could add a tenth classifier and this
// file would have to be edited for the gate to see it.
//
// Some functions stream but deliberately do not classify: `write_body_to_file`
// in `lfs.rs` and `upload_body_is_empty` in `oci.rs` preserve the typed
// `axum::Error` as an `anyhow` source precisely so their caller can classify it
// once, for every path into them. Demanding the classification at the read site
// would push those trees to duplicate it. So coverage also passes upward: a
// function that does not classify is covered when it has production callers and
// *every* one of them is covered. Every one, not any: a second caller that
// swallows the error is the hole, and "some caller handles it" is how a gate
// stops noticing that.
//
// ── The verdict half ───────────────────────────────────────────────────────
//
// Reaching the classifier is not the contract; answering `413` is. So every
// `is_length_limit_error` guard in the tree is read as well, and the branch it
// guards must produce a payload-too-large response — `AppError::payload_too_large`
// or `StatusCode::PAYLOAD_TOO_LARGE`, directly or through a `let` binding in the
// same function (`artifacts.rs` builds its refusal as an `over_ceiling` closure
// so the ceiling's byte count is written once). Without this half the exact
// mutation that proved card_134c43fa6089 — `stream_to_file` returning
// `bad_request` on a length-limit trip — keeps the classifier call and stays
// green.
//
// Truth boundary: everything is read through `productionRustCode`, so a
// classifier that exists only in a comment or only inside a `#[cfg(test)]`
// fixture reads as absent — see card_d67b6f433341.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import { productionRustCode } from './lib/rust-source.mjs';

const root = process.cwd();
const CRATES = path.join(root, 'crates');
const HTTP_SRC = path.join(CRATES, 'rg-http', 'src');

const failures = [];

// ── The reads that can surface a tripped ceiling ───────────────────────────
//
// Matched on the call, not on a type: `into_data_stream` is the only way an
// Axum `Body` becomes a stream, and `next_field` / `chunk` are the two ways a
// `Multipart` is consumed incrementally. Adding a fourth spelling here is a
// one-line change; the floor below is what makes a spelling silently falling
// out of this list red rather than invisible.
//
// `immediate` says where the error appears. `next_field` and `chunk` hand it
// back at the call, so the call site itself is what has to classify it, and
// asking the enclosing function instead is too coarse: `decode_twine_upload`
// classifies four multipart errors, and losing the one on `next_field` left it
// still "a function that classifies". `into_data_stream` is the other shape —
// it returns a stream and the error arrives later from `stream.next()`, in the
// loop, so there the function is the right unit.
const READ_SITES = [
  { name: 'Body::into_data_stream', re: /\.into_data_stream\s*\(/g, immediate: false },
  { name: 'Multipart::next_field', re: /\.next_field\s*\(/g, immediate: true },
  { name: 'Field::chunk', re: /\.chunk\s*\(/g, immediate: true },
];

// The seed of the classifying set. Everything else that classifies is found by
// closing over the call graph from here.
const CLASSIFIER = 'is_length_limit_error';
const CLASSIFIER_FILE = path.join(HTTP_SRC, 'body_limit.rs');

// What "answers 413" looks like in this tree, in both of its dialects: the
// `AppError` constructor and the raw status the OCI and git-http paths build
// their own envelopes around.
const TOO_LARGE = ['payload_too_large', 'PAYLOAD_TOO_LARGE'];

// Floors. A parser that stopped understanding its subject reports a clean tree:
// with no read sites recognised, every path is trivially covered, and with no
// classifiers recognised the verdict half asserts nothing. The tree has 12
// production read sites and 9 classifier guards today; the floors sit one under
// each, so retiring one path does not trip them while any of the three
// spellings silently falling out of `READ_SITES` does.
const MIN_READ_SITES = 11;
const MIN_CLASSIFIER_GUARDS = 8;

/** Every `.rs` file under `dir`, depth-first, sorted so verdicts are stable. */
function rustFiles(dir) {
  const found = [];
  for (const entry of readdirSync(dir).sort()) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) found.push(...rustFiles(full));
    else if (entry.endsWith('.rs')) found.push(full);
  }
  return found;
}

/**
 * The production view of one `.rs` file.
 *
 * Everything below reads this and never the bytes: a classifier that survives
 * only in a comment, or a handler that exists only inside a `#[cfg(test)]`
 * fixture, has to read as absent (card_d67b6f433341). String contents are
 * blanked too, which costs nothing here — this check reads names and calls, and
 * a `413` spelled inside a log message was never evidence of one being sent.
 */
function views(file) {
  return { code: productionRustCode(readFileSync(file, 'utf8')) };
}

/**
 * Every top-level `fn` in a production code view, with the span of its body.
 *
 * `rustFnBlock` in the shared library reads `pub async fn` only, and half the
 * functions this gate cares about are private helpers (`fn lfs_body_error`,
 * `async fn upload_body_is_empty`). Same anchoring rule as that reader: the
 * declaration is at column 0 and rustfmt puts the closing brace at column 0,
 * which is what the tree's `cargo fmt` gate keeps true.
 */
function topLevelFns(code) {
  const fns = [];
  const declaration = /^(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)/gm;
  let match;
  while ((match = declaration.exec(code)) !== null) {
    let depth = 0;
    let open = -1;
    for (let i = match.index + match[0].length; i < code.length; i += 1) {
      const ch = code[i];
      if (ch === '(') depth += 1;
      else if (ch === ')') depth -= 1;
      else if (ch === '{' && depth === 0) {
        open = i;
        break;
      } else if (ch === ';' && depth === 0) break; // a trait method with no body
    }
    if (open < 0) continue;
    let braces = 0;
    let end = -1;
    for (let i = open; i < code.length; i += 1) {
      if (code[i] === '{') braces += 1;
      else if (code[i] === '}') {
        braces -= 1;
        if (braces === 0) {
          end = i + 1;
          break;
        }
      }
    }
    if (end < 0) continue;
    fns.push({ name: match[1], start: open, end });
  }
  return fns;
}

/** The function whose body holds `index`, or `null` when it sits outside one. */
function ownerOf(fns, index) {
  let owner = null;
  for (const fn of fns) {
    if (index >= fn.start && index < fn.end) {
      // Innermost wins: a nested `fn` declared inside another is still at
      // column 0 nowhere in this tree, but taking the tightest span costs
      // nothing and is right if one ever is.
      if (owner === null || fn.start > owner.start) owner = fn;
    }
  }
  return owner;
}

/**
 * Free-function calls made in `body`, by bare name.
 *
 * Two exclusions, and both were holes before they were exclusions. A name
 * preceded by `.` is a *method* — `file.create(…)`, `stream.chunk(…)` — and a
 * name preceded by `::` belongs to another path — `tokio::fs::File::create(…)`.
 * Counting either as an edge to a same-named free function in this crate is how
 * `create`, `run` and `publish` joined the classifying set: `write_body_to_file`
 * came out "classifying" because it calls `tokio::fs::File::create`, and from
 * there the closure swallowed most of the crate and every mutation below went
 * green. A bare identifier at a call position is the only shape this reader can
 * honestly call an edge.
 */
function callsIn(body) {
  const names = new Set();
  const call = /(^|[^.:\w])([a-z_][A-Za-z0-9_]*)\s*\(/g;
  let match;
  while ((match = call.exec(body)) !== null) {
    names.add(match[2]);
    call.lastIndex -= 1; // a match consumes the separator the next one may need
  }
  return names;
}

// ── Load the crate ─────────────────────────────────────────────────────────

const files = rustFiles(HTTP_SRC);
/**
 * file → (name → [record]). The call graph is deliberately per FILE, not
 * crate-wide: a bare `foo(…)` in Rust resolves to something in scope here, and
 * every classifier chain this tree actually has is intra-file
 * (`attachment_body_error` in `attachments.rs`, `oci_body_error` in `oci.rs`).
 * A crate-wide name index turns two unrelated `fn create` into one node, which
 * is the same over-reach `callsIn` documents from the other side. A helper
 * reached across files is spelled with a module path and so is not an edge here
 * — it reads as uncovered, which is a question rather than a false green.
 */
const byFile = new Map();
const loaded = [];

for (const file of files) {
  const { code } = views(file);
  const fns = topLevelFns(code);
  const entry = { file, code, fns };
  loaded.push(entry);
  const index = new Map();
  for (const fn of fns) {
    const record = { ...fn, file, body: code.slice(fn.start, fn.end) };
    if (!index.has(fn.name)) index.set(fn.name, []);
    index.get(fn.name).push(record);
  }
  byFile.set(file, index);
}

const relative = (file) => path.relative(root, file).split(path.sep).join('/');
const lineOf = (code, index) => code.slice(0, index).split('\n').length;

// ── Who classifies a body error ────────────────────────────────────────────
//
// Seeded with the functions that name the classifier outright, then closed
// over the per-file call graph: a function bare-calling something already in
// the set classifies too.

// The classifier is reached by its full path (`crate::body_limit::…`), so
// "classifies directly" is the name appearing in the body at all; everything
// else is a bare same-file call to something already in the set.
const classifying = new Set();
const key = (file, name) => `${file}::${name}`;

for (const [file, index] of byFile) {
  for (const [name, records] of index) {
    if (file === CLASSIFIER_FILE && name === CLASSIFIER) continue;
    if (records.some((record) => record.body.includes(CLASSIFIER))) {
      classifying.add(key(file, name));
    }
  }
}
for (let changed = true; changed; ) {
  changed = false;
  for (const [file, index] of byFile) {
    for (const [name, records] of index) {
      if (classifying.has(key(file, name))) continue;
      const reaches = records.some((record) =>
        [...callsIn(record.body)].some((called) => classifying.has(key(file, called))),
      );
      if (reaches) {
        classifying.add(key(file, name));
        changed = true;
      }
    }
  }
}

const classifierIndex = byFile.get(CLASSIFIER_FILE);
if (classifierIndex === undefined || !classifierIndex.has(CLASSIFIER)) {
  failures.push(
    `crates/rg-http/src/body_limit.rs: \`${CLASSIFIER}\` is gone from the production source — ` +
      'it is what every 413 on this list is decided by, and without it this check has nothing ' +
      'to close the classifying set over',
  );
} else {
  // The classifier has to keep doing the one thing that makes it work.
  // `RequestBodyLimitLayer` hands back a `LengthLimitError` nested inside an
  // `axum::Error`, so a direct downcast on the outer error never matches: drop
  // the `source()` walk and every path below keeps its call, keeps its 413
  // branch, and answers 400 for every body that trips a ceiling.
  const body = classifierIndex.get(CLASSIFIER)[0].body;
  for (const half of ['LengthLimitError', 'source()']) {
    if (!body.includes(half)) {
      failures.push(
        `crates/rg-http/src/body_limit.rs: \`${CLASSIFIER}\` no longer mentions \`${half}\`, so ` +
          'it cannot recognise a nested length-limit trip — every caller would keep its 413 ' +
          'branch and never take it',
      );
    }
  }
}

// ── Coverage: every read site reaches a classifier ─────────────────────────

const sites = [];
for (const entry of loaded) {
  for (const site of READ_SITES) {
    site.re.lastIndex = 0;
    let match;
    while ((match = site.re.exec(entry.code)) !== null) {
      const owner = ownerOf(entry.fns, match.index);
      sites.push({
        kind: site.name,
        immediate: site.immediate,
        file: entry.file,
        line: lineOf(entry.code, match.index),
        owner,
        at: match.index,
        ownerBody: owner === null ? '' : entry.code.slice(owner.start, owner.end),
      });
    }
  }
}

if (sites.length < MIN_READ_SITES) {
  failures.push(
    `only ${sites.length} streaming body read(s) were recognised in rg-http (expected at least ` +
      `${MIN_READ_SITES}). Either the reads moved to a spelling READ_SITES does not match, or ` +
      'the function reader went quiet — a gate that sees no reads reports every path as ' +
      'covered. If these paths really were retired, lower the floor on purpose',
  );
}

/** Balanced closer for the opener at `open`, or `null`. */
function balanced(code, open, opener, closer) {
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === opener) depth += 1;
    else if (code[i] === closer) {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return null;
}

/**
 * What happens to the error of the call whose name starts at `index`.
 *
 * Three shapes, and the distinction is the whole point of reading call sites
 * rather than functions. `match stream_body_to_file(…).await { … }` HANDLES the
 * error right there, so the arms are what has to classify; `stage_archive_file(…)?`
 * PROPAGATES it, so the question moves up to this function's own callers; a
 * plain statement neither, which reads as unhandled.
 *
 * Function-level granularity cannot tell the first two apart, and the gap is
 * not theoretical: `complete_upload` in `oci.rs` classifies three body errors
 * and consumes a fourth. Asking only "does the caller classify somewhere" let
 * the fourth arm answer `400` for a tripped ceiling with this gate green.
 */
function callDisposition(code, index) {
  const open = code.indexOf('(', index);
  if (open < 0) return { kind: 'unhandled', region: '' };
  const close = balanced(code, open, '(', ')');
  if (close === null) return { kind: 'unhandled', region: '' };

  // Walk the postfix chain — `.await`, `.map(…)`, `.into_response()` — to
  // whatever decides the error's fate.
  let i = close + 1;
  for (;;) {
    while (i < code.length && /\s/.test(code[i])) i += 1;
    if (code[i] !== '.') break;
    let j = i + 1;
    while (j < code.length && /[A-Za-z0-9_]/.test(code[j])) j += 1;
    while (j < code.length && /\s/.test(code[j])) j += 1;
    if (code[j] === '(') {
      const inner = balanced(code, j, '(', ')');
      if (inner === null) break;
      j = inner + 1;
    }
    i = j;
  }

  // The chain itself is part of the answer: `.map_err(|error|
  // package_multipart_error(error, …))?` classifies and then propagates, and
  // reading only what follows the `?` would miss it.
  const chain = code.slice(close + 1, i);
  if (code[i] === '?') return { kind: 'propagated', region: chain };
  if (code[i] === '{') {
    const blockEnd = balanced(code, i, '{', '}');
    if (blockEnd === null) return { kind: 'unhandled', region: chain };
    return { kind: 'handled', region: chain + code.slice(i, blockEnd + 1) };
  }
  // Anything else: read to the end of the enclosing expression, so a
  // `.map_err(|error| lfs_body_error(error))` chain is still seen.
  let depth = 0;
  for (let j = close + 1; j < code.length; j += 1) {
    const ch = code[j];
    if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') {
      if (depth === 0) return { kind: 'handled', region: code.slice(close + 1, j) };
      depth -= 1;
    } else if ((ch === ';' || ch === ',') && depth === 0) {
      return { kind: 'handled', region: code.slice(close + 1, j) };
    }
  }
  return { kind: 'unhandled', region: chain };
}

/** Whether `region` names something that classifies a body error. */
function regionClassifies(file, region) {
  if (region.includes(CLASSIFIER)) return true;
  return [...callsIn(region)].some((called) => classifying.has(key(file, called)));
}

/** Every bare `name(` call inside another top-level fn of `file`. */
function callSitesOf(file, name) {
  const index = byFile.get(file);
  const sites = [];
  for (const [caller, records] of index) {
    if (caller === name) continue;
    for (const record of records) {
      const call = new RegExp(`(^|[^.:\\w])${name}\\s*\\(`, 'g');
      let match;
      while ((match = call.exec(record.body)) !== null) {
        sites.push({ caller, at: record.start + match.index + match[1].length });
      }
    }
  }
  return sites;
}

/**
 * Whether the error a call to `name` yields is classified before it reaches a
 * client, on EVERY path out of it.
 *
 * Every path, not some: a second call site that swallows the error is exactly
 * the hole, and "one caller handles it" is how a gate stops noticing that. A
 * function nobody calls reads as unhandled — an unreachable streaming helper
 * proves nothing about the routes that are mounted. `seen` breaks recursion,
 * because a cycle is not evidence.
 */
function escapeHandled(file, name, seen = new Set()) {
  if (seen.has(name)) return false;
  seen.add(name);
  const code = loaded.find((entry) => entry.file === file).code;
  const sites = callSitesOf(file, name);
  if (sites.length === 0) return false;
  return sites.every(({ caller, at }) => {
    const disposition = callDisposition(code, at);
    if (disposition.kind === 'handled') return regionClassifies(file, disposition.region);
    if (disposition.kind === 'propagated') return escapeHandled(file, caller, seen);
    return false;
  });
}

for (const site of sites) {
  if (site.owner === null) {
    failures.push(
      `${relative(site.file)}:${site.line}: this \`${site.kind}\` call sits outside any top-level ` +
        'function this check can read, so nothing here can tell whether its body errors are ' +
        'classified — fix the reader rather than skipping the site',
    );
    continue;
  }
  // A multipart read is answered at the call: its error is the call's value.
  // A stream read is answered by the function that drives the stream, or by
  // every caller it hands the error to.
  const code = loaded.find((entry) => entry.file === site.file).code;
  const handled = site.immediate
    ? regionClassifies(site.file, callDisposition(code, site.at).region)
    : regionClassifies(site.file, site.ownerBody) ||
      escapeHandled(site.file, site.owner.name);
  if (!handled) {
    const where = site.immediate
      ? `this \`${site.kind}\` call's error is not handed to `
        + `\`crate::body_limit::${CLASSIFIER}\` or to a helper that reaches it`
      : `\`${site.owner.name}\` streams the request body (\`${site.kind}\`) but neither it nor `
        + `every caller it hands the error to reaches \`crate::body_limit::${CLASSIFIER}\``;
    failures.push(
      `${relative(site.file)}:${site.line}: ${where}. A body refused by the ceiling this route ` +
        'declares would be reported as an ordinary transport failure — 400 or 500 for a limit ' +
        'the route documents as 413. Classify the error where it is read, or hand it to a ' +
        'caller that does',
    );
  }
}

// ── Verdict: a recognised length-limit trip answers 413 ────────────────────

/** The `{ … }` block guarded by the `if` whose condition holds `index`. */
function guardedBlock(code, index) {
  let depth = 0;
  for (let i = index; i < code.length; i += 1) {
    const ch = code[i];
    if (ch === '(') depth += 1;
    else if (ch === ')') depth -= 1;
    else if (ch === '{' && depth <= 0) {
      let braces = 0;
      for (let j = i; j < code.length; j += 1) {
        if (code[j] === '{') braces += 1;
        else if (code[j] === '}') {
          braces -= 1;
          if (braces === 0) return code.slice(i, j + 1);
        }
      }
      return null;
    }
  }
  return null;
}

/** `let over_ceiling = || { … };` in `body` → the initializer, or `null`. */
function bindingInitializer(body, name) {
  const declaration = new RegExp(`\\blet\\s+${name}\\s*(?::[^=;]*)?=`).exec(body);
  if (declaration === null) return null;
  let depth = 0;
  for (let i = declaration.index + declaration[0].length; i < body.length; i += 1) {
    const ch = body[i];
    if (ch === '(' || ch === '{' || ch === '[') depth += 1;
    else if (ch === ')' || ch === '}' || ch === ']') depth -= 1;
    else if (ch === ';' && depth === 0) {
      return body.slice(declaration.index, i);
    }
  }
  return null;
}

/** Whether `block` produces a payload-too-large answer, directly or via a `let`. */
function answersTooLarge(block, fnBody) {
  if (TOO_LARGE.some((marker) => block.includes(marker))) return true;
  const identifier = /\b([a-z_][A-Za-z0-9_]*)\b/g;
  let match;
  while ((match = identifier.exec(block)) !== null) {
    const initializer = bindingInitializer(fnBody, match[1]);
    if (initializer !== null && TOO_LARGE.some((marker) => initializer.includes(marker))) {
      return true;
    }
  }
  return false;
}

let guards = 0;
for (const entry of loaded) {
  const guard = new RegExp(`\\b${CLASSIFIER}\\s*\\(`, 'g');
  let match;
  while ((match = guard.exec(entry.code)) !== null) {
    const owner = ownerOf(entry.fns, match.index);
    if (owner === null || owner.name === CLASSIFIER) continue;
    guards += 1;
    const line = lineOf(entry.code, match.index);
    const block = guardedBlock(entry.code, match.index);
    if (block === null) {
      failures.push(
        `${relative(entry.file)}:${line}: the branch guarded by \`${CLASSIFIER}\` cannot be read, ` +
          'so what this path answers on a tripped ceiling is unknown — fix the reader rather ' +
          'than passing over the guard',
      );
      continue;
    }
    if (!answersTooLarge(block, entry.code.slice(owner.start, owner.end))) {
      failures.push(
        `${relative(entry.file)}:${line}: \`${owner.name}\` recognises a tripped body ceiling and ` +
          'then answers with something other than 413. Recognising the limit and reporting it as ' +
          'a client or transport error is the same lie as not recognising it — return ' +
          '`AppError::payload_too_large` (or `StatusCode::PAYLOAD_TOO_LARGE`) on this branch',
      );
    }
  }
}

if (guards < MIN_CLASSIFIER_GUARDS) {
  failures.push(
    `only ${guards} \`${CLASSIFIER}\` guard(s) were found outside the classifier itself (expected ` +
      `at least ${MIN_CLASSIFIER_GUARDS}). A verdict half that reads no guards asserts nothing ` +
      'about what any path answers. If a path really stopped classifying, the coverage half ' +
      'above should have said so first — check that verdict before lowering this floor',
  );
}

// ── The subject boundary, held rather than assumed ─────────────────────────
//
// Everything above reads `rg-http` because that is where Axum lives today. That
// is a fact about the tree, not a law: the day a second crate takes an Axum
// `Body`, this gate would go on reporting a clean tree while a whole new set of
// streaming paths sits outside its subject. So the boundary is asserted.
//
// A crate can only take an Axum `Body` or `Multipart` if it depends on Axum, so
// that is what decides whether its reads are in question. Matching the calls
// alone also caught `reqwest::Response::chunk` — an *outbound* download, such
// as the LFS objects an import fetches from its source — which no request-body
// ceiling governs. A crate whose manifest cannot be read is scanned anyway:
// not knowing its dependencies is not knowing it is safe.

/** Whether `crate` declares an Axum dependency, or cannot say. */
function mayTakeAxumBodies(crate) {
  let manifest;
  try {
    manifest = readFileSync(path.join(CRATES, crate, 'Cargo.toml'), 'utf8');
  } catch {
    return true;
  }
  return /^\s*axum\s*(=|\.)/m.test(manifest);
}

for (const crate of readdirSync(CRATES).sort()) {
  if (crate === 'rg-http') continue;
  if (!mayTakeAxumBodies(crate)) continue;
  const src = path.join(CRATES, crate, 'src');
  let entries;
  try {
    entries = rustFiles(src);
  } catch {
    continue;
  }
  for (const file of entries) {
    const { code } = views(file);
    for (const site of READ_SITES) {
      site.re.lastIndex = 0;
      const match = site.re.exec(code);
      if (match === null) continue;
      failures.push(
        `${relative(file)}:${lineOf(code, match.index)}: \`${site.name}\` is called outside ` +
          '`rg-http`, which is the only crate this check reads. Either move the streaming read ' +
          'back behind the HTTP boundary, or widen this check\'s subject — leaving it here means ' +
          'a request-body ceiling with nothing asserting what it answers',
      );
    }
  }
}

if (failures.length > 0) {
  console.error('❌ body limit status contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ body limit status contract: all ${sites.length} streaming body reads in rg-http reach a ` +
    `length-limit classifier, and all ${guards} classifier guards answer 413`,
);
