/// Colours a server value is allowed to become in a `style` attribute.

const HEX_COLOR = /^#[0-9a-f]{6}$/i;

/**
 * Return `value` as a `#rrggbb` string, or `fallback` when it is anything else.
 *
 * Label colours and board-column colours are written straight into `style`
 * attributes (`background-color: {color}`, `border-top: 3px solid {color}`),
 * and the app's CSP allows `style-src 'unsafe-inline'`. A value that closes its
 * CSS declaration — the old server check accepted `#0;x:1;`, which is seven
 * characters starting with `#` — therefore becomes CSS of the attacker's
 * choosing in the reader's page. The server validates new writes, but rows can
 * predate that rule (imports from other forges, direct database writes), so
 * every interpolation goes through this helper rather than trusting the row.
 *
 * Case is normalised so a stored `#ABCDEF` and `#abcdef` render identically;
 * `fallback` is placed in the attribute as-is and is expected to be a literal
 * colour constant chosen by the caller.
 */
export function safeHexColor(value: unknown, fallback: string): string {
  if (typeof value !== 'string' || !HEX_COLOR.test(value)) return fallback;
  return value.toLowerCase();
}
