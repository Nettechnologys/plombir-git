import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RepoHeader from '../components/RepoHeader.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';

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
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
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

function actionButton(marker: string): HTMLButtonElement {
	const found = element(rendered!.container, marker).closest<HTMLButtonElement>('button');
	if (!found) throw new Error(`${marker} is not inside an action button`);
	return found;
}

describe('RepoHeader async state ownership', () => {
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
