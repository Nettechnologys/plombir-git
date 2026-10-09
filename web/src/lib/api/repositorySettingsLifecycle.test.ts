import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RepositorySettingsPage from '../../routes/[owner]/[repo]/settings/+page.svelte';
import { navigation, setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	input,
	renderComponent,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	vi.clearAllMocks();
	vi.useFakeTimers();
	// No native dialog may stand in for the page's own confirmation: a
	// `confirm()` call now fails the test instead of answering it.
	vi.stubGlobal('confirm', vi.fn(() => {
		throw new Error('window.confirm() was called');
	}));
	resetTestClient();
	setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		description: null,
		is_private: false,
		default_branch: 'main',
		created_at: '2026-08-30T00:00:00Z',
		// Transfer and deletion are offered to repository administrators only.
		viewer_permission: 'admin',
	});
	repos.transfer.mockResolvedValue(undefined);
	repos.delete.mockResolvedValue(undefined);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.clearAllTimers();
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

async function renderSettings(): Promise<void> {
	rendered = await renderComponent(RepositorySettingsPage);
}

async function transferTo(destinationOwner: string): Promise<void> {
	await input(element<HTMLInputElement>(rendered!.container, '#new-owner'), destinationOwner);
	await click(element(rendered!.container, '.transfer-repo'));
	await click(element(document.body, '.confirm-transfer'));
}

describe('repository settings resource lifetime', () => {
	it('redirects a live page once to the destination confirmed by the transfer', async () => {
		await renderSettings();
		await transferTo(' bob ');

		expect(repos.transfer).toHaveBeenCalledWith('alice', 'demo', 'bob');
		expect(navigation.goto).not.toHaveBeenCalled();

		await input(element<HTMLInputElement>(rendered!.container, '#new-owner'), 'mallory');
		await vi.advanceTimersByTimeAsync(1_499);
		expect(navigation.goto).not.toHaveBeenCalled();

		await vi.advanceTimersByTimeAsync(1);
		expect(navigation.goto).toHaveBeenCalledOnce();
		expect(navigation.goto).toHaveBeenCalledWith('/bob/demo');
	});

	it('cancels the pending transfer redirect when the page is destroyed', async () => {
		await renderSettings();
		await transferTo('bob');

		await rendered!.destroy();
		rendered = undefined;
		await vi.advanceTimersByTimeAsync(1_500);

		expect(navigation.goto).not.toHaveBeenCalled();
	});

	it('names both ends of a transfer and does nothing until it is confirmed', async () => {
		await renderSettings();
		await input(element<HTMLInputElement>(rendered!.container, '#new-owner'), 'bob');
		await click(element(rendered!.container, '.transfer-repo'));

		expect(repos.transfer).not.toHaveBeenCalled();
		expect(document.body.querySelector('[role="dialog"]')?.textContent).toContain('alice/demo → bob/demo');
		const cancel = Array.from(document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find(
			(candidate) => candidate.textContent?.trim() === 'Cancel',
		)!;
		await click(cancel);

		expect(document.body.querySelector('.confirm-transfer')).toBeNull();
		expect(repos.transfer).not.toHaveBeenCalled();
	});

	it('keeps repository deletion redirect immediate', async () => {
		await renderSettings();
		await input(element<HTMLInputElement>(rendered!.container, '#delete-confirm'), 'alice/demo');
		await click(element(rendered!.container, '.btn-danger'));
		expect(repos.delete).not.toHaveBeenCalled();
		await click(element(document.body, '.confirm-delete-repo'));

		expect(repos.delete).toHaveBeenCalledWith('alice', 'demo');
		expect(navigation.goto).toHaveBeenCalledOnce();
		expect(navigation.goto).toHaveBeenCalledWith('/dashboard');
	});
});
