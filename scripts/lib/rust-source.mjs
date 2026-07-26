// Helpers for asserting against Rust sources from the contract checks.

/**
 * Strip Rust line/block comments from `source`, preserving string literals and newlines.
 *
 * Router assertions grep for `.route("/path", get(handler))`. Without this, a route that was
 * merely commented out still satisfies the regex — the endpoint is gone from the binary while
 * the gate stays green. Stripping comments first makes "commented out" fail like "deleted".
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
