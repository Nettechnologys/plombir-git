import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	qs: vi.fn(() => ''),
	request: vi.fn(),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import { packages } from './packages';

function packageSummary(name: string) {
	return {
		id: 1,
		name,
		description: null,
		homepage: null,
		version_count: 1,
		latest_version: '1.0.0',
		download_count: 0,
		keywords: null,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
});

afterEach(() => {
	vi.restoreAllMocks();
});

describe('package list availability', () => {
	it('returns and logs the registry types whose requests did not answer', async () => {
		const failure = new Error('registry unavailable');
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		base.request
			.mockResolvedValueOnce({
				registries: [
					{ package_type: 'npm', enabled: true },
					{ package_type: 'cargo', enabled: true },
				],
			})
			.mockResolvedValueOnce({ packages: [packageSummary('visible-package')] })
			.mockRejectedValueOnce(failure);

		const result = await packages.list('alice', 'demo');

		expect(result.data).toEqual([
			expect.objectContaining({ name: 'visible-package', format: 'npm' }),
		]);
		expect(result.pagination.total).toBe(1);
		expect(result.failedRegistryTypes).toEqual(['cargo']);
		expect(warn).toHaveBeenCalledExactlyOnceWith(
			'Could not load packages for registry cargo:',
			failure,
		);
	});
});
