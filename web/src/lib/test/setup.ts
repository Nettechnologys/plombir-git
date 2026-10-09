import { beforeEach, vi } from 'vitest';

vi.mock('$app/stores', async () => {
	const { pageStore } = await import('$lib/test/app');
	return { page: pageStore };
});

vi.mock('$app/state', async () => {
	const { pageState } = await import('$lib/test/app');
	return {
		navigating: null,
		page: pageState,
		updated: { check: vi.fn(), current: false },
	};
});

vi.mock('$app/navigation', async () => {
	const { navigation } = await import('$lib/test/app');
	return navigation;
});

vi.mock('$app/environment', () => ({
	browser: true,
	building: false,
	dev: false,
	version: 'test',
}));

vi.mock('$lib/api/client.svelte', async () => import('$lib/test/client'));

// Pages confirm irreversible actions in their own `ConfirmModal`, never in a
// native dialog (card_4c186d530f59): a browser that suppresses dialogs answers
// `false` without showing anything. Any `window.confirm()` fails the test.
beforeEach(() => {
	window.confirm = () => {
		throw new Error('window.confirm() was called; confirm through ConfirmModal');
	};
});
