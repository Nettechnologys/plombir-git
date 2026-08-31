import { mount, unmount } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api/_base', async () => {
	const actual = await vi.importActual<typeof import('./_base.svelte')>('$lib/api/_base');
	return { ...actual, downloadApiFile: vi.fn() };
});

import RepoHeader from '../components/RepoHeader.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import RepoHeaderRouteHarness from '../test/RepoHeaderRouteHarness.svelte';
import { auth, repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';
import { downloadApiFile } from '$lib/api/_base';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

let rendered: RenderedComponent | undefined;
let routeHarness: { visit: (owner: string, repo: string, defaultBranch?: string) => void } | undefined;

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	repos.star.mockResolvedValue({ starred: true });
	repos.watch.mockImplementation(async (_owner: string, _repo: string, watchState: string) => ({
		watch_state: watchState,
	}));
	repos.unwatch.mockResolvedValue({ watch_state: 'not_watching' });
	vi.mocked(downloadApiFile).mockReset();
	vi.mocked(downloadApiFile).mockResolvedValue(undefined);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	routeHarness = undefined;
	await logout();
});

async function renderHeader(): Promise<void> {
	rendered = await renderComponent(RepoHeader, {
		owner: 'alice',
		repo: 'demo',
		starsCount: 3,
		defaultBranch: 'main',
	});
}

async function renderRouteHeader(): Promise<void> {
	const container = document.createElement('div');
	document.body.append(container);
	const instance = mount(RepoHeaderRouteHarness, {
		target: container,
		props: { owner: 'alice', repo: 'demo' },
	});
	await settle();

	routeHarness = instance;
	rendered = {
		container,
		destroy: async () => {
			await unmount(instance);
			container.remove();
		},
	};
}

function actionButton(marker: string): HTMLButtonElement {
	const found = element(rendered!.container, marker).closest<HTMLButtonElement>('button');
	if (!found) throw new Error(`${marker} is not inside an action button`);
	return found;
}

describe('RepoHeader async state ownership', () => {
	it('keeps the third visit archive ref when the first A response succeeds late', async () => {
		const firstA = deferred<{ default_branch: string }>();
		repos.get
			.mockReturnValueOnce(firstA.promise)
			.mockResolvedValueOnce({ default_branch: 'b-main' })
			.mockResolvedValueOnce({ default_branch: 'third-main' });

		await renderRouteHeader();
		routeHarness!.visit('bob', 'other');
		await settle();
		routeHarness!.visit('alice', 'demo');
		await settle();
		expect(repos.get).toHaveBeenCalledTimes(3);

		firstA.resolve({ default_branch: 'stale-first-main' });
		await settle();
		await click(element(rendered!.container, '.btn-code'));
		await click(element(rendered!.container, '.clone-footer-link'));

		expect(downloadApiFile).toHaveBeenCalledWith(
			'/repos/alice/demo/archive/third-main.zip',
			'demo-third-main.zip',
		);
	});

	it('keeps the third visit archive ref when the first A response fails late', async () => {
		const firstA = deferred<{ default_branch: string }>();
		repos.get
			.mockReturnValueOnce(firstA.promise)
			.mockResolvedValueOnce({ default_branch: 'b-main' })
			.mockResolvedValueOnce({ default_branch: 'third-main' });

		await renderRouteHeader();
		routeHarness!.visit('bob', 'other');
		await settle();
		routeHarness!.visit('alice', 'demo');
		await settle();

		firstA.reject(new Error('stale first visit failed'));
		await settle();
		await click(element(rendered!.container, '.btn-code'));
		await click(element(rendered!.container, '.clone-footer-link'));

		expect(downloadApiFile).toHaveBeenCalledWith(
			'/repos/alice/demo/archive/third-main.zip',
			'demo-third-main.zip',
		);
	});

	it('keeps newer star and watch clicks when both initial responses arrive late', async () => {
		const initialStar = deferred<{ starred: boolean }>();
		const initialWatch = deferred<{
			watch_state: 'not_watching' | 'watching' | 'ignoring';
		}>();
		repos.starred.mockReturnValueOnce(initialStar.promise);
		repos.watchStatus.mockReturnValueOnce(initialWatch.promise);

		await renderHeader();
		const star = actionButton('.star-icon');
		const watch = actionButton('.watch-icon');

		await click(star);
		await click(watch);
		expect(star.textContent).toContain('⭐');
		expect(element(rendered!.container, '.count').textContent).toBe('4');
		expect(watch.textContent).toContain('Watching');

		initialStar.resolve({ starred: false });
		initialWatch.resolve({ watch_state: 'not_watching' });
		await settle();

		expect(star.textContent).toContain('⭐');
		expect(element(rendered!.container, '.count').textContent).toBe('4');
		expect(watch.textContent).toContain('Watching');
	});

	it('keeps the Watch -> Watching -> Ignoring -> Watch cycle aligned with the API', async () => {
		await renderHeader();
		const watch = actionButton('.watch-icon');

		await click(watch);
		expect(watch.textContent).toContain('Watching');
		expect(repos.watch).toHaveBeenLastCalledWith('alice', 'demo', 'watching');

		await click(watch);
		expect(watch.textContent).toContain('Ignoring');
		expect(repos.watch).toHaveBeenLastCalledWith('alice', 'demo', 'ignoring');

		await click(watch);
		expect(watch.textContent).toContain('Watch');
		expect(repos.unwatch).toHaveBeenCalledWith('alice', 'demo');
	});

	it('does not send a second star mutation while the first one owns the control', async () => {
		const mutation = deferred<{ starred: boolean }>();
		repos.star.mockReturnValueOnce(mutation.promise);
		await renderHeader();
		const star = actionButton('.star-icon');

		await click(star);
		expect(star.disabled).toBe(true);
		expect(star.getAttribute('aria-busy')).toBe('true');
		await click(star);
		expect(repos.star).toHaveBeenCalledTimes(1);

		mutation.resolve({ starred: true });
		await settle();
		expect(star.disabled).toBe(false);
		expect(star.getAttribute('aria-busy')).toBe('false');
	});
});
