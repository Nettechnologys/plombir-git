/**
 * Line anchors in the code view: `#L10` and `#L10-L20` (card_61e77c8abec1).
 *
 * Nothing in the app could link to a line before — the numbers were text.
 */

export interface LineRange {
  start: number;
  end: number;
}

/** The range a URL hash names, or `null` for any other hash. */
export function parseLineHash(hash: string): LineRange | null {
  const match = /^#L(\d+)(?:-L(\d+))?$/.exec(hash);
  if (!match) return null;
  const first = Number(match[1]);
  const second = match[2] === undefined ? first : Number(match[2]);
  if (first < 1 || second < 1) return null;
  return { start: Math.min(first, second), end: Math.max(first, second) };
}

/** The hash for `range`: `#L7` for one line, `#L7-L9` for several. */
export function formatLineHash(range: LineRange): string {
  return range.start === range.end ? `#L${range.start}` : `#L${range.start}-L${range.end}`;
}

/**
 * The range after a click on line `line`. A shift-click extends the current
 * selection from its first line; any other click selects the one line.
 */
export function selectLine(current: LineRange | null, line: number, extend: boolean): LineRange {
  if (extend && current) {
    return { start: Math.min(current.start, line), end: Math.max(current.start, line) };
  }
  return { start: line, end: line };
}
