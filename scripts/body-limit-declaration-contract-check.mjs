#!/usr/bin/env node

// A route whose handler buffers an opaque body must declare the ceiling it
// buffers up to.
//
// The defect this hunts is silence, not a wrong number. `Bytes`, `String` and
// `Multipart` read the whole body into memory before the handler starts, and a
// route that layers nothing onto them does not get "no limit" — it inherits
// Axum's `DefaultBodyLimit` of 2 MiB. That number belongs to Axum's defaults,
// not to this server's contract, and it is invisible at both ends: nothing in
// `routes.rs` mentions it, and the client is told only that its body was too
// large. Which way it hurts depends on the route, and this phase has now seen
// both:
//
//   * too small — the OCI manifest push (card_6cbde71c452d) ran on 2 MiB while
//     the spec allows 4 MiB, and the job-log upload (card_f0958a52d286) lost a
//     verbose build's entire log to it;
//   * too large — the inbound CI webhook (card_f82dd9f820e1) let any account
//     with `RepoWrite` make the server hold 2 MiB and run HMAC-SHA256 over all
//     of it, for a payload whose legitimate size is a few hundred bytes.
//
// All three were found by somebody reading the router, which is not a method:
// nothing under `scripts/` looked at body limits at all, so the fourth would
// have arrived exactly as quietly. This is that reading, mechanised.
//
// ── What is in scope, and why the line is where it is ──────────────────────
//
// Opaque buffered bodies only: `String`, `Bytes`, `Multipart`. Their size is
// chosen by the caller's content — a build log, an uploaded file, a signed
// payload — so what a route accepts is a decision somebody has to have taken,
// and 2 MiB is the mark of nobody having taken it.
//
//   * `Json<T>` / `Form<T>` buffer too, but their size is bounded by a declared
//     struct rather than by user content, and 78 of them are mounted here. A
//     gate covering those would demand 78 declarations or 78 allowlist entries
//     on its first run, which is how an allowlist stops meaning anything. Where
//     a JSON envelope really does carry user-sized content the router already
//     declares it (`content_edit_envelope`, `package_envelope`), and that is a
//     decision this gate is happy to see, not one it forces.
//   * `Body` is the *streaming* extractor: it inherits no default because it
//     buffers nothing. The routes that take it (`git-receive-pack`, LFS,
//     release assets, package publish) bound memory by spooling instead, which
//     is a different contract with its own checks.
//
// ── What "declares a ceiling" means ────────────────────────────────────────
//
// The route is mounted with `*_with(..., &binding)` where the binding's `let`
// in the router calls one of the `Wrap` constructors that carry a body limit.
// Reading the constructor's *name* would be the same trap `Wrap::credential`
// documents one level down — a `Wrap::body_limit_soon` would satisfy a
// name-matching gate while layering nothing — so the names below are held to
// `route_table.rs`: each must reach `apply_body_limit`, and `apply_body_limit`
// itself must still apply both halves. Dropping `DefaultBodyLimit::max` from it
// is one line that would silently return every route on this list to 2 MiB
// while every wrapper still reads as present.
//
// ── And how big that ceiling is allowed to be ──────────────────────────────
//
// A declared ceiling is only half an answer, because until this half existed
// the check read the constructor's name and never its argument. The router has
// bindings for every scale — `upload_limit` is 10 GiB for OCI blobs,
// `lfs_upload_limit` is another 10 GiB — and all of them are legitimate on the
// *streaming* routes they are mounted on. Nothing stopped a `Bytes` or `String`
// handler being mounted on one of them tomorrow: the wrapper resolves to
// `Wrap::body_limit`, the check says "ceiling declared", and the server starts
// allocating ten gibibytes per request before the handler is entered. That is
// not hypothetical — card_1d9115c2340f is the same defect one extractor over,
// where the release-asset route collected an `axum::Body` up to 512 MiB.
//
// So the ceiling of a route whose extractor produces ONE allocation of the
// whole body — `String` and `Bytes` — is resolved to a number and held under
// `WHOLE_BODY_MAX_BYTES`. `Multipart` is deliberately not: it hands the handler
// one field at a time, so its ceiling bounds a framed envelope rather than a
// buffer, and what the handler then does with a field is the *consumption*
// question that `body-limit-status-contract-check.mjs` owns. Holding the
// attachment routes' honest 101 MiB envelope to a buffered body's cap would be
// a red on correct code, which is how a gate gets switched off.
//
// Truth boundary: the router and every handler are read through
// `productionRustSource`, so a commented-out registration reads as deleted and
// a handler built inside `#[cfg(test)]` cannot answer for the one the server
// mounts.

import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import {
  loadMountedHandlers,
  productionRustCode,
  productionRustSource,
  rustFnBlock,
  rustFnHead,
  rustParamType,
  splitRustParams,
} from './lib/rust-source.mjs';

const root = process.cwd();
const HTTP_SRC = path.join(root, 'crates/rg-http/src');
const ROUTER = path.join(HTTP_SRC, 'routes.rs');
const ROUTE_TABLE = path.join(HTTP_SRC, 'route_table.rs');

const failures = [];

// ── The extractors this gate is about ──────────────────────────────────────
//
// Matched on the parameter's declared type, in every spelling the tree uses
// (`Bytes` and `axum::body::Bytes` are the same extractor).
const BUFFERING = [
  { kind: 'String', matches: (type) => type === 'String' },
  { kind: 'Bytes', matches: (type) => type === 'Bytes' || type === 'axum::body::Bytes' },
  { kind: 'Multipart', matches: (type) => type === 'Multipart' || type.endsWith('::Multipart') },
];

/**
 * The extractor inside `Result<T, SomeRejection>`, or the type itself.
 *
 * Taking the body as a `Result` is how a handler keeps its own error envelope
 * on a rejection instead of Axum's plain text — `oci::put_manifest` does it, and
 * it is the *same* buffered `String`, inheriting the same default. Reading the
 * outer type only exempted the very route the OCI half of this class was found
 * on (card_6cbde71c452d); the mutation stand is what caught that, by refusing to
 * go red when that route's wrapper was taken away.
 */
function unwrapRejection(type) {
  const inner = /^Result<(.+)>$/.exec(type);
  if (inner === null) return type;
  let depth = 0;
  for (let i = 0; i < inner[1].length; i += 1) {
    const ch = inner[1][i];
    if (ch === '<' || ch === '(' || ch === '[') depth += 1;
    else if (ch === '>' || ch === ')' || ch === ']') depth -= 1;
    else if (ch === ',' && depth === 0) return inner[1].slice(0, i);
  }
  return inner[1];
}

// The `Wrap` constructors that layer a request-body ceiling. Names only — each
// one is checked against `route_table.rs` below, because a name is exactly what
// a wrapper that layers nothing could also claim.
const LIMIT_CONSTRUCTORS = ['body_limit', 'runner_auth_with_body_limit'];

// The extractors whose ceiling is also the size of a single allocation the
// server makes before the handler runs. `Multipart` is not one of them — see
// the header — so it is bound by the declaration half alone.
const WHOLE_BODY_KINDS = new Set(['String', 'Bytes']);

// The largest ceiling a fully buffered body may declare. It is the largest one
// the tree honestly has today (`JOB_LOG_MAX_BYTES`), not a round number chosen
// for comfort: raising it is a decision somebody has to take here, in one
// place, rather than by mounting a route on a bigger binding somewhere else.
const WHOLE_BODY_MAX_BYTES = 8 * 1024 * 1024;

// Floor for the same reason the buffering floor exists one level up: with no
// whole-body routes recognised this half asserts nothing and the check still
// exits 0. Three today (`oci::put_manifest`, `runners::upload_log`,
// `webhooks_external::external_ci_webhook`); the floor sits one under.
const MIN_WHOLE_BODY_ROUTES = 2;

// ── Exemptions ─────────────────────────────────────────────────────────────
//
// A route that buffers an opaque body and deliberately declares no ceiling
// belongs here with the reason written out, so the decision is readable next to
// the rule rather than absent from it. Empty today: every buffering route in
// the tree declares its limit. An entry naming a route that is no longer
// mounted, or one that has since been given a limit, is itself a failure —
// otherwise a stale exemption quietly widens the gate.
const EXEMPT = [];

// Floors. A parser that stopped understanding its input reports a clean tree
// rather than a broken parse: with no buffering routes recognised, every route
// is trivially compliant. The tree has ten such registrations today (two
// `String`, two `Bytes`, six `Multipart`); the floor sits one under that, so
// retiring a route does not trip it while any of the three kinds silently
// falling out of `BUFFERING` does.
//
// It came down from 9 when the CI cache upload stopped buffering: that route
// now takes `axum::body::Body` and spools it, so it is *correctly* invisible
// here — this check reads signatures, and a route with no buffering extractor
// has nothing for it to hold against a mount. Lowering the floor is what makes
// that deliberate rather than a hole; the route keeps its declared ceiling, and
// what proves the ceiling now lives in `runners.rs`'s own spool tests.
//
// Back up to 9 with the MCP endpoint (`POST /api/v1/mcp`, a `Bytes` body under
// its own declared ceiling): ten registrations, the floor one under them, so
// losing the two `String` routes still trips it.
//
// 10 with the avatar upload (`PUT /api/v1/users/me/avatar`, a `Bytes` body
// under `AVATAR_UPLOAD_MAX_BYTES`, card_ca894e30ac80): eleven registrations,
// the floor one under them, for the same reason.
const MIN_BUFFERING_ROUTES = 10;

/** `api::runners::upload_log` → the file it lives in and the fn name. */
function locate(handler) {
  const segments = handler.split('::');
  const fn = segments.pop();
  const base = path.join(HTTP_SRC, ...segments);
  for (const candidate of [`${base}.rs`, path.join(base, 'mod.rs')]) {
    if (existsSync(candidate)) return { file: candidate, fn };
  }
  return null;
}

/** Every buffering extractor in `fn`'s signature, by kind. */
function bufferingExtractors(source, fn) {
  // `rustFnBlock` reads `pub async fn`, which every mounted handler is; the
  // head-only fallback keeps a handler written in another shape from silently
  // reading as "takes no body" rather than as unreadable.
  const block = rustFnBlock(source, fn);
  let params = null;
  if (block) {
    params = splitRustParams(block.params);
  } else {
    const head = rustFnHead(source, fn);
    if (head === null) return null;
    const open = head.indexOf('(');
    const close = head.lastIndexOf(')');
    if (open < 0 || close < open) return null;
    params = splitRustParams(head.slice(open + 1, close));
  }
  if (params === null) return null;

  const kinds = new Set();
  for (const param of params) {
    const declared = rustParamType(param);
    if (declared === null) continue;
    const type = unwrapRejection(declared.replace(/\s+/g, ''));
    for (const extractor of BUFFERING) {
      if (extractor.matches(type)) kinds.add(extractor.kind);
    }
  }
  return [...kinds];
}

/** The index of the `)` closing the `(` at `open`, or `null`. */
function balanced(source, open) {
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === '(') depth += 1;
    else if (source[i] === ')') {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return null;
}

/**
 * `let manifest_limit = Wrap::body_limit(oci::MANIFEST_MAX_BYTES)` →
 * `manifest_limit` → `{ constructor: 'body_limit', argument: 'oci::MANIFEST_MAX_BYTES' }`.
 *
 * The argument is what the magnitude half reads, and it is taken by balancing
 * the call's own parentheses rather than by matching up to the next `)`: the
 * package envelope is spelled `Wrap::body_limit(package_upload_envelope_limit(x))`
 * and a lazy match would hand back half of it.
 */
function wrapBindings(routerSource) {
  const bindings = new Map();
  const re = /\blet\s+(\w+)\s*=\s*Wrap::(\w+)\s*\(/g;
  let match;
  while ((match = re.exec(routerSource)) !== null) {
    const open = re.lastIndex - 1;
    const close = balanced(routerSource, open);
    bindings.set(match[1], {
      constructor: match[2],
      argument: close === null ? null : routerSource.slice(open + 1, close).trim(),
    });
  }
  return bindings;
}

/**
 * The ceiling argument of a `Wrap` constructor call.
 *
 * `Wrap::body_limit(n)` takes it alone; `Wrap::runner_auth_with_body_limit(state, n)`
 * takes the state first, so the ceiling is the last top-level argument in both.
 */
function ceilingArgument(argument) {
  if (argument === null) return null;
  let depth = 0;
  let last = 0;
  for (let i = 0; i < argument.length; i += 1) {
    const ch = argument[i];
    if (ch === '(' || ch === '<' || ch === '[') depth += 1;
    else if (ch === ')' || ch === '>' || ch === ']') depth -= 1;
    else if (ch === ',' && depth === 0) last = i + 1;
  }
  return argument.slice(last).trim();
}

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
 * `SCREAMING_CASE` constant → its initialiser, over the crate's production
 * source.
 *
 * A name defined twice with two different expressions is dropped rather than
 * guessed at: this reader has no module resolution, and answering with one of
 * two candidate numbers would be worse than saying the ceiling cannot be read.
 */
function constantIndex() {
  const found = new Map();
  const re = /\bconst\s+([A-Z][A-Z0-9_]*)\s*:[^=;]+=\s*([^;]+);/g;
  for (const file of rustFiles(HTTP_SRC)) {
    const code = productionRustCode(readFileSync(file, 'utf8'));
    let match;
    while ((match = re.exec(code)) !== null) {
      const expression = match[2].replace(/\s+/g, ' ').trim();
      const seen = found.get(match[1]);
      if (seen === undefined) found.set(match[1], expression);
      else if (seen !== expression) found.set(match[1], null); // ambiguous
    }
  }
  return found;
}

const CONSTANTS = constantIndex();

/**
 * A Rust byte-count expression as a number, or `null` when it cannot be read.
 *
 * Handles what the router's ceilings are actually made of: integer literals,
 * `as` casts, parentheses, `+ - * /`, and paths to `SCREAMING_CASE` constants
 * in this crate, resolved recursively. Anything else — a call, a config-derived
 * value, a constant from another crate — returns `null`, and the caller reports
 * that as a ceiling nobody can check rather than as a ceiling that passed. A
 * non-integral result is `null` too: JavaScript's `/` is not Rust's, and a
 * number this reader computed differently is not evidence about the server.
 */
function resolveBytes(expression, seen = new Set(), depth = 0) {
  if (expression === null || depth > 8) return null;
  let text = expression.replace(/\bas\s+[A-Za-z_][A-Za-z0-9_]*/g, ' ');
  text = text.replace(/(\d)_(?=\d)/g, '$1');
  const paths = text.match(/[A-Za-z_][A-Za-z0-9_]*(?:\s*::\s*[A-Za-z_][A-Za-z0-9_]*)*/g) ?? [];
  for (const reference of new Set(paths)) {
    const name = reference.split('::').pop().trim();
    if (seen.has(name)) return null;
    const initialiser = CONSTANTS.get(name);
    if (initialiser === undefined || initialiser === null) return null;
    const value = resolveBytes(initialiser, new Set([...seen, name]), depth + 1);
    if (value === null) return null;
    text = text.split(reference).join(`(${value})`);
  }
  if (!/^[\d\s+\-*/()]+$/.test(text) || !/\d/.test(text)) return null;
  let value;
  try {
    // eslint-disable-next-line no-new-func -- the expression is arithmetic only by the test above.
    value = Function(`"use strict"; return (${text});`)();
  } catch {
    return null;
  }
  return Number.isSafeInteger(value) && value > 0 ? value : null;
}

/** `8388608` → `8 MiB`, for a verdict somebody has to act on. */
function human(bytes) {
  const units = ['bytes', 'KiB', 'MiB', 'GiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1 && value % 1024 === 0) {
    value /= 1024;
    unit += 1;
  }
  return `${value} ${units[unit]}`;
}

// ── The constructors have to be what their names say ───────────────────────

// `Wrap`'s limit constructors are `impl` methods and `apply_body_limit` is a
// private free fn, so neither is a top-level `pub async fn` — the shared
// finders in `lib/rust-source.mjs` anchor at column 0 and read neither. This
// reads a body by balancing braces in the code-only production view, where
// string contents and comments are already blanked, so a `{` inside a doc
// comment or a literal cannot end a body early and a `#[cfg(test)]` double
// cannot answer for the real one.
function fnBody(source, name) {
  const code = productionRustCode(source);
  const declaration = new RegExp(`\\bfn\\s+${name}\\s*(?:<[^>]*>)?\\s*\\(`).exec(code);
  if (declaration === null) return null;
  const brace = code.indexOf('{', declaration.index + declaration[0].length);
  if (brace < 0) return null;
  let depth = 0;
  for (let i = brace; i < code.length; i += 1) {
    if (code[i] === '{') depth += 1;
    else if (code[i] === '}') {
      depth -= 1;
      if (depth === 0) return code.slice(brace, i + 1);
    }
  }
  return null;
}

const routeTable = readFileSync(ROUTE_TABLE, 'utf8');

const applyBodyLimit = fnBody(routeTable, 'apply_body_limit');
if (applyBodyLimit === null) {
  failures.push(
    'crates/rg-http/src/route_table.rs: `apply_body_limit` is missing or unreadable — it is what ' +
      'every declared ceiling on this list is made of',
  );
} else {
  // Both halves, or the declaration is only half true: the transport ceiling
  // refuses a declared `Content-Length` before routing, and `DefaultBodyLimit`
  // is the one a buffered extractor reads. Without the second, every route
  // below keeps its wrapper and quietly returns to Axum's 2 MiB.
  for (const half of ['DefaultBodyLimit::max', 'RequestBodyLimitLayer::new']) {
    if (!applyBodyLimit.includes(half)) {
      failures.push(
        `crates/rg-http/src/route_table.rs: \`apply_body_limit\` no longer applies \`${half}\` — ` +
          'a route that declares a ceiling would still be served under a different one',
      );
    }
  }
}

for (const constructor of LIMIT_CONSTRUCTORS) {
  const body = fnBody(routeTable, constructor);
  if (body === null) {
    failures.push(
      `crates/rg-http/src/route_table.rs: \`Wrap::${constructor}\` is gone, but this check still ` +
        'accepts it as a declared ceiling — remove it from LIMIT_CONSTRUCTORS, or the routes ' +
        'naming it pass on a name that means nothing',
    );
    continue;
  }
  if (!body.includes('apply_body_limit')) {
    failures.push(
      `crates/rg-http/src/route_table.rs: \`Wrap::${constructor}\` does not reach ` +
        '`apply_body_limit`, so a route mounted with it declares a ceiling nothing enforces',
    );
  }
}

// ── Every buffering route against its mount ────────────────────────────────

const routerSource = productionRustSource(readFileSync(ROUTER, 'utf8'));
const bindings = wrapBindings(routerSource);
const rows = loadMountedHandlers(ROUTER);

const sources = new Map();
function sourceOf(file) {
  if (!sources.has(file)) sources.set(file, readFileSync(file, 'utf8'));
  return sources.get(file);
}

const buffering = [];
for (const row of rows) {
  const located = locate(row.handler);
  if (located === null) {
    failures.push(
      `crates/rg-http/src/routes.rs:${row.line}: \`${row.handler}\` cannot be resolved to a file, ` +
        'so nothing here can tell whether it buffers its body',
    );
    continue;
  }
  const kinds = bufferingExtractors(sourceOf(located.file), located.fn);
  if (kinds === null) {
    failures.push(
      `crates/rg-http/src/routes.rs:${row.line}: the signature of \`${row.handler}\` cannot be ` +
        'read, so its extractors are unknown — fix the reader rather than skipping the handler',
    );
    continue;
  }
  if (kinds.length > 0) buffering.push({ ...row, kinds });
}

if (buffering.length < MIN_BUFFERING_ROUTES) {
  failures.push(
    `only ${buffering.length} buffering route(s) were recognised in the router (expected at least ` +
      `${MIN_BUFFERING_ROUTES}). Either the extractors moved to a spelling BUFFERING does not ` +
      'match, or the handler reader went quiet — a gate that sees no buffering routes reports ' +
      'every route as compliant. If these routes really were retired, lower the floor on purpose',
  );
}

/** How a route is named in a failure and in an exemption. */
const routeName = (row) => `${row.method} ${(row.prefix ?? '') + (row.path ?? '<generated>')}`;

const exemptByName = new Map();
for (const entry of EXEMPT) {
  if (!entry.reason || entry.reason.trim().length === 0) {
    failures.push(`the exemption for ${entry.route} carries no reason, which is what it is for`);
    continue;
  }
  exemptByName.set(`${entry.route} → ${entry.handler}`, entry);
}
const exemptionsUsed = new Set();

const wholeBody = [];

for (const row of buffering) {
  const key = `${routeName(row)} → ${row.handler}`;
  const binding = row.wrapper === null ? null : bindings.get(row.wrapper.replace(/^&/, ''));
  const constructor = binding === undefined ? undefined : binding?.constructor ?? null;
  const declares = constructor != null && LIMIT_CONSTRUCTORS.includes(constructor);

  if (exemptByName.has(key)) {
    exemptionsUsed.add(key);
    if (declares) {
      failures.push(
        `${key} declares a ceiling now, so its exemption is stale — remove the EXEMPT entry rather ` +
          'than leaving a decision recorded that the router no longer takes',
      );
    }
    continue;
  }

  if (!declares) {
    const how =
      row.wrapper === null
        ? 'mounted with no wrapper at all'
        : `mounted with \`${row.wrapper}\`, which is ` +
          (constructor === undefined
            ? 'not a `Wrap` bound in this router'
            : `\`Wrap::${constructor}\` — not one of the constructors that carry a body limit`);
    failures.push(
      `crates/rg-http/src/routes.rs:${row.line}: ${key} buffers its body ` +
        `(${row.kinds.join(', ')}) but is ${how}. A buffered extractor with nothing declared runs ` +
        "on Axum's 2 MiB default, which is not a ceiling this router chose — mount it with " +
        '`*_with(..., &<a Wrap::body_limit binding>)`, or record the decision in EXEMPT with a ' +
        'reason',
    );
    continue;
  }

  // ── The magnitude half ───────────────────────────────────────────────────
  //
  // Only for the extractors that turn the ceiling into one allocation. A
  // `Multipart` route's ceiling bounds the framed envelope it streams, so the
  // number is not a memory budget and holding it to one would be a red on
  // correct code.
  if (!row.kinds.some((kind) => WHOLE_BODY_KINDS.has(kind))) continue;
  wholeBody.push(row);

  const argument = ceilingArgument(binding.argument);
  const bytes = resolveBytes(argument);
  if (bytes === null) {
    failures.push(
      `crates/rg-http/src/routes.rs:${row.line}: ${key} buffers its whole body ` +
        `(${row.kinds.join(', ')}), but the ceiling it declares — \`${argument}\` — cannot be ` +
        'read as a byte count here, so nothing checks how much memory one request may take. ' +
        'Declare it as a literal or a `SCREAMING_CASE` constant in `rg-http`, or stop buffering ' +
        'the body',
    );
    continue;
  }
  if (bytes > WHOLE_BODY_MAX_BYTES) {
    failures.push(
      `crates/rg-http/src/routes.rs:${row.line}: ${key} buffers its whole body ` +
        `(${row.kinds.join(', ')}) under a declared ceiling of ${human(bytes)}, above the ` +
        `${human(WHOLE_BODY_MAX_BYTES)} this check allows. A ceiling declared on a buffered ` +
        'extractor is the size of one allocation the server makes per request before the ' +
        'handler is entered, so this one is a heap budget, not a policy — stream or spool the ' +
        'body the way the release-asset and package routes do, or lower the ceiling',
    );
  }
}

if (wholeBody.length < MIN_WHOLE_BODY_ROUTES) {
  failures.push(
    `only ${wholeBody.length} route(s) buffering the whole body were recognised (expected at ` +
      `least ${MIN_WHOLE_BODY_ROUTES}). With none of them recognised no ceiling is measured at ` +
      'all and every declared limit passes on its name again, which is the defect this half was ' +
      'added for. If these routes really were retired, lower the floor on purpose',
  );
}

for (const [key] of exemptByName) {
  if (!exemptionsUsed.has(key)) {
    failures.push(
      `the exemption for ${key} matches no buffering route in the router — it was renamed, ` +
        'unmounted, or no longer buffers, and a stale exemption is a hole nobody reads',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ body limit declaration contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ body limit declaration contract: all ${buffering.length} routes buffering an opaque body ` +
    `declare the ceiling they buffer up to, and the ${wholeBody.length} that buffer it whole ` +
    `declare a readable ceiling at or under ${human(WHOLE_BODY_MAX_BYTES)}`,
);
