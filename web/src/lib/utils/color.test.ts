import { describe, expect, it } from 'vitest';

import { safeHexColor } from './color';

describe('safeHexColor', () => {
	it('normalises a valid six-digit hex colour', () => {
		expect(safeHexColor('#ABCDEF', '#000000')).toBe('#abcdef');
		expect(safeHexColor('#ff0000', '#000000')).toBe('#ff0000');
	});

	it('refuses values that would escape the style declaration', () => {
		// What the old `starts_with('#') && len() == 7` server check accepted.
		expect(safeHexColor('#0;x:1;', '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('#ff0000; background-image: url(https://evil.example/p.png)', '#6366f1')).toBe('#6366f1');
	});

	it('falls back for anything that is not a string of the exact shape', () => {
		expect(safeHexColor(undefined, '#6366f1')).toBe('#6366f1');
		expect(safeHexColor(null, '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('red', '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('#12345', '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('#1234567', '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('1234567', '#6366f1')).toBe('#6366f1');
		expect(safeHexColor('#12345g', '#6366f1')).toBe('#6366f1');
	});

	it('passes the caller fallback through unchanged', () => {
		expect(safeHexColor('', 'var(--text-muted)')).toBe('var(--text-muted)');
	});
});
