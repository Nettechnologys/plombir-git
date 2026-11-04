/** Keep the longest ordered prefix that fits, reserving More only on overflow. */
export function visibleTabCount(widths: readonly number[], available: number, moreWidth: number): number {
  if (widths.reduce((total, width) => total + width, 0) <= available) return widths.length;
  const budget = Math.max(0, available - moreWidth);
  let used = 0;
  let count = 0;
  for (const width of widths) {
    if (used + width > budget) break;
    used += width;
    count += 1;
  }
  return count;
}
