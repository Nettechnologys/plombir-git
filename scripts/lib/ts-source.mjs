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
