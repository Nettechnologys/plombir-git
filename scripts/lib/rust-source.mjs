// Helpers for asserting against Rust sources from the contract checks.

import { readFileSync } from 'node:fs';

/**
 * Strip Rust line/block comments from `source`, preserving string literals and newlines.
 *
 * Assertions grep the source for a construct — a route row, a handler, a DTO field. Without
 * this, a construct that was merely commented out still satisfies the regex: the code is gone
 * from the binary while the gate stays green. Stripping comments first makes "commented out"
 * fail like "deleted".
 *
 * Note: Rust raw strings (`r"..."` / `r#"..."#`) are not handled; the router sources these
 * checks read do not use them.
 */
export function stripRustComments(source) {
  let out = '';
  let i = 0;
  while (i < source.length) {
    const two = source.slice(i, i + 2);
    if (two === '//') {
      while (i < source.length && source[i] !== '\n') i += 1;
    } else if (two === '/*') {
      let depth = 1;
      i += 2;
      while (i < source.length && depth > 0) {
        if (source.slice(i, i + 2) === '/*') {
          depth += 1;
          i += 2;
        } else if (source.slice(i, i + 2) === '*/') {
          depth -= 1;
          i += 2;
        } else {
          if (source[i] === '\n') out += '\n';
          i += 1;
        }
      }
    } else if (source[i] === '"') {
      out += source[i];
      i += 1;
      while (i < source.length) {
        if (source[i] === '\\') {
          out += source.slice(i, i + 2);
          i += 2;
          continue;
        }
        out += source[i];
        i += 1;
        if (source[i - 1] === '"') break;
      }
    } else {
      out += source[i];
      i += 1;
    }
  }
  return out;
}

// ── Route table ────────────────────────────────────────────────────────────
//
// `crates/rg-http/src/routes.rs` no longer registers routes with axum's
// `.route("/path", post(handler))`. Every route now goes through `RouteTable`,
// whose methods take the access level first:
//
//     .post(User, "/imports", api::imports::start_import)
//     .get_with(Public, "/auth/login", api::auth::login, &credential_limit)
//
// Checks that grepped the old spelling went red on a router refactor that
// changed nothing about their contract. Reading the table once, here, keeps
// that form in a single place — and hands the checks the access level, which
// the old form could not express at all.

/** Builder methods on `RouteTable`; `_with` variants take a trailing wrapper. */
const ROUTE_METHODS = ['get', 'head', 'post', 'put', 'patch', 'delete'];

/**
 * Lower bound on the number of rows a healthy parse yields (the table holds
 * ~325). A parser that silently understands nothing turns every check that
 * uses it into a no-op, which is worse than a red check — so falling under
 * this floor is a loud error, not an empty result.
 */
const MIN_PARSED_ROUTES = 200;

/**
 * Split the argument list of a call whose opening `(` sits at `open`.
 *
 * Returns the top-level arguments as raw source slices, or `null` if the
 * parentheses never balance (truncated source).
 */
function splitCallArgs(source, open) {
  const args = [];
  let depth = 0;
  let start = open + 1;
  let i = open;
  while (i < source.length) {
    const ch = source[i];
    if (ch === '"') {
      i += 1;
      while (i < source.length) {
        if (source[i] === '\\') {
          i += 2;
          continue;
        }
        if (source[i] === '"') break;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') {
      depth += 1;
    } else if (ch === ')' || ch === ']' || ch === '}') {
      depth -= 1;
      if (depth === 0) {
        args.push(source.slice(start, i));
        const trimmed = args.map((arg) => arg.trim());
        // Rust's trailing comma leaves an empty tail segment.
        if (trimmed.length > 0 && trimmed[trimmed.length - 1] === '') trimmed.pop();
        return trimmed;
      }
    } else if (ch === ',' && depth === 1) {
      args.push(source.slice(start, i));
      start = i + 1;
    }
    i += 1;
  }
  return null;
}

/** `const GIT_HTTP: Access = Foreign("git-over-HTTP: ...");` → `Foreign("...")`. */
function accessConstants(source) {
  const constants = new Map();
  const re = /const\s+([A-Z][A-Z0-9_]*)\s*:\s*Access\s*=\s*([^;]+);/g;
  let match;
  while ((match = re.exec(source)) !== null) {
    constants.set(match[1], match[2].trim().replace(/\s+/g, ' '));
  }
  return constants;
}

/**
 * Parse the `(method, path, access, handler)` rows out of a `RouteTable` build.
 *
 * Comments are stripped first: a commented-out row must read as a deleted
 * route, not as a live one (see `stripRustComments`).
 *
 * Paths are as written in the source — i.e. relative to the sub-router's nest
 * prefix, the same spelling the checks assert on. The nesting prefix lives in
 * `RouteTable::new("/api/v1")` and is applied by the Rust side, not here.
 */
export function parseRouteTable(source) {
  const src = stripRustComments(source);
  const constants = accessConstants(src);
  const rows = [];
  const re = new RegExp(`\\.(${ROUTE_METHODS.join('|')})(_with)?\\s*\\(`, 'g');
  let match;
  while ((match = re.exec(src)) !== null) {
    const open = re.lastIndex - 1;
    const args = splitCallArgs(src, open);
    // A route registration is `(access, "path", handler)` — at minimum three
    // arguments with a string literal in the middle. Anything else (`map.get(k)`,
    // `opt.get_or_insert_with(f)`) is not ours.
    if (!args || args.length < 3) continue;
    const pathLiteral = args[1].match(/^"((?:[^"\\]|\\.)*)"$/);
    if (!pathLiteral) continue;

    const rawAccess = args[0].replace(/\s+/g, ' ');
    rows.push({
      method: match[1].toUpperCase(),
      path: pathLiteral[1],
      handler: args[2].replace(/\s+/g, ''),
      access: constants.get(rawAccess) ?? rawAccess,
      layered: Boolean(match[2]),
      line: src.slice(0, match.index).split('\n').length,
    });
  }
  return rows;
}

/**
 * Read `routerPath` and parse its route table.
 *
 * Throws when the parse comes back implausibly small: the router form changed
 * again and the resolver — not the individual assertions — is what needs
 * fixing. A check that cannot read the router must say so, not pass.
 */
export function loadRouteTable(routerPath) {
  const routes = parseRouteTable(readFileSync(routerPath, 'utf8'));
  if (routes.length < MIN_PARSED_ROUTES) {
    throw new Error(
      `Route table parser understood only ${routes.length} routes in ${routerPath} ` +
        `(expected at least ${MIN_PARSED_ROUTES}). The router registration form probably changed — ` +
        'fix parseRouteTable() in scripts/lib/rust-source.mjs rather than the assertions that use it.',
    );
  }
  return routes;
}

/** The row for `method path`, or `undefined`. */
export function findRoute(routes, method, routePath) {
  return routes.find((route) => route.method === method.toUpperCase() && route.path === routePath);
}

/** Whether any row declares `method path`. */
export function hasRoute(routes, method, routePath) {
  return findRoute(routes, method, routePath) !== undefined;
}

/**
 * Check expectations against the parsed table.
 *
 * Each expectation is `{ method, path, handler?, access? }`; `handler` and
 * `access` are asserted only when given. Returns one message per problem, so a
 * route that exists but lost its access level reads differently from one that
 * is gone.
 */
export function routeFailures(routes, expectations) {
  const failures = [];
  for (const { method, path: routePath, handler, access } of expectations) {
    const route = findRoute(routes, method, routePath);
    if (!route) {
      const others = routes.filter((candidate) => candidate.path === routePath).map((candidate) => candidate.method);
      const hint = others.length > 0 ? ` (path is declared for ${others.join(', ')})` : '';
      failures.push(`Route table must declare ${method.toUpperCase()} ${routePath}${hint}`);
      continue;
    }
    if (handler && route.handler !== handler) {
      failures.push(
        `Route ${method.toUpperCase()} ${routePath} must be served by ${handler} (declared: ${route.handler})`,
      );
    }
    if (access && route.access !== access) {
      failures.push(
        `Route ${method.toUpperCase()} ${routePath} must declare access ${access} (declared: ${route.access})`,
      );
    }
  }
  return failures;
}
