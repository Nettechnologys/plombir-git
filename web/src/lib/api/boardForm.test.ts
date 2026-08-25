import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import BoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import {
  buildBoardCardUpdatePayload,
  buildBoardUpdatePayload,
  buildColumnUpdatePayload,
} from './boardForm';
import { setTestPage } from '../test/app';
import { boards, issues, resetTestClient } from '../test/client';
import {
	change,
	click,
	element,
	input,
	renderComponent,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

const board = { id: 7, name: 'Roadmap', description: 'Q4' };
const issue = { id: 42, number: 42, title: 'Ship it' };
const boardResponse = {
	board,
	columns: [
		{
			column: { id: 3, board_id: 7, name: 'Todo', position: 0 },
			cards: [
				{
					id: 9,
					column_id: 3,
					note: 'old note',
					issue_id: 42,
					position: 0,
					issue,
				},
			],
		},
	],
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
	boards.list.mockResolvedValue([board]);
	boards.get.mockResolvedValue(boardResponse);
	boards.update.mockResolvedValue({ ...board, name: 'Roadmap 2', description: '' });
	boards.updateColumn.mockResolvedValue({ id: 3, board_id: 7, name: 'In review', position: 0 });
	issues.list.mockResolvedValue({ data: [issue] });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('board edit payloads', () => {
  it('keeps an existing issue link while editing the note', () => {
    expect(buildBoardCardUpdatePayload({ note: 'updated note', issueId: '42' })).toEqual({
      note: 'updated note',
      issue_id: 42,
    });
  });

  it('uses explicit null only when the operator selects no issue', () => {
    expect(buildBoardCardUpdatePayload({ note: 'standalone', issueId: '' })).toEqual({
      note: 'standalone',
      issue_id: null,
    });
  });

  it('normalizes board and column names', () => {
    expect(buildBoardUpdatePayload({ name: ' Roadmap ', description: ' Q4 ' })).toEqual({
      name: 'Roadmap',
      description: 'Q4',
    });
    expect(buildColumnUpdatePayload(' In review ')).toEqual({ name: 'In review' });
  });

	it('submits all three rendered editors through their canonical builders', async () => {
		rendered = await renderComponent(BoardPage);

		await click(element(rendered.container, '.board-header-actions .btn-sm'));
		const boardInputs = rendered.container.querySelectorAll<HTMLInputElement>('.board-edit-form input');
		await input(boardInputs[0], ' Roadmap 2 ');
		await input(boardInputs[1], '   ');
		await click(element(rendered.container, '.board-edit-form .btn-primary'));
		expect(boards.update).toHaveBeenCalledWith('alice', 'demo', 7, {
			name: 'Roadmap 2',
			description: '',
		});

		await click(element(rendered.container, '.col-header [title="Edit"]'));
		await input(element(rendered.container, '.column-name-input'), ' In review ');
		await click(element(rendered.container, '.col-header [title="Save"]'));
		expect(boards.updateColumn).toHaveBeenCalledWith('alice', 'demo', 7, 3, {
			name: 'In review',
		});

		await click(element(rendered.container, '.card-actions [title="Edit"]'));
		await input(element(rendered.container, '.modal textarea'), ' updated note ');
		await change(element(rendered.container, '.modal select'), '');
		await click(element(rendered.container, '.modal .btn-primary'));
		expect(boards.updateCard).toHaveBeenCalledWith('alice', 'demo', 7, 9, {
			note: 'updated note',
			issue_id: null,
		});
	});
});
