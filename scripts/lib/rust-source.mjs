// Helpers for asserting against source files from the contract checks.

import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Extract the block matched by `re`, or record a failure and return `null`.
 *
 * Replaces the idiom `source.match(re)?.[0] || ''`. That spelling hands an
 * empty string to whatever inspects the block, and a *negative* assertion over
 * an empty string always passes — so a drifted anchor reads as "the forbidden
 * construct is absent" when what really happened is "I never found the code I
 * was supposed to inspect". The gate goes green over an unread file, which is
 * strictly worse than going red: nobody investigates a passing check.
 *
 * This is the per-block version of what `loadRouteTable` already refuses to do
 * for the router — a check that cannot read its subject must say so, not pass.
 *
 * Callers guard with `if (block)`, so a missed anchor turns the check red.
 *
 * @param {number} group Capture group to return; 0 (default) is the whole match.
 */
export function requireBlock(source, re, message, failures, group = 0) {
  const match = source.match(re);
  if (!match || match[group] === undefined) {
    failures.push(message);
    return null;
  }
  return match[group];
}

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

/**
 * The parameter list and body of a top-level `pub async fn <name>`, or `null`
 * when the function is not where the caller expects it.
 *
 * Handler-level assertions express what a file-wide grep cannot. Counting five
 * occurrences of a call across a module stays green when one handler drops the
 * call and another gains a second one — and it says nothing about *which*
 * handler is missing it. Reading each handler on its own makes "this one door
 * stopped checking" red, and names the door.
 *
 * Relies on rustfmt putting a multi-line signature's closing paren and the
 * function's own closing brace at column 0; the tree is fmt-clean and CI keeps
 * it that way. Callers guard on `null`, so a form this cannot read turns the
 * check red rather than passing over an unread function.
 */
export function rustFnBlock(source, name) {
  const start = source.search(new RegExp(`^pub(?:\\(crate\\))? async fn ${name}\\s*(?:<[^>]*>)?\\s*\\(`, 'm'));
  if (start < 0) return null;

  const rest = source.slice(start);
  const close = rest.search(/\n\}/);
  if (close < 0) return null;
  const block = rest.slice(0, close + 2);

  const head = /^pub(?:\(crate\))? async fn \w+\s*(?:<[^>]*>)?\s*\(/.exec(block);
  if (!head) return null;
  const open = head[0].lastIndexOf('(');
  let depth = 0;
  let end = open;
  while (end < block.length) {
    if (block[end] === '(') depth += 1;
    else if (block[end] === ')') {
      depth -= 1;
      if (depth === 0) break;
    }
    end += 1;
  }
  if (depth !== 0) return null;

  const brace = block.indexOf('{', end + 1);
  if (brace < 0) return null;

  return { params: block.slice(open + 1, end), body: block.slice(brace) };
}

/**
 * The signature of a top-level `fn <name>` — parameter list and return type, up
 * to (not including) the body's opening brace — or `null`.
 *
 * `rustFnBlock` only reads `pub async fn`, which leaves free helper functions to
 * the idiom `/fn list_branch_refs[\s\S]*?anyhow::Result<Vec<BranchRef>>/`. That
 * spelling asserts nothing about `list_branch_refs`: the lazy bridge walks past
 * the function's own signature into the next one that happens to return the
 * type, so changing this function's return type stays green as long as *some*
 * later function returns it. Read the one signature, assert inside it.
 *
 * Handles rustfmt's wrapped form (closing paren and `->` on their own line),
 * since the whole head is returned rather than a single line.
 */
export function rustFnHead(source, name) {
  const start = source.search(new RegExp(`^(?:pub(?:\\([^)]*\\))?\\s+)?(?:async\\s+)?fn ${name}\\b`, 'm'));
  if (start < 0) return null;
  const rest = source.slice(start);
  const open = rest.indexOf('(');
  if (open < 0) return null;

  let depth = 0;
  let end = open;
  while (end < rest.length) {
    if (rest[end] === '(') depth += 1;
    else if (rest[end] === ')') {
      depth -= 1;
      if (depth === 0) break;
    }
    end += 1;
  }
  if (depth !== 0) return null;

  const brace = rest.indexOf('{', end + 1);
  if (brace < 0) return null;
  return rest.slice(0, brace);
}

/**
 * The field block of a top-level `struct <name> { … }`, or `null`.
 *
 * The idiom this replaces is `/pub struct Foo[\s\S]*field: T/` — which does not
 * assert that `Foo` has the field at all. `[\s\S]*` happily bridges from the
 * struct's header to a field of some *other* struct further down the file, so
 * moving the field out keeps the gate green under a message that names `Foo`.
 * Verified on `SsoProviderInfo`: three of its four asserted fields could be
 * moved into a neighbouring struct with the check still passing
 * (card_c7aef378ad3d). Read the body once, assert inside it.
 *
 * Relies on rustfmt putting the struct's closing brace at column 0, the same
 * assumption `rustFnBlock` makes; a shape this cannot read returns `null` so the
 * caller fails loudly instead of asserting over an empty string.
 */
export function rustStructBody(source, name) {
  const start = source.search(new RegExp(`^(?:pub(?:\\([^)]*\\))?\\s+)?struct ${name}\\b`, 'm'));
  if (start < 0) return null;
  const rest = source.slice(start);
  const open = rest.indexOf('{');
  if (open < 0) return null;
  const close = rest.search(/\n\}/);
  if (close < 0 || close < open) return null;
  return rest.slice(open + 1, close);
}

/**
 * Split a Rust function parameter list at top-level commas.
 *
 * This is deliberately separate from `splitCallArgs`: generic type arguments
 * (`Query<HashMap<String, String>>`) add `<...>` nesting that a call expression
 * does not. Treating that comma as a parameter boundary would make a handler
 * appear to take a made-up, untyped parameter and weaken every signature-based
 * contract check built on this helper.
 */
export function splitRustParams(params) {
  const result = [];
  let depth = 0;
  let start = 0;
  let i = 0;
  while (i < params.length) {
    const ch = params[i];
    if (ch === '"') {
      i += 1;
      while (i < params.length) {
        if (params[i] === '\\') {
          i += 2;
          continue;
        }
        if (params[i] === '"') break;
        i += 1;
      }
    } else if (ch === '(' || ch === '[' || ch === '{' || ch === '<') {
      depth += 1;
    } else if (ch === ')' || ch === ']' || ch === '}' || ch === '>') {
      depth -= 1;
      if (depth < 0) return null;
    } else if (ch === ',' && depth === 0) {
      const param = params.slice(start, i).trim();
      if (param) result.push(param);
      start = i + 1;
    }
    i += 1;
  }
  if (depth !== 0) return null;
  const tail = params.slice(start).trim();
  if (tail) result.push(tail);
  return result;
}

/**
 * The declared type of one Rust parameter: the text after the top-level `:`.
 *
 * Patterns may contain their own colons (`RepoWrite { actor_id: id }`) and type
 * paths contain `::`; neither is the separator between the pattern and type.
 */
export function rustParamType(param) {
  let depth = 0;
  let i = 0;
  while (i < param.length) {
    const ch = param[i];
    if (ch === '(' || ch === '[' || ch === '{' || ch === '<') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}' || ch === '>') depth -= 1;
    else if (ch === ':' && depth === 0) {
      if (param[i + 1] === ':') {
        i += 2;
        continue;
      }
      return param.slice(i + 1).trim();
    }
    i += 1;
  }
  return null;
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

/**
 * Resolve the nest prefix each `RouteTable` row is registered under.
 *
 * A path literal in `routes.rs` is relative to its table: `"/repos/{owner}"` in
 * the `RouteTable::new("/api/v1")` chain is served at `/api/v1/repos/{owner}`.
 * Any comparison against a URL declared *elsewhere* — an `#[utoipa::path]`
 * annotation, a frontend call — is meaningless without that prefix, so it is
 * resolved here rather than assumed by each caller.
 *
 * Two hops, because the table is built in two shapes:
 *
 *   1. Directly — the nearest preceding `RouteTable::new("…")` *inside the same
 *      top-level fn*. The enclosing-fn bound is what stops a helper defined
 *      after `build_docs_routes` from inheriting that function's `""`.
 *   2. Through `.with(helper)` — `maven_layout_routes` & co. take the table as
 *      an argument, so their rows carry no `RouteTable::new` at all. Their
 *      prefix is the one at the `.with(...)` call site.
 *
 * A helper reached from two different prefixes is ambiguous and resolves to
 * `null`, as does anything the two hops cannot place. `null` means "unknown",
 * not "root": a caller comparing URLs must skip such a row out loud instead of
 * comparing it against the wrong base.
 */
function routePrefixResolver(src) {
  const fns = [];
  const fnRe = /^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/gm;
  let match;
  while ((match = fnRe.exec(src)) !== null) fns.push({ name: match[1], start: match.index });

  const tables = [];
  const tableRe = /RouteTable::new\s*\(\s*"((?:[^"\\]|\\.)*)"\s*\)/g;
  while ((match = tableRe.exec(src)) !== null) tables.push({ index: match.index, prefix: match[1] });

  /** The last entry of `list` at or before `index`, or `null`. */
  const lastBefore = (list, index, key) => {
    let found = null;
    for (const entry of list) {
      if (entry[key] > index) break;
      found = entry;
    }
    return found;
  };

  // Hop 1: the table opened in this fn, or the fn itself when there is none.
  const direct = (index) => {
    const fn = lastBefore(fns, index, 'start');
    const table = lastBefore(tables, index, 'index');
    if (table && (!fn || table.index > fn.start)) return { prefix: table.prefix };
    return { fn: fn ? fn.name : null };
  };

  // Hop 2: `.with(helper)` hands the helper the caller's table.
  const viaWith = new Map();
  const withRe = /\.with\s*\(\s*(\w+)\s*\)/g;
  while ((match = withRe.exec(src)) !== null) {
    const site = direct(match.index);
    if (site.prefix === undefined) continue;
    const helper = match[1];
    if (viaWith.has(helper) && viaWith.get(helper) !== site.prefix) {
      viaWith.set(helper, null); // reached from two prefixes — ambiguous
      continue;
    }
    viaWith.set(helper, site.prefix);
  }

  return (index) => {
    const site = direct(index);
    if (site.prefix !== undefined) return site.prefix;
    if (site.fn === null) return null;
    return viaWith.has(site.fn) ? viaWith.get(site.fn) : null;
  };
}

/**
 * Every handler path mounted by a `RouteTable` build, regardless of how the URL
 * was spelled.
 *
 * `parseRouteTable` only yields a row when the path argument is a string
 * *literal*, because the (method, path) assertions it feeds have nothing to say
 * about a row whose URL it cannot read. That filter is a blind spot for a
 * coverage question: `maven_layout_routes` / `cargo_index_routes` register their
 * rows in a `for path in CONST_ARRAY` loop, so the path argument is an
 * identifier and the whole Maven and Cargo-sparse surface is invisible to the
 * literal-only parse. A coverage gate built on that parse would report those
 * handlers as "not mounted" — i.e. silently exempt them.
 *
 * So this reads the third argument instead, which is a handler path in every
 * spelling, and reports the path literal only when there happens to be one.
 *
 * Returns `{ handler, method, path|null, prefix|null, line }` rows; one per
 * registration, so a handler mounted under several URLs appears several times.
 * `prefix` is the sub-router's nest prefix (see `routePrefixResolver`), so
 * `prefix + path` is the URL the server actually answers on; `null` means the
 * prefix could not be established, not that there is none.
 */
export function parseMountedHandlers(source) {
  const src = stripRustComments(source);
  const prefixAt = routePrefixResolver(src);
  const rows = [];
  const re = new RegExp(`\\.(${ROUTE_METHODS.join('|')})(?:_with)?\\s*\\(`, 'g');
  let match;
  while ((match = re.exec(src)) !== null) {
    const open = re.lastIndex - 1;
    const args = splitCallArgs(src, open);
    if (!args || args.length < 3) continue;
    // `(access, path, handler)` — the handler is a Rust path expression. This is
    // what separates a route registration from `map.get(k)` now that the path
    // argument is no longer required to be a literal.
    const handler = args[2].replace(/\s+/g, '');
    if (!/^(?:crate::)?[a-z_][\w:]*::[a-z_]\w*$/.test(handler)) continue;
    const pathLiteral = args[1].match(/^"((?:[^"\\]|\\.)*)"$/);
    rows.push({
      handler: handler.replace(/^crate::/, ''),
      method: match[1].toUpperCase(),
      path: pathLiteral ? pathLiteral[1] : null,
      prefix: prefixAt(match.index),
      line: src.slice(0, match.index).split('\n').length,
    });
  }
  return rows;
}

/**
 * Read `routerPath` and parse every mounted handler.
 *
 * Same fail-loud floor as `loadRouteTable`: a parser that understands nothing
 * would turn the coverage gate into a no-op that reports full coverage.
 */
export function loadMountedHandlers(routerPath) {
  const rows = parseMountedHandlers(readFileSync(routerPath, 'utf8'));
  if (rows.length < MIN_PARSED_ROUTES) {
    throw new Error(
      `Handler parser understood only ${rows.length} route registrations in ${routerPath} ` +
        `(expected at least ${MIN_PARSED_ROUTES}). The router registration form probably changed — ` +
        'fix parseMountedHandlers() in scripts/lib/rust-source.mjs rather than the checks that use it.',
    );
  }
  return rows;
}

// ── OpenAPI annotations ────────────────────────────────────────────────────
//
// Every handler declares its route twice: once as a row in the `RouteTable`,
// once as `#[utoipa::path(method, path = "…")]` above the function. The second
// copy is what the published spec is built from — and nothing compared the two,
// so a URL could be changed in the router alone and the spec would go on
// advertising the old one. Reading the annotations here is what lets the
// coverage gate compare (method, path) instead of handler names only.

/** HTTP operations `#[utoipa::path]` accepts as its leading argument. */
const UTOIPA_METHODS = ['get', 'post', 'put', 'delete', 'patch', 'head', 'options', 'trace', 'connect'];

/**
 * Lower bound on a healthy annotation sweep of `crates/rg-http/src/api`
 * (~288 handlers carry one). Same reasoning as `MIN_PARSED_ROUTES`: a parser
 * that quietly understands nothing would turn every (method, path) assertion
 * into a no-op that reports perfect agreement.
 */
const MIN_PARSED_ANNOTATIONS = 200;

/**
 * The value of a top-level `key = "…"` in an attribute body.
 *
 * Depth-aware on purpose: `responses(...)` and `params(...)` carry `= "…"`
 * pairs of their own, and a flat regex would happily read one of those as the
 * operation's path.
 */
function attributeStringValue(body, key) {
  let depth = 0;
  let i = 0;
  while (i < body.length) {
    const ch = body[i];
    if (ch === '"') {
      i += 1;
      while (i < body.length) {
        if (body[i] === '\\') {
          i += 2;
          continue;
        }
        if (body[i] === '"') break;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') depth -= 1;
    else if (depth === 0) {
      const rest = body.slice(i);
      const hit = new RegExp(`^${key}\\s*=\\s*"((?:[^"\\\\]|\\\\.)*)"`).exec(rest);
      // Guard against matching the tail of a longer identifier.
      if (hit && (i === 0 || !/[\w:]/.test(body[i - 1]))) return hit[1];
    }
    i += 1;
  }
  return null;
}

/** Whether a top-level attribute key is followed by one of `continuations`. */
function hasAttributeDeclaration(body, key, continuations) {
  let depth = 0;
  let i = 0;
  while (i < body.length) {
    const ch = body[i];
    if (ch === '"') {
      i += 1;
      while (i < body.length) {
        if (body[i] === '\\') {
          i += 2;
          continue;
        }
        if (body[i] === '"') break;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') depth -= 1;
    else if (
      depth === 0 &&
      body.startsWith(key, i) &&
      (i === 0 || !/[\w:]/.test(body[i - 1])) &&
      !/\w/.test(body[i + key.length] ?? '')
    ) {
      let next = i + key.length;
      while (/\s/.test(body[next] ?? '')) next += 1;
      if (continuations.includes(body[next])) return true;
    }
    i += 1;
  }
  return false;
}

/** The body of a top-level `key(...)` declaration, or `null` when absent. */
function attributeCallBody(body, key) {
  let depth = 0;
  let i = 0;
  while (i < body.length) {
    const ch = body[i];
    if (ch === '"') {
      i += 1;
      while (i < body.length) {
        if (body[i] === '\\') {
          i += 2;
          continue;
        }
        if (body[i] === '"') break;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') depth -= 1;
    else if (
      depth === 0 &&
      body.startsWith(key, i) &&
      (i === 0 || !/[\w:]/.test(body[i - 1])) &&
      !/\w/.test(body[i + key.length] ?? '')
    ) {
      let open = i + key.length;
      while (/\s/.test(body[open] ?? '')) open += 1;
      if (body[open] !== '(') return null;
      let callDepth = 1;
      let end = open + 1;
      while (end < body.length && callDepth > 0) {
        if (body[end] === '"') {
          end += 1;
          while (end < body.length) {
            if (body[end] === '\\') {
              end += 2;
              continue;
            }
            if (body[end] === '"') break;
            end += 1;
          }
        } else if (body[end] === '(') callDepth += 1;
        else if (body[end] === ')') callDepth -= 1;
        end += 1;
      }
      return callDepth === 0 ? body.slice(open + 1, end - 1) : null;
    }
    i += 1;
  }
  return null;
}

/**
 * The `#[utoipa::path(...)]` annotations of one Rust source file.
 *
 * `modulePath` is the Rust module the file defines (`api::collaborators`), so
 * the rows key by the same `api::module::handler` spelling the router uses.
 *
 * The annotation must sit directly above the handler — only further attributes
 * may come between. An annotation this cannot attribute to a function is
 * returned with `handler: null` rather than dropped, so the caller can fail on
 * it: a silently skipped annotation is a hole in the very comparison these rows
 * exist for.
 *
 * Returns rows carrying the annotation declarations and the attributed
 * handler's raw signature parameters. A caller can therefore compare the
 * document's claims with the handler input without maintaining a handler list.
 */
export function parseUtoipaPaths(source, modulePath, file) {
  const src = stripRustComments(source);
  const rows = [];
  const token = '#[utoipa::path(';
  let cursor = 0;
  while (true) {
    const start = src.indexOf(token, cursor);
    if (start === -1) break;

    let i = start + token.length;
    let depth = 1;
    while (i < src.length && depth > 0) {
      const ch = src[i];
      if (ch === '"') {
        i += 1;
        while (i < src.length) {
          if (src[i] === '\\') {
            i += 2;
            continue;
          }
          if (src[i] === '"') break;
          i += 1;
        }
      } else if (ch === '(') depth += 1;
      else if (ch === ')') depth -= 1;
      i += 1;
    }
    if (depth !== 0) {
      throw new Error(`Unbalanced #[utoipa::path(...)] in ${file} — the annotation form changed.`);
    }

    const body = src.slice(start + token.length, i - 1);
    cursor = i;

    const owner = /^\]\s*(?:#\[[^\]]*\]\s*)*pub(?:\(crate\))?\s+async\s+fn\s+(\w+)/.exec(src.slice(i));
    const method = new RegExp(`^\\s*(${UTOIPA_METHODS.join('|')})\\s*,`, 'i').exec(body);
    const fnBlock = owner ? rustFnBlock(src, owner[1]) : null;

    rows.push({
      handler: owner ? `${modulePath}::${owner[1]}` : null,
      method: method ? method[1].toUpperCase() : null,
      path: attributeStringValue(body, 'path'),
      signatureParams: fnBlock?.params ?? null,
      declaresParams: hasAttributeDeclaration(body, 'params', ['(']),
      paramsBody: attributeCallBody(body, 'params'),
      declaresRequestBody: hasAttributeDeclaration(body, 'request_body', ['(', '=']),
      file,
      line: src.slice(0, start).split('\n').length,
    });
  }
  return rows;
}

/** `crates/rg-http/src/api/packages/npm.rs` under `api/` → `api::packages::npm`. */
function moduleForFile(relativePath, rootModule) {
  const segments = relativePath.replace(/\.rs$/, '').split('/');
  if (segments[segments.length - 1] === 'mod') segments.pop();
  return [rootModule, ...segments].join('::');
}

/**
 * Sweep `apiDir` for `#[utoipa::path]` annotations, keyed by handler.
 *
 * Throws when the sweep comes back implausibly small or when one handler
 * carries two annotations — in both cases the assertions built on top would be
 * quietly weaker than they read, and the resolver, not the caller, is what
 * needs fixing.
 */
export function loadUtoipaPaths(apiDir, rootModule = 'api') {
  const files = [];
  const walk = (dir, prefix) => {
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.name.startsWith('.')) continue;
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(join(dir, entry.name), relative);
      else if (entry.isFile() && entry.name.endsWith('.rs')) files.push(relative);
    }
  };
  walk(apiDir, '');

  const byHandler = new Map();
  const unattributed = [];
  for (const relative of files) {
    const source = readFileSync(join(apiDir, relative), 'utf8');
    for (const row of parseUtoipaPaths(source, moduleForFile(relative, rootModule), relative)) {
      if (row.handler === null) {
        unattributed.push(`${row.file}:${row.line}`);
        continue;
      }
      if (byHandler.has(row.handler)) {
        const first = byHandler.get(row.handler);
        throw new Error(
          `${row.handler} carries two #[utoipa::path] annotations (${first.file}:${first.line} and ` +
            `${row.file}:${row.line}). Only one can reach the spec — fix the source, or teach ` +
            'loadUtoipaPaths() in scripts/lib/rust-source.mjs which one wins.',
        );
      }
      byHandler.set(row.handler, row);
    }
  }

  if (unattributed.length > 0) {
    throw new Error(
      `${unattributed.length} #[utoipa::path(...)] annotation(s) in ${apiDir} could not be attributed to a ` +
        `handler (${unattributed.join(', ')}). The annotation is expected directly above a ` +
        '`pub async fn` — fix parseUtoipaPaths() in scripts/lib/rust-source.mjs rather than ignoring them.',
    );
  }
  if (byHandler.size < MIN_PARSED_ANNOTATIONS) {
    throw new Error(
      `Annotation parser understood only ${byHandler.size} #[utoipa::path(...)] annotations in ${apiDir} ` +
        `(expected at least ${MIN_PARSED_ANNOTATIONS}). The annotation form probably changed — fix ` +
        'parseUtoipaPaths() in scripts/lib/rust-source.mjs rather than the checks that use it.',
    );
  }
  return byHandler;
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
