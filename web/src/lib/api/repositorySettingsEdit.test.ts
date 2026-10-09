import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_3625a7b89abb: the repository settings page was read-only — the
// description, visibility, default branch and name could only be changed with
// `curl`. The page's client namespace is the shared mock, but `repos.update`
// is pointed at the REAL client, whose transport is this mock — so each
// assertion reads the request as it leaves the browser.
const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import RepositorySettingsPage from '../../routes/[owner]/[repo]/settings/+page.svelte';
import { ApiError } from './error';
import { buildRepoSettingsPatch, repoSettingsFormState } from './repoSettingsForm';
import { repos } from './repos';
import { navigation, setTestPage } from '../test/app';
import { expectEscapeClosesAndRestoresFocus, expectModalSurvivesInteraction, openModalFrom } from '../test/modalContract';
import { repos as routeRepos, resetTestClient } from '../test/client';
import { change, click, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

function repository(overrides: Record<string, unknown> = {}) {
	return {
		id: 1,
		owner_id: 1,
		name: 'demo',
		description: 'Old words',
		is_private: false,
		default_branch: 'main',
		stars_count: 0,
		forks_count: 0,
		created_at: '2026-08-30T00:00:00Z',
		viewer_permission: 'admin',
		...overrides,
	};
}

function patchCalls() {
	return base.request.mock.calls.filter(([, init]) => init?.method === 'PATCH');
}

function onlyPatchBody(): Record<string, unknown> {
	const calls = patchCalls();
	expect(calls).toHaveLength(1);
	expect(calls[0][0]).toBe('/repos/alice/demo');
	return JSON.parse(calls[0][1].body);
}

async function renderSettings(): Promise<HTMLElement> {
	rendered = await renderComponent(RepositorySettingsPage);
	return rendered.container;
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	base.request.mockReset();
	setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
	routeRepos.get.mockResolvedValue(repository());
	routeRepos.branches.mockResolvedValue([
		{ name: 'main', is_default: true },
		{ name: 'develop', is_default: false },
	]);
	routeRepos.update.mockImplementation(repos.update);
	base.request.mockImplementation(async (_path: string, init?: RequestInit) => ({
		...repository(),
		...JSON.parse(String(init?.body ?? '{}')),
	}));
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

describe('buildRepoSettingsPatch', () => {
	const original = { name: 'demo', description: 'Old words', is_private: false, default_branch: 'main' };

	it('sends nothing for an untouched form', () => {
		expect(buildRepoSettingsPatch(original, repoSettingsFormState(original))).toEqual({});
	});

	it('sends an emptied description as null and leaves an absent one alone', () => {
		expect(buildRepoSettingsPatch(original, { ...repoSettingsFormState(original), description: '  ' })).toEqual({
			description: null,
		});
		const blank = { ...original, description: null };
		expect(buildRepoSettingsPatch(blank, repoSettingsFormState(blank))).toEqual({});
	});

	it('sends each changed key and only those', () => {
		expect(
			buildRepoSettingsPatch(original, {
				name: ' renamed ',
				description: 'Old words',
				isPrivate: true,
				defaultBranch: 'develop',
			}),
		).toEqual({ name: 'renamed', is_private: true, default_branch: 'develop' });
	});
});

describe('repository settings form', () => {
	it('is shown to an administrator and to nobody else', async () => {
		routeRepos.get.mockResolvedValue(repository({ viewer_permission: 'write' }));
		let container = await renderSettings();

		expect(container.querySelector('#repo-description')).toBeNull();
		expect(container.querySelector('.toggle-visibility')).toBeNull();
		expect(container.querySelector('.open-rename')).toBeNull();
		expect(container.querySelector('.transfer-repo')).toBeNull();
		expect(container.querySelector('#delete-confirm')).toBeNull();
		expect(container.textContent).toContain('Old words');
		expect(routeRepos.branches).not.toHaveBeenCalled();

		await rendered!.destroy();
		routeRepos.get.mockResolvedValue(repository());
		container = await renderSettings();

		expect(element<HTMLTextAreaElement>(container, '#repo-description').value).toBe('Old words');
		expect(element<HTMLSelectElement>(container, '#repo-default-branch').value).toBe('main');
		expect(container.querySelector('.toggle-visibility')).not.toBeNull();
		expect(container.querySelector('.open-rename')).not.toBeNull();
	});

	it('sends only the description when only the description changed', async () => {
		const container = await renderSettings();
		const save = element<HTMLButtonElement>(container, '.save-general');
		expect(save.disabled).toBe(true);

		await input(element(container, '#repo-description'), 'New words');
		await submit(element<HTMLFormElement>(container, '.general-form'));

		expect(onlyPatchBody()).toEqual({ description: 'New words' });
		expect(container.querySelector('[role="status"]')).not.toBeNull();
	});

	it('clears the description with null', async () => {
		const container = await renderSettings();

		await input(element(container, '#repo-description'), '');
		await submit(element<HTMLFormElement>(container, '.general-form'));

		expect(onlyPatchBody()).toEqual({ description: null });
	});

	it('sends only the default branch, picked from the branch list', async () => {
		const container = await renderSettings();
		const select = element<HTMLSelectElement>(container, '#repo-default-branch');
		expect(Array.from(select.options).map((option) => option.value)).toEqual(['main', 'develop']);

		await change(select, 'develop');
		await submit(element<HTMLFormElement>(container, '.general-form'));

		expect(onlyPatchBody()).toEqual({ default_branch: 'develop' });
	});

	it('shows the server refusal of a save', async () => {
		routeRepos.update.mockRejectedValue(new ApiError('branch develop does not exist', 400));
		const container = await renderSettings();

		await change(element<HTMLSelectElement>(container, '#repo-default-branch'), 'develop');
		await submit(element<HTMLFormElement>(container, '.general-form'));

		expect(element(container, '.save-error').textContent).toContain('branch develop does not exist');
	});

	it('changes the visibility through a confirm dialog that says who loses access', async () => {
		const container = await renderSettings();
		const opener = element<HTMLButtonElement>(container, '.toggle-visibility');

		const dialog = await openModalFrom(document.body, opener);
		expect(patchCalls()).toHaveLength(0);
		expect(dialog.textContent).toContain('loses access');
		await expectModalSurvivesInteraction(document.body, element(dialog, '#visibility-title'));
		await expectEscapeClosesAndRestoresFocus(document.body, opener);
		expect(patchCalls()).toHaveLength(0);

		await click(opener);
		await click(element(document.body, '[role="dialog"] .confirm-visibility'));

		expect(onlyPatchBody()).toEqual({ is_private: true });
		expect(document.body.querySelector('[role="dialog"]')).toBeNull();
		expect(element(container, '.toggle-visibility').textContent).toContain('Make public');
	});

	it('renames through a confirm dialog that warns there is no redirect, then follows the new name', async () => {
		const container = await renderSettings();
		const opener = element<HTMLButtonElement>(container, '.open-rename');
		expect(opener.disabled).toBe(true);

		await input(element(container, '#repo-rename'), ' renamed ');
		const dialog = await openModalFrom(document.body, opener);
		expect(dialog.textContent).toContain('alice/renamed');
		expect(dialog.textContent).toContain('no redirect');
		expect(patchCalls()).toHaveLength(0);

		await click(element(dialog, '.confirm-rename'));

		expect(onlyPatchBody()).toEqual({ name: 'renamed' });
		expect(navigation.goto).toHaveBeenCalledWith('/alice/renamed/settings');
	});

	it('keeps the rename dialog open with the server message when the name is taken', async () => {
		routeRepos.update.mockRejectedValue(new ApiError('repository alice/taken already exists', 409));
		const container = await renderSettings();

		await input(element(container, '#repo-rename'), 'taken');
		await click(element(container, '.open-rename'));
		await click(element(document.body, '[role="dialog"] .confirm-rename'));

		expect(element(document.body, '[role="dialog"] .rename-error').textContent).toContain(
			'repository alice/taken already exists',
		);
		expect(navigation.goto).not.toHaveBeenCalled();
	});
});
