import { vi } from 'vitest';

import { pageState } from './page-state.svelte';

export { pageState };

export function setTestPage(
	path: string,
	params: Record<string, string>,
	data: Record<string, unknown> = {},
): void {
	Object.assign(pageState, {
		data,
		params,
		route: { id: null },
		state: {},
		status: 200,
		url: new URL(path, 'https://plombir-git.test'),
	});
}

export const navigation = {
	afterNavigate: vi.fn(),
	beforeNavigate: vi.fn(),
	disableScrollHandling: vi.fn(),
	goto: vi.fn(),
	invalidate: vi.fn(),
	invalidateAll: vi.fn(),
	onNavigate: vi.fn(),
	preloadCode: vi.fn(),
	preloadData: vi.fn(),
	pushState: vi.fn(),
	replaceState: vi.fn(),
};
