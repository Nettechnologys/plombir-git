import { describe, expect, it } from 'vitest';
import { visibleTabCount } from './overflowTabs';

describe('ordered overflow navigation', () => {
  it('uses the whole row when every tab fits, without reserving an unused menu', () => {
    expect(visibleTabCount([80, 120, 100], 300, 70)).toBe(3);
    expect(visibleTabCount([], 0, 70)).toBe(0);
  });

  it('moves the rightmost tabs one at a time as space is lost', () => {
    const widths = [80, 120, 100, 90];
    expect([390, 389, 369, 269, 149].map(space => visibleTabCount(widths, space, 70)))
      .toEqual([4, 3, 2, 1, 0]);
  });

  it('keeps a prefix instead of skipping a long label to fit a later short one', () => {
    expect(visibleTabCount([80, 200, 30], 250, 70)).toBe(1);
  });

  it('accounts for fractional glyph widths without clipping the final tab', () => {
    expect(visibleTabCount([80.25, 100.5, 90], 250.7, 70)).toBe(1);
    expect(visibleTabCount([80.25, 100.5, 90], 250.75, 70)).toBe(2);
  });

  it('puts everything in More when the container cannot fit even one tab', () => {
    expect(visibleTabCount([180, 120], 90, 70)).toBe(0);
    expect(visibleTabCount([180, 120], 40, 70)).toBe(0);
  });
});
