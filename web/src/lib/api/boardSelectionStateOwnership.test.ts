import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Component } from 'svelte';

import StandaloneBoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import { LatestRepositoryResourceRequestFence } from '../asyncStateOwnership';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, issues, repos, resetTestClient } from '../test/client';
import { click, renderComponent, settle, type RenderedComponent } from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason: Error) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason: Error) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

const firstBoard = { id: 10, name: 'First board', description: null };
const secondBoard = { id: 20, name: 'Second board', description: null };

function fullBoard(board: typeof firstBoard, cardId: number, note: string) {
	return {
		board,
		columns: [
			{
				column: { id: board.id + 1, board_id: board.id, name: 'Todo', position: 0 },
				cards: [
					{
						id: cardId,
						column_id: board.id + 1,
						issue_id: null,
						note,
						position: 0,
						issue: null,
					},
				],
			},
		],
	};
}

const firstResponse = fullBoard(firstBoard, 101, 'first card');
const secondResponse = fullBoard(secondBoard, 201, 'second card');

type BoardSurface = {
	name: string;
	component: Component<any>;
	path: string;
	tabSelector: string;
	deleteSelector: string;
};

const surfaces: BoardSurface[] = [
	{
		name: 'standalone board',
		component: StandaloneBoardPage,
		path: '/alice/demo/boards',
		tabSelector: '.tab',
		deleteSelector: '.card-actions button[title="Delete"]',
	},
];

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
	repos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'admin' });
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	boards.list.mockResolvedValue([firstBoard, secondBoard]);
	boards.get.mockResolvedValue(firstResponse);
	issues.list.mockResolvedValue({ data: [] });
	boards.deleteCard.mockResolvedValue(undefined);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

async function renderSurface(surface: BoardSurface): Promise<void> {
	setTestPage(surface.path, { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(surface.component);
	expect(rendered.container.textContent).toContain('first card');
}

function tab(surface: BoardSurface, label: string): HTMLElement {
	const found = Array.from(rendered!.container.querySelectorAll<HTMLElement>(surface.tabSelector)).find(
		(candidate) => candidate.textContent?.includes(label),
	);
	if (!found) throw new Error(`${surface.name} is missing tab "${label}"`);
	return found;
}

describe('repository resource request fence', () => {
	it('requires the same generation, repository route, and resource identity', () => {
		const fence = new LatestRepositoryResourceRequestFence<number>();
		const claim = fence.begin('alice', 'demo', firstBoard.id);

		expect(fence.owns(claim, 'alice', 'demo', firstBoard.id)).toBe(true);
		expect(fence.owns(claim, 'bob', 'demo', firstBoard.id)).toBe(false);
		expect(fence.owns(claim, 'alice', 'other', firstBoard.id)).toBe(false);
		expect(fence.owns(claim, 'alice', 'demo', secondBoard.id)).toBe(false);

		fence.begin('alice', 'demo', secondBoard.id);
		expect(fence.owns(claim, 'alice', 'demo', firstBoard.id)).toBe(false);
	});
});

describe.each(surfaces)('$name selection ownership', (surface) => {
	it('keeps the latest board when the older successful response finishes last', async () => {
		await renderSurface(surface);
		const oldDelete = rendered!.container.querySelector<HTMLElement>(surface.deleteSelector)!;
		const stale = deferred<typeof firstResponse>();
		const current = deferred<typeof secondResponse>();
		boards.get.mockReturnValueOnce(stale.promise).mockReturnValueOnce(current.promise);

		await click(tab(surface, firstBoard.name));
		await click(tab(surface, secondBoard.name));
		expect(rendered!.container.querySelector('.card')).toBeNull();
		expect(rendered!.container.querySelector('.board-selection-loading')).not.toBeNull();

		// Keep a reference to the old control and dispatch after the transition:
		// Svelte must have detached it, and no request may escape from that stale UI.
		expect(oldDelete.isConnected).toBe(false);
		await click(oldDelete);
		expect(boards.deleteCard).not.toHaveBeenCalled();

		current.resolve(secondResponse);
		await settle();
		stale.resolve(firstResponse);
		await settle();

		expect(rendered!.container.textContent).toContain('second card');
		expect(rendered!.container.textContent).not.toContain('first card');
	});

	it('does not let a stale failure publish or release the current loading claim', async () => {
		await renderSurface(surface);
		const stale = deferred<typeof firstResponse>();
		const current = deferred<typeof secondResponse>();
		boards.get.mockReturnValueOnce(stale.promise).mockReturnValueOnce(current.promise);

		await click(tab(surface, firstBoard.name));
		await click(tab(surface, secondBoard.name));
		stale.reject(new Error('stale board failure'));
		await settle();

		expect(rendered!.container.querySelector('.board-selection-loading')).not.toBeNull();
		expect(rendered!.container.textContent).not.toContain('stale board failure');

		current.resolve(secondResponse);
		await settle();
		expect(rendered!.container.textContent).toContain('second card');
		expect(rendered!.container.textContent).not.toContain('stale board failure');
	});
});
