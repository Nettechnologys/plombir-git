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
// Truth boundary: the router and every handler are read through
// `productionRustSource`, so a commented-out registration reads as deleted and
// a handler built inside `#[cfg(test)]` cannot answer for the one the server
// mounts.

import { existsSync, readFileSync } from 'node:fs';
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
const MIN_BUFFERING_ROUTES = 9;

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

/** `let manifest_limit = Wrap::body_limit(…)` → `manifest_limit` → `body_limit`. */
function wrapBindings(routerSource) {
  const bindings = new Map();
  const re = /\blet\s+(\w+)\s*=\s*Wrap::(\w+)\s*\(/g;
  let match;
  while ((match = re.exec(routerSource)) !== null) bindings.set(match[1], match[2]);
  return bindings;
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

for (const row of buffering) {
  const key = `${routeName(row)} → ${row.handler}`;
  const constructor = row.wrapper === null ? null : bindings.get(row.wrapper.replace(/^&/, ''));
  const declares = constructor !== null && LIMIT_CONSTRUCTORS.includes(constructor);

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
  }
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
    'declare the ceiling they buffer up to',
);
