import { describe, expect, it } from 'vitest';

import { isHttpUrl } from './url';

describe('isHttpUrl', () => {
	it('accepts absolute http and https URLs', () => {
		expect(isHttpUrl('http://ci.example.com/build/1')).toBe(true);
		expect(isHttpUrl('https://ci.example.com/build/1')).toBe(true);
	});

	it('refuses schemes an anchor would execute or smuggle', () => {
		expect(isHttpUrl('javascript:alert(1)')).toBe(false);
		expect(isHttpUrl('JavaScript:alert(1)')).toBe(false);
		expect(isHttpUrl('data:text/html,<script>alert(1)</script>')).toBe(false);
		expect(isHttpUrl('vbscript:msgbox(1)')).toBe(false);
		expect(isHttpUrl('file:///etc/passwd')).toBe(false);
	});

	it('refuses what new URL would normalise into a script URL', () => {
		// `new URL` strips these, so the anchor would resolve the same string.
		expect(isHttpUrl('java\nscript:alert(1)')).toBe(false);
		expect(isHttpUrl('java\tscript:alert(1)')).toBe(false);
	});

	it('refuses relative and protocol-relative URLs', () => {
		expect(isHttpUrl('/builds/1')).toBe(false);
		expect(isHttpUrl('//ci.example.com/build/1')).toBe(false);
		expect(isHttpUrl('ci.example.com/build/1')).toBe(false);
	});

	it('refuses values that are not strings', () => {
		expect(isHttpUrl(undefined)).toBe(false);
		expect(isHttpUrl(null)).toBe(false);
		expect(isHttpUrl(42)).toBe(false);
		expect(isHttpUrl('')).toBe(false);
	});
});
