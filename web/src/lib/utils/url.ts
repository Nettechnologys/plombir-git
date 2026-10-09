/**
 * Whether `value` is an absolute `http`/`https` URL, i.e. one the page may hand
 * to an `href`.
 *
 * Commit status `target_url`s come from many places. The backend refuses
 * non-`http(s)` values on new rows — `javascript:`, `data:`, protocol-relative
 * and relative URLs — but rows written before that rule existed are still
 * served, and a status row can name any string. Binding one straight to an
 * `href` is exactly the sink the rule exists to close, so both directions go
 * through one predicate instead of each page trusting `{#if status.target_url}`.
 *
 * `new URL` follows the same URL parsing as the browser: it strips tabs and
 * newlines, so `java\nscript:` is recognised as `javascript:` and refused,
 * rather than slipping past a `startsWith('http')` check here and being
 * normalised into a live script URL by the anchor.
 */
export function isHttpUrl(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  return parsed.protocol === 'http:' || parsed.protocol === 'https:';
}
