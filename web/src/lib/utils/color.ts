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

/**
 * Black or white, whichever reads better on `background` (a `#rrggbb` string,
 * as {@link safeHexColor} returns). WCAG relative luminance, with the
 * crossover where both reach the same contrast ratio.
 */
export function readableTextOn(background: string): '#000000' | '#ffffff' {
  const channel = (offset: number) => {
    const value = parseInt(background.slice(offset, offset + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const luminance = 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
  return luminance > 0.179 ? '#000000' : '#ffffff';
}
