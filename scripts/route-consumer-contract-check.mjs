#!/usr/bin/env node

// Every mounted MUTATING route must have something that calls it — or a
// recorded reason why nothing in this repository ever will.
//
// The defect this hunts lives in the *absence* of a call, so no amount of
// reading the handler finds it: the route is mounted, the gate in front of it is
// right, the handler is correct, and the button that would reach it was never
// wired. Four of them were found one at a time over this phase — package yank
// (card_e141e9fa00d5), review dismissal (card_1714b4dacad5), SSO unlink
// (card_2cd2d40f27d2), CI artifact deletion (card_c085f7d514bb) — each by
// somebody noticing, which is not a method. The one-off sweep that finally
// enumerated them lived in a scratchpad, so the fifth would have been found the
// same way. This is that sweep as a gate.
//
// MUTATING only, on purpose. The reading half was swept the same way when this
// was written: of 174 `GET` rows, exactly six had no consumer — `/api-docs*`
// (the server hands them to Swagger UI), `/metrics` (Prometheus) and
// `/ci/oidc/jwks` (an external verifier) — all correct. The hole was in the
// writing half, and a gate that also polices reads would spend its allowlist on
// documented non-defects.
//
// ── What counts as a consumer ──────────────────────────────────────────────
//
// A file under `web/src` that carries BOTH the route's URL shape and the
// route's method. Not a single expression, because the client does not write
// one: `request('<path>', { method })` in one place, `fetch(withApiBase(path(…)),
// { method })` for a multipart upload in another, `xhr.open('POST', withApiBase(
// path))` where a progress bar is wanted — and in the last one the URL is the
// caller's argument, several frames from the method. Insisting on a single
// resolvable expression would have reported eight live routes as orphans and
// pushed them into the allowlist, which is how an allowlist stops meaning
// anything.
//
// File-level co-location is the broad fallback that survives all three
// spellings, and it is still tight enough for a route with no competing path
// registration: every original defect above was a route no client file
// mentioned *at all*. Where router patterns overlap, the shared UI-surface
// parser keeps method + path on the same exported member and applies Axum's
// static > placeholder > catch-all ownership. Otherwise a DELETE path, an
// unrelated PATCH and a static sibling in one module can manufacture a call
// that does not exist. Comments never count — both views discard them.
//
// Two narrow expansions keep the client's own indirection from reading as
// absence, and both are bounded on purpose:
//
//   * a `${helper(…)}` segment is replaced by the literal a same-file
//     `function helper(…) { return '<path>'; }` returns — one hop, single
//     `return`, no recursion. `attachments.ts` builds every one of its six URLs
//     that way;
//   * a `${…}` segment may also stand for a multi-segment member of a
//     string-literal union declared in the same file. `AttachmentTarget`
//     includes `'issues/comments'`, so one client segment is two route
//     segments. Only members containing `/` are expanded, which is why this
//     cannot loosen ordinary matching: a single-segment member is already what
//     `${…}` matches.
//
// ── What the gate cannot see, and what is done about it ────────────────────
//
// This proves a route has a *client call*, not that a page reaches it. The hop
// beyond — an exported `api.*` method nothing outside its own module calls —
// was measured rather than assumed: ten of them, and the ones read were
// deliberate rather than broken (`repos.unstar` is a redundant helper over a
// `PUT /star` that already toggles, `createCommitStatus` belongs to a surface
// external CI writes and no page ever will). So that hop is not gated here: it
// would spend its allowlist on decisions rather than on defects, and this
// phase's criterion is about what the UI *promises*, which is the hop above.
//
// A `${…}` in the client can select any one segment of the route. That is the
// honest reading of a generic client (`packages.publish` selects a registry at
// runtime), but a concrete static sibling owns its own literal. The vacuity
// self-test below keeps the broad fallback from degenerating into "everything
// matches": a route spelled to exist nowhere must come back unconsumed, or the
// index has stopped discriminating and every verdict above it is worthless.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';

import { canOutrank, outrankingRoutes } from './lib/route-specificity.mjs';
import { loadMountedHandlers, loadRouteTable } from './lib/rust-source.mjs';
import {
  extractBrowserTransportCalls,
  OPAQUE_SEGMENT,
  productionTsSource,
} from './lib/ts-source.mjs';
import {
  createLocalPathResolver,
  expandLocalPathCalls,
  multiSegmentUnionMembers,
} from './lib/ts-path-resolver.mjs';
import { parseApiSurface } from './lib/ui-surface.mjs';

const root = process.cwd();
const ROUTER = path.join(root, 'crates/rg-http/src/routes.rs');
const CLIENT_ROOT = path.join(root, 'web/src');
const API_PREFIX = '/api/v1';
const MUTATING = new Set(['POST', 'PUT', 'PATCH', 'DELETE']);

// Floors. A parser that stopped understanding its input must say so rather than
// report a clean tree: at zero rows every route is trivially consumed, and at
// zero client files every route is trivially an orphan. Both readings are
// worthless, and only the second is loud on its own.
const MIN_MUTATING_ROUTES = 120;
const MIN_CLIENT_FILES = 60;

const failures = [];

// ── The reasons a mounted mutating route may have no consumer here ─────────
//
// A closed vocabulary, not free text, so the allowlist below reads as four
// decisions rather than as twenty-seven exceptions. A new kind of exemption is
// a new entry here and a sentence about why it is a kind.
const REASONS = new Map([
  [
    'protocol',
    'a package or Git protocol endpoint: the client is `docker` / `cargo` / `npm` / `pip` / ' +
      '`nuget` / `git` / `git-lfs`, and its request shape is fixed by that protocol rather than ' +
      'by anything in this repository',
  ],
  [
    'runner',
    'the CI runner API: `forgekeep-runner` calls it, and it is a separate binary in `crates/' +
      'rg-runner`, not the SPA',
  ],
  [
    'agent',
    'an agent-facing endpoint reached by an MCP client or an operator with a token, deliberately ' +
      'without a page behind it',
  ],
  [
    'external-webhook',
    'an inbound webhook: the caller is somebody else\'s CI system by definition',
  ],
]);

// Keyed by `METHOD <url>` exactly as the router mounts it. An entry naming a
// route the router no longer serves fails below: the list is a ratchet, and a
// stale entry is how one turns into a blanket.
const ALLOWED_WITHOUT_CONSUMER = new Map([
  ['PUT /v2/{owner}/{repo}/manifests/{reference}', 'protocol'],
  ['POST /v2/{owner}/{repo}/blobs/uploads/', 'protocol'],
  ['POST /v2/{owner}/{repo}/blobs/uploads', 'protocol'],
  ['PATCH /v2/{owner}/{repo}/blobs/uploads/{uuid}', 'protocol'],
  ['PUT /v2/{owner}/{repo}/blobs/uploads/{uuid}', 'protocol'],
  ['PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new', 'protocol'],
  [
    'DELETE /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/yank',
    'protocol',
  ],
  [
    'PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/unyank',
    'protocol',
  ],
  ['POST /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems', 'protocol'],
  ['POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy/', 'protocol'],
  ['POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy', 'protocol'],
  ['POST /git/{owner}/{repo}/git-upload-pack', 'protocol'],
  ['POST /git/{owner}/{repo}/git-receive-pack', 'protocol'],
  ['POST /{owner}/{repo}/git-upload-pack', 'protocol'],
  ['POST /{owner}/{repo}/git-receive-pack', 'protocol'],
  ['POST /api/v1/repos/{owner}/{name}/lfs/objects/batch', 'protocol'],
  ['PUT /api/v1/repos/{owner}/{name}/lfs/objects/{oid}', 'protocol'],
  ['PUT /api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}', 'protocol'],
  ['PUT /api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}', 'protocol'],
  [
    'DELETE /api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}',
    'protocol',
  ],
  ['PUT /api/v1/repos/{owner}/{name}/packages/nuget/publish', 'protocol'],
  ['POST /api/v1/runners/{id}/heartbeat', 'runner'],
  ['POST /api/v1/runners/{id}/deregister', 'runner'],
  ['POST /api/v1/runners/{id}/jobs/{job_id}/start', 'runner'],
  ['POST /api/v1/runners/{id}/jobs/{job_id}/log', 'runner'],
  ['PUT /api/v1/runners/{id}/jobs/{job_id}/cache', 'runner'],
  ['POST /api/v1/runners/{id}/jobs/{job_id}/finish', 'runner'],
  ['PUT /api/v1/runners/{id}/jobs/{job_id}/artifacts/staging', 'runner'],
  ['POST /api/v1/runners/{id}/jobs/{job_id}/artifacts', 'runner'],
  ['POST /api/v1/ai/repos/{owner}/{name}/index', 'agent'],
  ['POST /api/v1/repos/{owner}/{name}/webhooks/external/ci', 'external-webhook'],
]);

// ── The router side ────────────────────────────────────────────────────────

/** `/a/{b}/c` → `['a', null, 'c']`; `null` is "any one segment". */
function shapeOf(url) {
  return url
    .replace(/\/+$/, '')
    .split('/')
    .slice(1)
    .map((segment) => (/^\{.*\}$/.test(segment) ? null : segment));
}

const table = loadRouteTable(ROUTER);
const prefixByLine = new Map(loadMountedHandlers(ROUTER).map((row) => [row.line, row.prefix]));
const routerRoutes = table.map((row) => {
  const prefix = prefixByLine.get(row.line);
  const url = `${prefix ?? ''}${row.path}`;
  return { ...row, url, prefixKnown: prefix != null, shape: shapeOf(url) };
});
const routes = routerRoutes.filter((row) => MUTATING.has(row.method));
const routeUrls = routerRoutes.map((row) => row.url);

if (routes.length < MIN_MUTATING_ROUTES) {
  failures.push(
    `Only ${routes.length} mutating routes were read out of ${path.relative(root, ROUTER)} ` +
      `(expected at least ${MIN_MUTATING_ROUTES}). Every verdict below is about a router this ` +
      'check can no longer read — fix the parsing, not the code.',
  );
}
for (const route of routerRoutes) {
  if (!route.prefixKnown) {
    failures.push(
      `${route.method} ${route.path} (${route.handler}) is mounted under a nest prefix this check ` +
        'cannot resolve, so the URL it compares against the client is a guess. Fix ' +
        'routePrefixResolver() in scripts/lib/rust-source.mjs.',
    );
  }
}

// ── The client side ────────────────────────────────────────────────────────

// The client's own test files are not consumers. `ssoLinks.test.ts` asserts
// that `unlinkSso` builds `/auth/sso/{slug}/unlink`, so it carries the URL and
// the method — and while it was counted, deleting the only production call left
// this check green over a feature the UI could no longer reach. "Alive because
// its own test calls it" is the state this check exists to name, which is the
// same reason the db-ops consumer gate strips `#[cfg(test)]` before counting.
const TEST_FILE = /\.(test|spec)\.(ts|js)$/;

function clientFiles(dir, found = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) clientFiles(entryPath, found);
    else if (/\.(ts|js|svelte)$/.test(entry.name) && !TEST_FILE.test(entry.name)) {
      found.push(entryPath);
    }
  }
  return found;
}

/** Any string or template literal that looks like a URL path. */
const PATH_LITERAL = /([`'"])(\/(?:[^\\`'"\n]|\\.|\$\{[^{}]*\})*)\1/g;

function indexClientFile({ file, relative, rawSource }, moduleSources) {
  const source = productionTsSource(rawSource);
  const resolver = createLocalPathResolver(source);
  const expanded = expandLocalPathCalls(source, resolver).map((row) => row.source).join('\n');
  const multiSegmentMembers = multiSegmentUnionMembers(resolver);

  const shapes = [];
  PATH_LITERAL.lastIndex = 0;
  let match;
  while ((match = PATH_LITERAL.exec(expanded)) !== null) {
    const literal = match[2].split('?')[0];
    const url = literal.startsWith(`${API_PREFIX}/`) ? literal : `${API_PREFIX}${literal}`;
    const base = url
      .replace(/\/+$/, '')
      .split('/')
      .slice(1)
      .map((segment) => (segment.includes('${') ? null : segment));
    shapes.push(base);
    for (let i = 0; i < base.length; i += 1) {
      if (base[i] !== null) continue;
      for (const member of multiSegmentMembers) {
        shapes.push([...base.slice(0, i), ...member, ...base.slice(i + 1)]);
      }
    }
  }

  const methods = new Set();
  for (const found of expanded.matchAll(/method:\s*['"]([A-Za-z]+)['"]/g)) {
    methods.add(found[1].toUpperCase());
  }
  for (const found of expanded.matchAll(/\.open\s*\(\s*['"]([A-Za-z]+)['"]/g)) {
    methods.add(found[1].toUpperCase());
  }

  // File-level co-location remains the deliberately broad fallback for routes
  // with no competing registration. Once two router patterns overlap, though,
  // method and path must come from the same client member: otherwise a DELETE
  // path plus an unrelated PATCH in the same module manufactures a call that
  // does not exist. The shared UI-surface parser already preserves that pair
  // across request(), fetch(), XHR and the bounded local/imported path helpers.
  const calls = new Map();
  const addCall = (call) => {
    const method = call.method.toUpperCase();
    const url = call.base === 'root'
      ? call.path
      : (call.path.startsWith(API_PREFIX) ? call.path : `${API_PREFIX}${call.path}`);
    if (url.includes(OPAQUE_SEGMENT)) return;
    calls.set(`${method}\u0000${url}`, { method, shape: shapeOf(url) });
  };
  for (const call of parseApiSurface(rawSource, relative, { moduleSources })) addCall(call);
  for (const call of extractBrowserTransportCalls(rawSource, relative)) addCall(call);

  return { file, shapes, methods, calls: [...calls.values()] };
}

const files = clientFiles(CLIENT_ROOT).map((file) => ({
  file,
  relative: path.relative(root, file).split(path.sep).join('/'),
  rawSource: readFileSync(file, 'utf8'),
}));
const moduleSources = new Map(files.map(({ relative, rawSource }) => [relative, rawSource]));
const indexed = files.map((file) => indexClientFile(file, moduleSources));
if (files.length < MIN_CLIENT_FILES) {
  failures.push(
    `Only ${files.length} client files were read out of ${path.relative(root, CLIENT_ROOT)} ` +
      `(expected at least ${MIN_CLIENT_FILES}). With no client to compare against, every route ` +
      'below would be reported as an orphan — fix the path, not the code.',
  );
}

function shapesMatch(route, client) {
  return (
    route.length === client.length &&
    route.every((segment, i) => segment === null || client[i] === null || segment === client[i])
  );
}

// A rival owns the whole client spelling only when every static segment it
// requires is static in that spelling. A dynamic `${value}` may still select a
// static registration at runtime (packages.publish is intentionally generic),
// but it is not proof that one particular static sibling stole the call.
function shapeCoveredBy(route, client) {
  return (
    route.length === client.length &&
    route.every((segment, i) => segment === null || (client[i] !== null && segment === client[i]))
  );
}

const routeRivals = new Map();
const rivalsOf = (routeUrl) => {
  let rivals = routeRivals.get(routeUrl);
  if (rivals === undefined) {
    rivals = outrankingRoutes(routeUrl, routeUrls).map(shapeOf);
    routeRivals.set(routeUrl, rivals);
  }
  return rivals;
};

const preciseRoutes = new Map();
function needsPreciseClientCall(routeUrl) {
  let precise = preciseRoutes.get(routeUrl);
  if (precise === undefined) {
    const registrationsAtPath = routeUrls.filter((candidate) => candidate === routeUrl).length;
    precise = registrationsAtPath > 1 || routeUrls.some(
      (candidate) => candidate !== routeUrl
        && (canOutrank(candidate, routeUrl) || canOutrank(routeUrl, candidate)),
    );
    preciseRoutes.set(routeUrl, precise);
  }
  return precise;
}

function routeOwnsClientShape(routeUrl, routeShape, clientShape) {
  return shapesMatch(routeShape, clientShape)
    && !rivalsOf(routeUrl).some((rival) => shapeCoveredBy(rival, clientShape));
}

function fileLevelConsumer(method, shape) {
  return indexed.find(
    (entry) => entry.methods.has(method) && entry.shapes.some((client) => shapesMatch(shape, client)),
  );
}

function consumedBy(method, routeUrl, shape) {
  if (needsPreciseClientCall(routeUrl)) {
    const precise = indexed.find((entry) => entry.calls.some(
      (call) => call.method === method && routeOwnsClientShape(routeUrl, shape, call.shape),
    ));
    return precise;
  }
  return fileLevelConsumer(method, shape);
}

// Vacuity self-test. A `${…}` segment matches any one route segment, so the
// rule degrades gracefully into "everything matches" if the shapes ever stop
// carrying literals. A URL nothing serves must come back unconsumed.
const IMPOSSIBLE = '/api/v1/__no_route_is_spelled_like_this__/{id}/__none__';
const vacuous = consumedBy('DELETE', IMPOSSIBLE, shapeOf(IMPOSSIBLE));
if (vacuous) {
  failures.push(
    `The client index claims ${IMPOSSIBLE} has a consumer (${path.relative(root, vacuous.file)}), ` +
      'so it no longer discriminates between routes and every verdict below is meaningless. Fix ' +
      'the shape matching, not the code.',
  );
}

// ── The comparison ─────────────────────────────────────────────────────────

for (const [key, reason] of ALLOWED_WITHOUT_CONSUMER) {
  if (!REASONS.has(reason)) {
    failures.push(
      `The allowlist exempts \`${key}\` for the reason \`${reason}\`, which is not one of the ` +
        `recorded kinds (${[...REASONS.keys()].join(', ')}). Add the kind and say why it is one.`,
    );
  }
  if (!routes.some((route) => `${route.method} ${route.url}` === key)) {
    failures.push(
      `The allowlist exempts \`${key}\`, but the router does not mount it any more — delete the ` +
        'entry so the list keeps describing the server.',
    );
  }
}
for (const [reason] of REASONS) {
  if (![...ALLOWED_WITHOUT_CONSUMER.values()].includes(reason)) {
    failures.push(
      `The reason \`${reason}\` is recorded but nothing uses it — delete it, or the vocabulary ` +
        'stops describing the exemptions it exists for.',
    );
  }
}

let consumed = 0;
const orphans = [];
for (const route of routes) {
  const key = `${route.method} ${route.url}`;
  const consumer = consumedBy(route.method, route.url, route.shape);
  if (consumer) {
    consumed += 1;
    if (ALLOWED_WITHOUT_CONSUMER.has(key)) {
      failures.push(
        `The allowlist says nothing calls \`${key}\`, but ${path.relative(
          root,
          consumer.file,
        )} does. Delete the entry — an exemption that no longer applies hides the next route that ` +
          'needs one.',
      );
    }
    continue;
  }
  if (ALLOWED_WITHOUT_CONSUMER.has(key)) continue;
  orphans.push(route);
}

for (const route of orphans) {
  failures.push(
    `${route.method} ${route.url} (${route.handler}, routes.rs:${route.line}) is mounted and ` +
      'nothing under web/src calls it. Wire it to the page that is supposed to reach it, or ' +
      'delete the route — a mounted mutating endpoint with no caller reads as a working feature ' +
      'and is not one. If the caller is deliberately not this repository, add it to ' +
      `ALLOWED_WITHOUT_CONSUMER with one of: ${[...REASONS.keys()].join(', ')}.`,
  );
}

if (failures.length > 0) {
  console.error('route consumer contract failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `route consumer contract ok (${consumed}/${routes.length} mutating routes called from ` +
    `${files.length} client files; ${ALLOWED_WITHOUT_CONSUMER.size} exempt across ` +
    `${REASONS.size} recorded reasons)`,
);
