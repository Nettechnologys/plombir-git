import { afterEach, describe, expect, it } from 'vitest';

import { formatRelativeTime, locale, pluralForm } from '.';

const BRANCH = { one: 'ветка', few: 'ветки', many: 'веток', other: 'ветки' };

afterEach(() => locale.set('en'));

describe('pluralForm', () => {
	it('picks the Russian form a count takes', () => {
		locale.set('ru');
		expect([1, 2, 5, 11, 21, 22, 25].map((n) => `${n} ${pluralForm(n, BRANCH)}`)).toEqual([
			'1 ветка',
			'2 ветки',
			'5 веток',
			'11 веток',
			'21 ветка',
			'22 ветки',
			'25 веток',
		]);
	});

	it('uses one and other in English', () => {
		locale.set('en');
		const forms = { one: 'branch', few: 'branches', many: 'branches', other: 'branches' };
		expect(pluralForm(1, forms)).toBe('branch');
		expect(pluralForm(0, forms)).toBe('branches');
		expect(pluralForm(3, forms)).toBe('branches');
	});
});

describe('formatRelativeTime', () => {
	const now = Date.parse('2026-11-24T12:00:00Z');

	it('says how long ago in the active locale', () => {
		locale.set('en');
		expect(formatRelativeTime('2026-11-24T11:55:00Z', now)).toBe('5 minutes ago');
		expect(formatRelativeTime('2026-11-21T12:00:00Z', now)).toBe('3 days ago');
		expect(formatRelativeTime('2026-11-23T12:00:00Z', now)).toBe('yesterday');
		locale.set('ru');
		expect(formatRelativeTime('2026-11-21T12:00:00Z', now)).toBe('3 дня назад');
	});

	it('falls back to the date after a year', () => {
		locale.set('en');
		expect(formatRelativeTime('2024-01-02T00:00:00Z', now)).toBe('Jan 2, 2024');
	});
});
