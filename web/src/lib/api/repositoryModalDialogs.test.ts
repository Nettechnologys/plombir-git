import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import BoardPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import PipelinesPage from '../../routes/[owner]/[repo]/pipelines/+page.svelte';
import LabelsPage from '../../routes/[owner]/[repo]/settings/labels/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import {
	artifacts,
	auth,
	boards,
	connectJobLogWebSocket,
	issues,
	labels,
	pipelines,
	repos,
	resetTestClient,
} from '../test/client';
import {
	expectEscapeClosesAndRestoresFocus,
	expectModalSurvivesInteraction,
	openModalFrom,
} from '../test/modalContract';
import { click, element, input, renderComponent, type RenderedComponent } from '../test/render';

// card_4a99471945dc: a click into the label Name field closed the form, a
// space typed into it was swallowed, and Enter on any control inside the job
// log closed the log.

const timestamp = '2026-08-26T00:00:00Z';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	auth.me.mockResolvedValue({ id: 1, username: 'alice', email: 'alice@example.com', is_admin: false });
	// The write controls follow `viewer_permission` (card_3625a7b89abb).
	repos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'write' });
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

describe('repository modal dialogs', () => {
	it('keeps the label form open while the user clicks and types a name with spaces', async () => {
		labels.list.mockResolvedValue([{ id: 1, name: 'bug', color: '#ff0000' }]);
		setTestPage('/alice/demo/settings/labels', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(LabelsPage);
		const opener = element<HTMLButtonElement>(rendered.container, '.page-header .btn-primary');

		await openModalFrom(rendered.container, opener);
		const name = element<HTMLInputElement>(rendered.container, '#label-name');
		expect(document.activeElement).toBe(name);
		await expectModalSurvivesInteraction(rendered.container, name);
		await input(name, 'needs triage');
		expect(name.value).toBe('needs triage');
		await expectModalSurvivesInteraction(rendered.container, element(rendered.container, '#label-desc'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});

	it('keeps the label delete confirmation open and visible while it is pending', async () => {
		let resolveDelete!: () => void;
		labels.list.mockResolvedValue([{ id: 1, name: 'bug', color: '#ff0000' }]);
		labels.delete.mockReturnValueOnce(new Promise<void>((resolve) => { resolveDelete = resolve; }));
		setTestPage('/alice/demo/settings/labels', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(LabelsPage);
		const opener = element<HTMLButtonElement>(rendered.container, '.label-actions button[title="Delete"]');

		const dialog = await openModalFrom(rendered.container, opener);
		expect(document.activeElement?.textContent?.trim()).toBe('Cancel');
		await expectModalSurvivesInteraction(rendered.container, element(dialog, 'h2'));

		const deleteButton = Array.from(dialog.querySelectorAll('button')).find(
			(candidate) => candidate.textContent?.trim() === 'Delete',
		)!;
		await click(deleteButton);
		expect(labels.delete).toHaveBeenCalledWith('alice', 'demo', 1);
		expect(element(rendered.container, '[role="dialog"]').textContent).toContain('Deleting...');
		resolveDelete();
	});

	it('keeps the board create and card edit dialogs open on clicks and typing', async () => {
		const board = {
			id: 10, repo_id: 20, org_id: null, name: 'Sprint', description: null,
			created_by: 1, created_at: timestamp, updated_at: timestamp,
		};
		const column = { id: 11, board_id: 10, name: 'Todo', color: null, position: 0, created_at: timestamp };
		const card = {
			id: 101, column_id: 11, issue_id: null, note: 'First card', position: 0,
			created_at: timestamp, updated_at: timestamp, issue: null,
		};
		boards.list.mockResolvedValue([board]);
		boards.get.mockResolvedValue({ board, columns: [{ column, cards: [card] }] });
		issues.list.mockResolvedValue({ data: [] });
		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		await fetchUser();
		rendered = await renderComponent(BoardPage);

		const createOpener = element<HTMLButtonElement>(rendered.container, '.page-header .btn-primary');
		const createDialog = await openModalFrom(rendered.container, createOpener);
		await expectModalSurvivesInteraction(rendered.container, element(createDialog, 'input'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, createOpener);

		const editOpener = element<HTMLButtonElement>(rendered.container, '.card button[title="Edit"]');
		const editDialog = await openModalFrom(rendered.container, editOpener);
		await expectModalSurvivesInteraction(rendered.container, element(editDialog, 'textarea'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, editOpener);
	});

	it('keeps the job log open on clicks and keys inside it', async () => {
		const pipeline = {
			id: 1, status: 'success', commit_sha: 'abcdef1', commit_message: 'Pipeline 1',
			ref_name: 'main', started_at: timestamp, finished_at: timestamp,
		};
		const job = { id: 5, name: 'build', status: 'success', started_at: timestamp, finished_at: timestamp };
		pipelines.list.mockResolvedValue({ data: [pipeline], pagination: { total_pages: 1 } });
		pipelines.get.mockResolvedValue({
			pipeline,
			stages: [{ id: 2, name: 'build', status: 'success', started_at: timestamp, finished_at: timestamp, jobs: [job] }],
		});
		pipelines.jobLog.mockResolvedValue({ content: 'compiling', offset: 0, next_offset: 9, total_length: 9 });
		repos.branches.mockResolvedValue([]);
		artifacts.list.mockResolvedValue([]);
		connectJobLogWebSocket.mockReturnValue({ close: vi.fn() });
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);
		await click(element(rendered.container, '.pipeline-item'));

		const opener = element<HTMLElement>(rendered.container, '.job-card');
		const dialog = await openModalFrom(rendered.container, opener);
		expect(dialog.textContent).toContain('compiling');
		expect(connectJobLogWebSocket.mock.calls[0]?.[0]).toBe(5);
		// The close button is the first control: Enter on it used to be caught
		// by the dialog's own "close on Enter/Space" handler.
		await expectModalSurvivesInteraction(rendered.container, element(dialog, '.log-content'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});
});
