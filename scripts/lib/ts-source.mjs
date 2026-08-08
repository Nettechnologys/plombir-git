// Helpers for asserting against TypeScript sources from the contract checks.
//
// The Rust side of every frontend/backend contract already has `rustStructBody`
// for this; the client side had nothing, so the checks kept the very idiom that
// helper exists to replace — see below.

/**
 * The member block of a top-level `interface <name> { … }`, or `null`.
 *
 * The idiom this replaces is `/interface Foo[\s\S]*field: T/`, which does not
 * assert that `Foo` declares the field: `[\s\S]*` bridges from the interface
 * header to a member of some *other* declaration further down the file, so
 * moving the field out keeps the gate green under a message naming `Foo`. It is
 * the TypeScript twin of the struct bridge documented on `rustStructBody`, and
 * it sat on the same lines — a check asserting a DTO on both sides of the wire
 * had the same hole in both halves.
 *
 * Relies on prettier putting a top-level interface's closing brace at column 0
 * (nested object types are indented, so they cannot end the block early). A
 * shape this cannot read returns `null` so the caller fails loudly instead of
 * asserting over an empty string.
 */
export function tsInterfaceBody(source, name) {
  const start = source.search(new RegExp(`^(?:export\\s+)?(?:declare\\s+)?interface ${name}\\b`, 'm'));
  if (start < 0) return null;
  const rest = source.slice(start);
  const open = rest.indexOf('{');
  if (open < 0) return null;
  const close = rest.search(/\n\}/);
  if (close < 0 || close < open) return null;
  return rest.slice(open + 1, close);
}

/**
 * The body of a `function <name>(…) { … }`, or `null`.
 *
 * The function twin of `tsInterfaceBody`, and the client-side twin of
 * `rustFnBlock`. It replaces asserting a rule against the whole file: a policy
 * expressed as `/\[A-Z\]/.test(source)` under a message naming one validator is
 * satisfied by *any* occurrence of that literal — and a Svelte page that also
 * carries the policy as an HTML `pattern=` attribute has a second copy of every
 * character class sitting right there. Deleting the rule from the validator
 * then leaves the gate green, quoting the attribute back at itself
 * (card_a08ef8308236).
 *
 * The closing brace is matched at the header's own indentation, which is what
 * lets this read a function nested in a Svelte `<script>` block as well as a
 * top-level one; prettier keeps that column, and CI keeps the tree formatted.
 * A shape this cannot read returns `null` so the caller fails loudly rather
 * than asserting over an empty string.
 */
export function tsFunctionBody(source, name) {
  const header = new RegExp(
    `^([ \\t]*)(?:export\\s+)?(?:default\\s+)?(?:async\\s+)?function\\s+${name}\\s*(?:<[^>]*>)?\\s*\\(`,
    'm',
  ).exec(source);
  if (header === null) return null;

  const rest = source.slice(header.index);
  let depth = 0;
  let i = header[0].length - 1;
  while (i < rest.length) {
    if (rest[i] === '(') depth += 1;
    else if (rest[i] === ')') {
      depth -= 1;
      if (depth === 0) break;
    }
    i += 1;
  }
  if (depth !== 0) return null;

  const open = rest.indexOf('{', i + 1);
  if (open < 0) return null;
  const close = rest.search(new RegExp(`\\n${header[1]}\\}`));
  if (close < 0 || close < open) return null;
  return rest.slice(open + 1, close);
}
