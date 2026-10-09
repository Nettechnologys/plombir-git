import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import BoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, issues, repos, resetTestClient } from '../test/client';
import {
	change,
	click,
	input,
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
	vi.stubGlobal('confirm', vi.fn(() => true));
	setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
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
	vi.unstubAllGlobals();
});

function cardWith(text: string): HTMLElement {
	const card = Array.from(rendered!.container.querySelectorAll<HTMLElement>('.card')).find(
		(candidate) => candidate.textContent?.includes(text),
	);
	if (!card) throw new Error(`Rendered board is missing card "${text}"`);
	return card;
}

function required<T extends Element>(selector: string, parent: ParentNode = rendered!.container): T {
	const element = parent.querySelector<T>(selector);
	if (!element) throw new Error(`Rendered board is missing ${selector}`);
	return element;
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
		const releasedMove = cardWith('First card').querySelector<HTMLSelectElement>('select.card-move')!;
		expect(releasedMove.disabled).toBe(false);
		await change(releasedMove, String(secondColumn.id));
		expect(boards.moveCard).toHaveBeenCalledOnce();
	});

	it('makes every board-wide mutation control visibly busy and rejects direct dispatch', async () => {
		const createBoardTrigger = required<HTMLButtonElement>('.page-header .btn-primary');
		const editBoardTrigger = required<HTMLButtonElement>('.board-header-actions button:first-child');
		const addColumnTrigger = required<HTMLButtonElement>('.board-header-actions button:last-child');
		const deleteBoardTrigger = required<HTMLButtonElement>('.board-tabs .close');

		await click(createBoardTrigger);
		const createModal = required<HTMLElement>('.modal');
		const createName = required<HTMLInputElement>('input', createModal);
		const createBoardSubmit = required<HTMLButtonElement>('.btn-primary', createModal);
		await input(createName, 'Parallel board');

		await click(editBoardTrigger);
		const boardEditForm = required<HTMLElement>('.board-edit-form');
		const saveBoardSubmit = required<HTMLButtonElement>('.btn-primary', boardEditForm);

		await click(addColumnTrigger);
		const addColumnForm = Array.from(rendered!.container.querySelectorAll<HTMLElement>('.board-layout > .inline-form'))
			.find((form) => !form.classList.contains('board-edit-form'))!;
		const newColumnName = required<HTMLInputElement>('input', addColumnForm);
		const addColumnSubmit = required<HTMLButtonElement>('.btn-primary', addColumnForm);
		await input(newColumnName, 'Doing');

		const columnEditTriggers = rendered!.container.querySelectorAll<HTMLButtonElement>('.col-header button[title="Edit"]');
		await click(columnEditTriggers[0]);
		const columnName = required<HTMLInputElement>('.column-name-input');
		const saveColumnSubmit = required<HTMLButtonElement>('.col-header button[title="Save"]');
		const remainingColumnEdit = required<HTMLButtonElement>('.col-header button[title="Edit"]');
		const deleteColumnTriggers = Array.from(
			rendered!.container.querySelectorAll<HTMLButtonElement>('.col-header > button[title="Delete"]'),
		);

		const firstAddCardTrigger = required<HTMLButtonElement>('.add-card-btn');
		await click(firstAddCardTrigger);
		const addCardForm = required<HTMLElement>('.col-body > .inline-form');
		const newCardTitle = required<HTMLInputElement>('input', addCardForm);
		const addCardSubmit = required<HTMLButtonElement>('.btn-primary', addCardForm);
		await input(newCardTitle, 'Blocked card');
		const remainingAddCardTrigger = required<HTMLButtonElement>('.add-card-btn');

		const first = cardWith('First card');
		const down = required<HTMLButtonElement>('button[title="Move card down"]', first);
		const remove = required<HTMLButtonElement>('button[title="Delete"]', first);
		const editCardTrigger = required<HTMLButtonElement>('button[title="Edit"]', first);
		const move = required<HTMLSelectElement>('select.card-move', first);
		await click(editCardTrigger);
		const cardModal = rendered!.container.querySelectorAll<HTMLElement>('.modal')[1];
		const saveCardSubmit = required<HTMLButtonElement>('.btn-primary', cardModal);

		const reorder = deferred<{ status: string }>();
		boards.reorderCards.mockReturnValueOnce(reorder.promise);
		await click(down);

		const mutationButtons = [
			createBoardTrigger,
			createBoardSubmit,
			deleteBoardTrigger,
			editBoardTrigger,
			saveBoardSubmit,
			addColumnTrigger,
			addColumnSubmit,
			saveColumnSubmit,
			remainingColumnEdit,
			...deleteColumnTriggers,
			addCardSubmit,
			remainingAddCardTrigger,
			editCardTrigger,
			saveCardSubmit,
			down,
			remove,
		];
		for (const control of mutationButtons) {
			expect(control.disabled).toBe(true);
			expect(control.getAttribute('aria-busy')).toBe('true');
		}
		expect(move.disabled).toBe(true);
		expect(move.getAttribute('aria-busy')).toBe('true');
		for (const field of [createName, ...boardEditForm.querySelectorAll<HTMLInputElement>('input'), newColumnName, columnName, newCardTitle]) {
			expect(field.disabled).toBe(true);
		}

		await click(createBoardSubmit);
		await click(deleteBoardTrigger);
		await click(saveBoardSubmit);
		await click(addColumnSubmit);
		await click(saveColumnSubmit);
		await click(deleteColumnTriggers[0]);
		await click(addCardSubmit);
		await click(saveCardSubmit);
		await change(move, String(secondColumn.id));
		await click(remove);
		await click(down);

		expect(boards.reorderCards).toHaveBeenCalledOnce();
		expect(boards.create).not.toHaveBeenCalled();
		expect(boards.delete).not.toHaveBeenCalled();
		expect(boards.update).not.toHaveBeenCalled();
		expect(boards.createColumn).not.toHaveBeenCalled();
		expect(boards.updateColumn).not.toHaveBeenCalled();
		expect(boards.deleteColumn).not.toHaveBeenCalled();
		expect(boards.createCard).not.toHaveBeenCalled();
		expect(boards.updateCard).not.toHaveBeenCalled();
		expect(boards.moveCard).not.toHaveBeenCalled();
		expect(boards.deleteCard).not.toHaveBeenCalled();

		reorder.resolve({ status: 'ok' });
		await settle();
		for (const control of mutationButtons.filter((control) => control !== down)) {
			expect(control.disabled).toBe(false);
		}
		expect(move.disabled).toBe(false);
	});

	it('does not open forms or switch boards while a mutation owns the page', async () => {
		const reorder = deferred<{ status: string }>();
		boards.reorderCards.mockReturnValueOnce(reorder.promise);

		const first = cardWith('First card');
		await click(required<HTMLButtonElement>('button[title="Move card down"]', first));

		const createBoardTrigger = required<HTMLButtonElement>('.page-header .btn-primary');
		const editBoardTrigger = required<HTMLButtonElement>('.board-header-actions button:first-child');
		const addColumnTrigger = required<HTMLButtonElement>('.board-header-actions button:last-child');
		const columnEditTrigger = required<HTMLButtonElement>('.col-header button[title="Edit"]');
		const editCardTrigger = required<HTMLButtonElement>('button[title="Edit"]', first);
		const addCardTrigger = required<HTMLButtonElement>('.add-card-btn');
		const boardTab = required<HTMLElement>('.board-tabs .tab');

		expect(boardTab.getAttribute('aria-disabled')).toBe('true');
		expect(boardTab.tabIndex).toBe(-1);
		await click(createBoardTrigger);
		await click(editBoardTrigger);
		await click(addColumnTrigger);
		await click(columnEditTrigger);
		await click(editCardTrigger);
		await click(addCardTrigger);
		await click(boardTab);

		expect(rendered!.container.querySelector('.modal')).toBeNull();
		expect(rendered!.container.querySelector('.board-edit-form')).toBeNull();
		expect(rendered!.container.querySelector('.board-layout > .inline-form')).toBeNull();
		expect(rendered!.container.querySelector('.column-name-input')).toBeNull();
		expect(rendered!.container.querySelector('.col-body > .inline-form')).toBeNull();
		expect(boards.get).toHaveBeenCalledOnce();

		reorder.resolve({ status: 'ok' });
		await settle();
		expect(boardTab.getAttribute('aria-disabled')).toBe('false');
		expect(boardTab.tabIndex).toBe(0);
		await click(createBoardTrigger);
		expect(rendered!.container.querySelector('.modal')).not.toBeNull();
	});
});
