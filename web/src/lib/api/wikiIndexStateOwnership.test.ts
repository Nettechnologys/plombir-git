import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import WikiIndexPage from '../../routes/[owner]/[repo]/wiki/+page.svelte';
import { navigation, setTestPage } from '../test/app';
import { resetTestClient, wiki, repos as viewerRepos } from '../test/client';
import {
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

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

function wikiPage(title: string) {
	return { title, updated_at: '2026-08-31T12:00:00Z' };
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	// The write controls follow `viewer_permission` (card_3625a7b89abb).
	viewerRepos.get.mockResolvedValue({ viewer_permission: 'write' });
	navigation.goto.mockReset();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function openCreateForm() {
	await click(element(rendered!.container, '.toolbar .btn-primary'));
	return element<HTMLFormElement>(rendered!.container, '.create-form form');
}

describe('wiki index state ownership', () => {
	it('rejects a list from the first A -> B -> A visit', async () => {
		const firstVisit = deferred<ReturnType<typeof wikiPage>[]>();
		wiki.list
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce([wikiPage('middle-page')])
			.mockResolvedValueOnce([wikiPage('current-page')]);
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(WikiIndexPage);

		setTestPage('/bob/other/wiki', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current-page');

		firstVisit.resolve([wikiPage('stale-page')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-page');
		expect(rendered.container.textContent).not.toContain('stale-page');
	});

	it('keeps the current loading and error claims when an old list fails', async () => {
		const oldVisit = deferred<ReturnType<typeof wikiPage>[]>();
		const currentVisit = deferred<ReturnType<typeof wikiPage>[]>();
		wiki.list.mockReturnValueOnce(oldVisit.promise).mockReturnValueOnce(currentVisit.promise);
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(WikiIndexPage);

		setTestPage('/bob/other/wiki', { owner: 'bob', repo: 'other' });
		await settle();
		oldVisit.reject(new Error('stale wiki failure'));
		await settle();
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
		expect(rendered.container.querySelector('p.text-secondary')).not.toBeNull();

		currentVisit.resolve([wikiPage('current-page')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-page');
	});

	it('lets the current list finish while create owns its independent claim', async () => {
		const currentList = deferred<ReturnType<typeof wikiPage>[]>();
		const currentCreate = deferred<void>();
		wiki.list.mockReturnValueOnce(currentList.promise);
		wiki.create.mockReturnValueOnce(currentCreate.promise);
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(WikiIndexPage);

		const form = await openCreateForm();
		await input(element<HTMLInputElement>(form, 'input'), 'New Page');
		await input(element<HTMLTextAreaElement>(form, 'textarea'), 'new content');
		await submit(form);
		currentList.resolve([wikiPage('current-page')]);
		await settle();

		expect(rendered.container.textContent).toContain('current-page');
		expect(rendered.container.querySelector('p.text-secondary')).toBeNull();
		currentCreate.resolve();
		await settle();
	});

	it('does not publish or duplicate a create from an old repository visit', async () => {
		const oldCreate = deferred<void>();
		wiki.list.mockResolvedValue([]);
		wiki.create.mockReturnValueOnce(oldCreate.promise);
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(WikiIndexPage);

		const oldForm = await openCreateForm();
		await input(element<HTMLInputElement>(oldForm, 'input'), 'Old Page');
		await input(element<HTMLTextAreaElement>(oldForm, 'textarea'), 'old content');
		await submit(oldForm);
		await submit(oldForm);
		expect(wiki.create).toHaveBeenCalledOnce();
		expect(wiki.create).toHaveBeenCalledWith('alice', 'demo', 'Old Page', 'old content');
		expect(element<HTMLButtonElement>(oldForm, 'button[type="submit"]').disabled).toBe(true);

		setTestPage('/bob/other/wiki', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		await settle();
		const currentForm = await openCreateForm();
		await input(element<HTMLInputElement>(currentForm, 'input'), 'Current Draft');
		await input(element<HTMLTextAreaElement>(currentForm, 'textarea'), 'current content');

		oldCreate.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(element<HTMLInputElement>(currentForm, 'input').value).toBe('Current Draft');
		expect(element<HTMLTextAreaElement>(currentForm, 'textarea').value).toBe('current content');
	});
});
