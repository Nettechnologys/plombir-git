import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import IssueBoardPage from '../../routes/[owner]/[repo]/issues/board/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, repos, resetTestClient } from '../test/client';
import { click, renderComponent, settle, type RenderedComponent } from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function board(id: number, name: string) {
	return {
		id,
		repo_id: id * 10,
		org_id: null,
		name,
		description: null,
		created_by: 1,
		created_at: '2026-08-31T00:00:00Z',
		updated_at: '2026-08-31T00:00:00Z',
	};
}

function fullBoard(value: ReturnType<typeof board>, label: string) {
	const column = {
		id: value.id * 10 + 1,
		board_id: value.id,
		name: 'Todo',
		color: null,
		position: 0,
		created_at: '2026-08-31T00:00:00Z',
	};
	return {
		board: value,
		columns: [
			{
				column,
				cards: [
					{
						id: value.id * 100 + 1,
						column_id: column.id,
						issue_id: null,
						note: `${label} card`,
						position: 0,
						created_at: '2026-08-31T00:00:00Z',
						updated_at: '2026-08-31T00:00:00Z',
						issue: null,
					},
				],
			},
		],
	};
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
	repos.get.mockResolvedValue({ default_branch: 'main' });
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

function deleteCard(): HTMLButtonElement {
	const button = rendered!.container.querySelector<HTMLButtonElement>('.card-delete');
	if (!button) throw new Error('Rendered issue board is missing the card delete control');
	return button;
}

describe('issue board repository-route ownership', () => {
	it('reloads without remount and rejects parent data from the first A -> B -> A visit', async () => {
		const staleBoard = board(10, 'Stale board');
		const middleBoard = board(20, 'Middle board');
		const currentBoard = board(30, 'Current board');
		const firstBoards = deferred<ReturnType<typeof board>[]>();
		boards.list
			.mockReturnValueOnce(firstBoards.promise)
			.mockResolvedValueOnce([middleBoard])
			.mockResolvedValueOnce([currentBoard]);
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) => {
			if (id === middleBoard.id) return fullBoard(middleBoard, 'middle');
			if (id === currentBoard.id) return fullBoard(currentBoard, 'current');
			return fullBoard(staleBoard, 'stale');
		});

		setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(IssueBoardPage);
		setTestPage('/bob/other/issues/board', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
		await settle();

		expect(boards.list).toHaveBeenCalledTimes(3);
		expect(rendered.container.textContent).toContain('Current board');
		expect(rendered.container.textContent).toContain('current card');

		firstBoards.resolve([staleBoard]);
		await settle();

		expect(rendered.container.textContent).toContain('Current board');
		expect(rendered.container.textContent).toContain('current card');
		expect(rendered.container.textContent).not.toContain('Stale board');
		expect(rendered.container.textContent).not.toContain('stale card');
		expect(boards.get).toHaveBeenCalledTimes(2);
	});

	it('does not let a stale parent failure publish or release the current loading claim', async () => {
		const middleBoard = board(20, 'Middle board');
		const currentBoard = board(30, 'Current board');
		const staleBoards = deferred<ReturnType<typeof board>[]>();
		const currentBoards = deferred<ReturnType<typeof board>[]>();
		boards.list
			.mockReturnValueOnce(staleBoards.promise)
			.mockResolvedValueOnce([middleBoard])
			.mockReturnValueOnce(currentBoards.promise);
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) =>
			id === middleBoard.id
				? fullBoard(middleBoard, 'middle')
				: fullBoard(currentBoard, 'current'),
		);

		setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(IssueBoardPage);
		setTestPage('/bob/other/issues/board', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
		await settle();

		staleBoards.reject(new Error('stale parent failure'));
		await settle();
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
		expect(rendered.container.querySelector('.loading-text')).not.toBeNull();

		currentBoards.resolve([currentBoard]);
		await settle();
		expect(rendered.container.textContent).toContain('current card');
		expect(rendered.container.textContent).not.toContain('stale parent failure');
	});

	it('keeps a current-route mutation busy after the previous visit finishes', async () => {
		const firstRouteBoard = board(10, 'First route board');
		const currentRouteBoard = board(20, 'Current route board');
		const firstMutation = deferred<void>();
		const currentMutation = deferred<void>();
		boards.list.mockImplementation(async (owner: string) =>
			owner === 'alice' ? [firstRouteBoard] : [currentRouteBoard],
		);
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) =>
			id === firstRouteBoard.id
				? fullBoard(firstRouteBoard, 'first route')
				: fullBoard(currentRouteBoard, 'current route'),
		);
		boards.deleteCard
			.mockReturnValueOnce(firstMutation.promise)
			.mockReturnValueOnce(currentMutation.promise);

		setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(IssueBoardPage);
		await click(deleteCard());

		setTestPage('/bob/other/issues/board', { owner: 'bob', repo: 'other' });
		await settle();
		await click(deleteCard());
		expect(boards.deleteCard).toHaveBeenCalledTimes(2);

		firstMutation.resolve();
		await settle();
		expect(deleteCard().disabled).toBe(true);
		expect(rendered.container.textContent).toContain('current route card');
		expect(rendered.container.textContent).not.toContain('first route card');
		expect(boards.get).toHaveBeenCalledTimes(2);

		currentMutation.resolve();
		await settle();
		expect(deleteCard().disabled).toBe(false);
		expect(boards.get).toHaveBeenCalledTimes(3);
	});
});
