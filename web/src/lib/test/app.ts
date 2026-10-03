import { vi } from 'vitest';

type TestPage = {
	data: Record<string, unknown>;
	params: Record<string, string>;
	route: { id: string | null };
	state: Record<string, unknown>;
	status: number;
	url: URL;
};

const subscribers = new Set<(page: TestPage) => void>();

export const pageState: TestPage = {
	data: {},
	params: {},
	route: { id: null },
	state: {},
	status: 200,
	url: new URL('https://plombir-git.test/'),
};

export const pageStore = {
	subscribe(run: (page: TestPage) => void): () => void {
		subscribers.add(run);
		run(pageState);
		return () => subscribers.delete(run);
	},
};

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
	for (const run of subscribers) run(pageState);
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
