import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import IssueBoardPage from '../../routes/[owner]/[repo]/issues/board/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, boards, issues, repos, resetTestClient } from '../test/client';
import {
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
	created_at: '2026-08-31T00:00:00Z',
	updated_at: '2026-08-31T00:00:00Z',
};

const secondBoard = { ...board, id: 20, name: 'Backlog' };
const createdBoard = { ...board, id: 30, name: 'Parallel board' };

const firstColumn = {
	id: 11,
	board_id: board.id,
	name: 'Todo',
	color: null,
	position: 0,
	created_at: '2026-08-31T00:00:00Z',
};

const secondColumn = {
	...firstColumn,
	id: 12,
	name: 'Done',
	position: 1,
};

const card = {
	id: 101,
	column_id: firstColumn.id,
	issue_id: null,
	note: 'First card',
	position: 0,
	created_at: '2026-08-31T00:00:00Z',
	updated_at: '2026-08-31T00:00:00Z',
	issue: null,
};

const fullBoard = {
	board,
	columns: [
		{ column: firstColumn, cards: [card] },
		{ column: secondColumn, cards: [] },
	],
};

const createdFullBoard = {
	board: createdBoard,
	columns: [],
};

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	vi.stubGlobal('confirm', vi.fn(() => true));
	setTestPage('/alice/demo/issues/board', { owner: 'alice', repo: 'demo' });
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
	boards.list.mockResolvedValue([board, secondBoard]);
	boards.get.mockImplementation(async (_owner: string, _repo: string, id: number) =>
		id === createdBoard.id ? createdFullBoard : fullBoard,
	);
	boards.create.mockResolvedValue(createdBoard);
	boards.createColumn.mockResolvedValue(firstColumn);
	boards.createCard.mockResolvedValue(card);
	boards.deleteColumn.mockResolvedValue(undefined);
	boards.deleteCard.mockResolvedValue(undefined);
	boards.moveCard.mockResolvedValue(card);
	boards.reorderCards.mockResolvedValue({ status: 'ok' });
	issues.list.mockResolvedValue({ data: [] });
	await fetchUser();
	rendered = await renderComponent(IssueBoardPage);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

function required<T extends Element>(selector: string, parent: ParentNode = rendered!.container): T {
	const element = parent.querySelector<T>(selector);
	if (!element) throw new Error(`Rendered issue board is missing ${selector}`);
	return element;
}

async function dispatch(element: Element, event: Event): Promise<void> {
	element.dispatchEvent(event);
	await settle();
}

describe('issue board mutation state ownership', () => {
	it('serializes every board control and releases them after the owner completes', async () => {
		const createBoardTrigger = required<HTMLButtonElement>('.board-tabs > .btn-ghost');
		const addColumnTrigger = required<HTMLButtonElement>('.board-toolbar > .btn-outline');

		await click(createBoardTrigger);
		const createName = required<HTMLInputElement>('input[placeholder="Board name"]');
		const createForm = createName.closest('.inline-form')!;
		const createBoardSubmit = required<HTMLButtonElement>('.btn-primary', createForm);
		await input(createName, createdBoard.name);

		await click(addColumnTrigger);
		const newColumnName = required<HTMLInputElement>('input[placeholder="Column name"]');
		const addColumnForm = newColumnName.closest('.inline-form')!;
		const addColumnSubmit = required<HTMLButtonElement>('.btn-primary', addColumnForm);
		await input(newColumnName, 'Doing');

		const firstAddCardTrigger = required<HTMLButtonElement>('.add-card-btn');
		await click(firstAddCardTrigger);
		const newCardNote = required<HTMLTextAreaElement>('textarea[placeholder="Add a note…"]');
		const addCardForm = newCardNote.closest('.add-card-form')!;
		const addCardSubmit = required<HTMLButtonElement>('.btn-primary', addCardForm);
		await input(newCardNote, 'Blocked card');

		const cardControl = required<HTMLElement>('.card');
		const deleteCard = required<HTMLButtonElement>('.card-delete', cardControl);
		const deleteColumns = Array.from(
			rendered!.container.querySelectorAll<HTMLButtonElement>('button[title="Delete column"]'),
		);
		const remainingAddCardTrigger = required<HTMLButtonElement>('.add-card-btn');
		const boardTabs = Array.from(
			rendered!.container.querySelectorAll<HTMLButtonElement>('.board-tab'),
		);
		const targetColumn = rendered!.container.querySelectorAll<HTMLElement>('.board-column')[1];

		const deletion = deferred<void>();
		boards.deleteCard.mockReturnValueOnce(deletion.promise);
		await click(deleteCard);

		const mutationButtons = [
			createBoardTrigger,
			createBoardSubmit,
			addColumnTrigger,
			addColumnSubmit,
			...deleteColumns,
			addCardSubmit,
			remainingAddCardTrigger,
			deleteCard,
		];
		for (const control of [...boardTabs, ...mutationButtons]) {
			expect(control.disabled).toBe(true);
			expect(control.getAttribute('aria-busy')).toBe('true');
		}
		for (const field of [createName, newColumnName, newCardNote]) {
			expect(field.disabled).toBe(true);
		}
		expect(cardControl.draggable).toBe(false);
		expect(cardControl.getAttribute('aria-disabled')).toBe('true');
		expect(cardControl.getAttribute('aria-busy')).toBe('true');
		expect(cardControl.tabIndex).toBe(-1);

		await click(createBoardTrigger);
		await click(addColumnTrigger);
		await click(createBoardSubmit);
		await click(addColumnSubmit);
		await click(deleteColumns[0]);
		await click(addCardSubmit);
		await click(remainingAddCardTrigger);
		await click(deleteCard);
		await click(boardTabs[1]);
		const blockedDrag = new Event('dragstart', { bubbles: true, cancelable: true });
		await dispatch(cardControl, blockedDrag);
		await dispatch(targetColumn, new Event('drop', { bubbles: true, cancelable: true }));

		expect(blockedDrag.defaultPrevented).toBe(true);
		expect(rendered!.container.querySelectorAll('.add-card-form')).toHaveLength(1);
		expect(rendered!.container.querySelector('input[placeholder="Board name"]')).not.toBeNull();
		expect(rendered!.container.querySelector('input[placeholder="Column name"]')).not.toBeNull();
		expect(boards.get).toHaveBeenCalledOnce();
		expect(boards.create).not.toHaveBeenCalled();
		expect(boards.createColumn).not.toHaveBeenCalled();
		expect(boards.deleteColumn).not.toHaveBeenCalled();
		expect(boards.createCard).not.toHaveBeenCalled();
		expect(boards.deleteCard).toHaveBeenCalledOnce();
		expect(boards.moveCard).not.toHaveBeenCalled();
		expect(boards.reorderCards).not.toHaveBeenCalled();

		deletion.resolve();
		await settle();

		expect(required<HTMLButtonElement>('.board-tabs > .btn-ghost').disabled).toBe(false);
		expect(required<HTMLButtonElement>('.board-toolbar > .btn-outline').disabled).toBe(false);
		expect(required<HTMLButtonElement>('.card-delete').disabled).toBe(false);
		expect(required<HTMLElement>('.card').draggable).toBe(true);

		await click(required<HTMLButtonElement>('.inline-form .btn-primary'));
		expect(boards.create).toHaveBeenCalledOnce();
	});
});
