import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import BoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, issues, repos, resetTestClient } from '../test/client';
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
						note: `${label} first card`,
						position: 0,
						created_at: '2026-08-31T00:00:00Z',
						updated_at: '2026-08-31T00:00:00Z',
						issue: null,
					},
					{
						id: value.id * 100 + 2,
						column_id: column.id,
						issue_id: null,
						note: `${label} second card`,
						position: 1,
						created_at: '2026-08-31T00:00:00Z',
						updated_at: '2026-08-31T00:00:00Z',
						issue: null,
					},
				],
			},
		],
	};
}

function issue(id: number, title: string) {
	return {
		id,
		number: id,
		title,
		state: 'open',
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

function cardWith(text: string): HTMLElement {
	const found = Array.from(rendered!.container.querySelectorAll<HTMLElement>('.card')).find(
		(candidate) => candidate.textContent?.includes(text),
	);
	if (!found) throw new Error(`Rendered board is missing card "${text}"`);
	return found;
}

function moveDown(text: string): HTMLButtonElement {
	const button = cardWith(text).querySelector<HTMLButtonElement>('button[title="Move card down"]');
	if (!button) throw new Error(`Rendered board is missing the move-down control for "${text}"`);
	return button;
}

describe('standalone board repository-route ownership', () => {
	it('reloads without remount and rejects parent data from the first A -> B -> A visit', async () => {
		const staleBoard = board(10, 'Stale board');
		const middleBoard = board(20, 'Middle board');
		const currentBoard = board(30, 'Current board');
		const firstBoards = deferred<ReturnType<typeof board>[]>();
		const firstIssues = deferred<{ data: ReturnType<typeof issue>[] }>();
		boards.list
			.mockReturnValueOnce(firstBoards.promise)
			.mockResolvedValueOnce([middleBoard])
			.mockResolvedValueOnce([currentBoard]);
		issues.list
			.mockReturnValueOnce(firstIssues.promise)
			.mockResolvedValueOnce({ data: [issue(20, 'Middle issue')] })
			.mockResolvedValueOnce({ data: [issue(30, 'Current issue')] });
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) => {
			if (id === middleBoard.id) return fullBoard(middleBoard, 'middle');
			if (id === currentBoard.id) return fullBoard(currentBoard, 'current');
			return fullBoard(staleBoard, 'stale');
		});

		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BoardPage);
		setTestPage('/bob/other/boards', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		await settle();

		expect(boards.list).toHaveBeenCalledTimes(3);
		expect(issues.list).toHaveBeenCalledTimes(3);
		expect(rendered.container.textContent).toContain('current first card');

		firstBoards.resolve([staleBoard]);
		firstIssues.resolve({ data: [issue(10, 'Stale issue')] });
		await settle();

		expect(rendered.container.textContent).toContain('Current board');
		expect(rendered.container.textContent).toContain('current first card');
		expect(rendered.container.textContent).not.toContain('Stale board');
		expect(rendered.container.textContent).not.toContain('stale first card');
		expect(boards.get).toHaveBeenCalledTimes(2);

		await click(cardWith('current first card').querySelector<HTMLButtonElement>('button[title="Edit"]')!);
		expect(rendered.container.textContent).toContain('Current issue');
		expect(rendered.container.textContent).not.toContain('Stale issue');
	});

	it('does not let a stale parent failure publish or release the current loading claim', async () => {
		const staleBoard = board(10, 'Stale board');
		const middleBoard = board(20, 'Middle board');
		const currentBoard = board(30, 'Current board');
		const staleIssues = deferred<{ data: ReturnType<typeof issue>[] }>();
		const currentBoards = deferred<ReturnType<typeof board>[]>();
		const currentIssues = deferred<{ data: ReturnType<typeof issue>[] }>();
		boards.list
			.mockResolvedValueOnce([staleBoard])
			.mockResolvedValueOnce([middleBoard])
			.mockReturnValueOnce(currentBoards.promise);
		issues.list
			.mockReturnValueOnce(staleIssues.promise)
			.mockResolvedValueOnce({ data: [] })
			.mockReturnValueOnce(currentIssues.promise);
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) =>
			id === middleBoard.id
				? fullBoard(middleBoard, 'middle')
				: fullBoard(currentBoard, 'current'),
		);

		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BoardPage);
		setTestPage('/bob/other/boards', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		await settle();

		staleIssues.reject(new Error('stale parent failure'));
		await settle();
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
		expect(rendered.container.querySelector('.loading-text')).not.toBeNull();

		currentBoards.resolve([currentBoard]);
		currentIssues.resolve({ data: [] });
		await settle();
		expect(rendered.container.textContent).toContain('current first card');
		expect(rendered.container.textContent).not.toContain('stale parent failure');
	});

	it('keeps a current-route mutation busy after the previous visit finishes', async () => {
		const firstRouteBoard = board(10, 'First route board');
		const currentRouteBoard = board(20, 'Current route board');
		const firstMutation = deferred<{ status: string }>();
		const currentMutation = deferred<{ status: string }>();
		boards.list.mockImplementation(async (owner: string) =>
			owner === 'alice' ? [firstRouteBoard] : [currentRouteBoard],
		);
		issues.list.mockResolvedValue({ data: [] });
		boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) =>
			id === firstRouteBoard.id
				? fullBoard(firstRouteBoard, 'first route')
				: fullBoard(currentRouteBoard, 'current route'),
		);
		boards.reorderCards
			.mockReturnValueOnce(firstMutation.promise)
			.mockReturnValueOnce(currentMutation.promise);

		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BoardPage);
		await click(moveDown('first route first card'));

		setTestPage('/bob/other/boards', { owner: 'bob', repo: 'other' });
		await settle();
		await click(moveDown('current route first card'));
		expect(boards.reorderCards).toHaveBeenCalledTimes(2);

		firstMutation.resolve({ status: 'ok' });
		await settle();
		expect(moveDown('current route first card').disabled).toBe(true);
		expect(rendered.container.textContent).not.toContain('first route second card');
		expect(boards.get).toHaveBeenCalledTimes(2);

		currentMutation.resolve({ status: 'ok' });
		await settle();
		expect(moveDown('current route first card').disabled).toBe(false);
		expect(boards.get).toHaveBeenCalledTimes(3);
	});
});
