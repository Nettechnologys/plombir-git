import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import BoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, issues, repos, resetTestClient } from '../test/client';
import {
	change,
	click,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const board = {
	id: 10,
	repo_id: 20,
	org_id: null,
	name: 'Sprint',
	description: null,
	created_by: 1,
	created_at: '2026-08-26T00:00:00Z',
	updated_at: '2026-08-26T00:00:00Z',
};

const firstColumn = {
	id: 11,
	board_id: board.id,
	name: 'Todo',
	color: null,
	position: 0,
	created_at: '2026-08-26T00:00:00Z',
};

const secondColumn = {
	...firstColumn,
	id: 12,
	name: 'Done',
	position: 1,
};

const cards = [
	{
		id: 101,
		column_id: firstColumn.id,
		issue_id: null,
		note: 'First card',
		position: 0,
		created_at: '2026-08-26T00:00:00Z',
		updated_at: '2026-08-26T00:00:00Z',
		issue: null,
	},
	{
		id: 102,
		column_id: firstColumn.id,
		issue_id: null,
		note: 'Second card',
		position: 1,
		created_at: '2026-08-26T00:00:00Z',
		updated_at: '2026-08-26T00:00:00Z',
		issue: null,
	},
];

const fullBoard = {
	board,
	columns: [
		{ column: firstColumn, cards },
		{ column: secondColumn, cards: [] },
	],
};

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
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
	boards.list.mockResolvedValue([board]);
	boards.get.mockResolvedValue(fullBoard);
	issues.list.mockResolvedValue({ data: [] });
	boards.moveCard.mockResolvedValue(cards[0]);
	boards.deleteCard.mockResolvedValue(undefined);
	await fetchUser();
	rendered = await renderComponent(BoardPage);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

function cardWith(text: string): HTMLElement {
	const card = Array.from(rendered!.container.querySelectorAll<HTMLElement>('.card')).find(
		(candidate) => candidate.textContent?.includes(text),
	);
	if (!card) throw new Error(`Rendered board is missing card "${text}"`);
	return card;
}

describe('board mutation state ownership', () => {
	it('blocks move and delete while a reorder owns the board', async () => {
		const reorder = deferred<{ status: string }>();
		boards.reorderCards.mockReturnValueOnce(reorder.promise);

		const first = cardWith('First card');
		const down = first.querySelector<HTMLButtonElement>('button[title="Move card down"]')!;
		const remove = first.querySelector<HTMLButtonElement>('button[title="Delete"]')!;
		const move = first.querySelector<HTMLSelectElement>('select.card-move')!;

		await click(down);

		expect(boards.reorderCards).toHaveBeenCalledOnce();
		expect(boards.reorderCards).toHaveBeenCalledWith('alice', 'demo', board.id, {
			column_id: firstColumn.id,
			positions: [
				[cards[1].id, 0],
				[cards[0].id, 1],
			],
		});
		expect(down.disabled).toBe(true);
		expect(remove.disabled).toBe(true);
		expect(move.disabled).toBe(true);
		expect(down.getAttribute('aria-busy')).toBe('true');

		// Dispatching events directly proves the shared claim in the handler as
		// well as the disabled DOM state: neither path may publish a request.
		await change(move, String(secondColumn.id));
		await click(remove);
		expect(boards.moveCard).not.toHaveBeenCalled();
		expect(boards.deleteCard).not.toHaveBeenCalled();

		reorder.resolve({ status: 'ok' });
		await settle();
		expect(cardWith('First card').querySelector<HTMLSelectElement>('select.card-move')!.disabled).toBe(false);
	});
});
