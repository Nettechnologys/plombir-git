import { describe, expect, it, vi } from 'vitest';

import { parseServerTime } from './serverTime';

// A zone-less value read as local time is only wrong away from UTC, and CI runs
// in UTC: pin a zone with an offset, or these cases pass on the bug too.
vi.stubEnv('TZ', 'Europe/Prague');

describe('parseServerTime', () => {
	it('runs in a zone where local time is not UTC', () => {
		expect(new Date('2026-10-17T11:56:56').getTimezoneOffset()).not.toBe(0);
	});

	it.each([
		['2026-10-17 11:56:56.597504074', '2026-10-17T11:56:56.597Z'],
		['2026-10-17 11:56:56', '2026-10-17T11:56:56.000Z'],
		['2026-10-17T11:56:56.5', '2026-10-17T11:56:56.500Z'],
		['2026-10-17 11:56', '2026-10-17T11:56:00.000Z'],
	])('reads the zone-less %s as UTC', (value, expected) => {
		expect(parseServerTime(value).toISOString()).toBe(expected);
	});

	it.each([
		['2026-10-17T11:56:56Z', '2026-10-17T11:56:56.000Z'],
		['2026-10-17T13:56:56+02:00', '2026-10-17T11:56:56.000Z'],
	])('keeps the zone %s carries', (value, expected) => {
		expect(parseServerTime(value).toISOString()).toBe(expected);
	});

	it.each([undefined, null])('answers an absent value (%s) as new Date did, not with a throw', (value) => {
		const absent = value as unknown as string;
		expect(parseServerTime(absent).getTime()).toEqual(new Date(absent).getTime());
	});

	it('leaves a value it cannot read invalid rather than guessing', () => {
		expect(Number.isNaN(parseServerTime('not a date').getTime())).toBe(true);
	});
});
