import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api/_base', async () => {
	const actual = await vi.importActual<typeof import('./_base.svelte')>('$lib/api/_base');
	return { ...actual, downloadApiFile: vi.fn() };
});

import RepoHeader from '../components/RepoHeader.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	renderComponent,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn> | undefined;

beforeEach(async () => {
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
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
	warn?.mockRestore();
	warn = undefined;
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

// `PUT /star` toggles server-side and the watch button cycles from the state it
// shows, so both controls mutate *relative to what they display*. A read that
// answered 5xx used to settle into "not starred" / "Watch" — the same picture a
// genuine "no" draws — which then aimed the very next click at the opposite
// mutation: a viewer who had already starred the repository would retract that
// star by clicking a button that said Star (card_da6f696b88f2).
describe('RepoHeader star and watch state availability', () => {
	it('does not draw a failed star read as "not starred"', async () => {
		repos.starred.mockRejectedValue(new Error('HTTP 500'));

		await renderHeader();
		const star = actionButton('.star-icon');

		expect(star.getAttribute('aria-label')).toBe('Star state unavailable — click to retry');
		expect(star.textContent).not.toContain('☆');
		expect(star.textContent).not.toContain('⭐');
		expect(warn).toHaveBeenCalled();
	});

	it('sends no star mutation from a state it could not read', async () => {
		repos.starred.mockRejectedValue(new Error('HTTP 500'));

		await renderHeader();
		await click(actionButton('.star-icon'));

		expect(repos.star).not.toHaveBeenCalled();
	});

	it('re-reads the star state on the click that follows a failed read', async () => {
		repos.starred.mockRejectedValueOnce(new Error('HTTP 500'));
		repos.starred.mockResolvedValue({ starred: true });

		await renderHeader();
		await click(actionButton('.star-icon'));

		const star = actionButton('.star-icon');
		expect(repos.starred).toHaveBeenCalledTimes(2);
		expect(star.textContent).toContain('⭐');
		expect(star.getAttribute('aria-label')).toBe('Unstar');

		// Only now, on a state that was actually read, may the button mutate.
		await click(star);
		expect(repos.star).toHaveBeenCalledWith('alice', 'demo');
	});

	it('does not draw a failed watch read as "not watching"', async () => {
		repos.watchStatus.mockRejectedValue(new Error('HTTP 500'));

		await renderHeader();
		const watch = actionButton('.watch-icon');

		expect(watch.textContent).toContain('Watch state unavailable');
		expect(watch.getAttribute('aria-label')).toBe('Watch state unavailable — click to retry');
		expect(warn).toHaveBeenCalled();
	});

	it('sends no watch mutation from a state it could not read', async () => {
		repos.watchStatus.mockRejectedValue(new Error('HTTP 500'));

		await renderHeader();
		await click(actionButton('.watch-icon'));

		expect(repos.watch).not.toHaveBeenCalled();
		expect(repos.unwatch).not.toHaveBeenCalled();
	});

	it('re-reads the watch state on the click that follows a failed read', async () => {
		repos.watchStatus.mockRejectedValueOnce(new Error('HTTP 500'));
		repos.watchStatus.mockResolvedValue({ watch_state: 'ignoring' });

		await renderHeader();
		await click(actionButton('.watch-icon'));

		const watch = actionButton('.watch-icon');
		expect(repos.watchStatus).toHaveBeenCalledTimes(2);
		expect(watch.textContent).toContain('Ignoring');

		// The cycle resumes from the state the server actually holds.
		await click(watch);
		expect(repos.unwatch).toHaveBeenCalledWith('alice', 'demo');
	});

	// The paired half: an answer that did arrive keeps its own meaning. Without
	// this, "always render unavailable" would pass every assertion above.
	it('still draws a server-answered "no" as a plain unstarred, unwatched header', async () => {
		await renderHeader();
		const star = actionButton('.star-icon');
		const watch = actionButton('.watch-icon');

		expect(star.getAttribute('aria-label')).toBe('Star');
		expect(star.textContent).toContain('☆');
		expect(watch.textContent).not.toContain('unavailable');
		expect(warn).not.toHaveBeenCalled();

		await click(star);
		expect(repos.star).toHaveBeenCalledWith('alice', 'demo');

		await click(actionButton('.watch-icon'));
		expect(repos.watch).toHaveBeenCalledWith('alice', 'demo', 'watching');
	});
});
