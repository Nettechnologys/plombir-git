import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import BlobPage from '../../routes/[owner]/[repo]/blob/[...path]/+page.svelte';
import EditFilePage from '../../routes/[owner]/[repo]/edit/[...path]/+page.svelte';
import NewFilePage from '../../routes/[owner]/[repo]/new/+page.svelte';
import PackageUploadPage from '../../routes/[owner]/[repo]/packages/upload/+page.svelte';
import EditReleasePage from '../../routes/[owner]/[repo]/releases/edit/[id]/+page.svelte';
import { navigation, setTestPage } from '../test/app';
import { packages, releases, repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	input,
	renderComponent,
	settle,
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

function blob(content: string, sha: string) {
	return {
		path: 'notes/readme.md',
		sha,
		size: content.length,
		content,
		encoding: 'utf-8',
		is_binary: false,
		name: 'readme.md',
	};
}

function release(title: string) {
	return {
		tag_name: `v-${title}`,
		title,
		body: `${title} notes`,
		is_draft: false,
		is_prerelease: false,
	};
}

async function dispatchClickTwice(button: HTMLButtonElement): Promise<void> {
	button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
	button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
	await settle();
}

async function dispatchSubmitTwice(form: HTMLFormElement): Promise<void> {
	form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
	form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
	await settle();
}

async function choosePackageFile(inputElement: HTMLInputElement, name: string): Promise<File> {
	const file = new File([`${name} bytes`], name, { type: 'application/octet-stream' });
	Object.defineProperty(inputElement, 'files', { configurable: true, value: [file] });
	inputElement.dispatchEvent(new Event('change', { bubbles: true }));
	await settle();
	return file;
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	navigation.goto.mockReset();
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: '2026-08-31T00:00:00Z',
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('repository content form state ownership', () => {
	it('keeps the newest blob after the first A -> B -> A load finishes', async () => {
		const firstVisit = deferred<ReturnType<typeof blob>>();
		repos.blob
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(blob('middle blob', 'bbbb'))
			.mockResolvedValueOnce(blob('current blob', 'cccc'));
		setTestPage('/alice/demo/blob/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		rendered = await renderComponent(BlobPage);

		setTestPage('/bob/other/blob/notes/readme.md?ref=main', {
			owner: 'bob',
			repo: 'other',
			path: 'notes/readme.md',
		});
		await settle();
		setTestPage('/alice/demo/blob/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		await settle();
		expect(rendered.container.textContent).toContain('current blob');

		firstVisit.resolve(blob('stale blob', 'aaaa'));
		await settle();
		expect(rendered.container.textContent).toContain('current blob');
		expect(rendered.container.textContent).not.toContain('stale blob');
		expect(repos.blob.mock.calls[0]).toEqual([
			'alice',
			'demo',
			'notes/readme.md',
			'main',
		]);
	});

	it('serializes delete and rejects navigation from the first A -> B -> A visit', async () => {
		const firstDelete = deferred<void>();
		const currentDelete = deferred<void>();
		repos.blob
			.mockResolvedValueOnce(blob('first blob', 'aaaa'))
			.mockResolvedValueOnce(blob('middle blob', 'bbbb'))
			.mockResolvedValueOnce(blob('current blob', 'cccc'));
		repos.deleteContent
			.mockReturnValueOnce(firstDelete.promise)
			.mockReturnValueOnce(currentDelete.promise);
		setTestPage('/alice/demo/blob/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		rendered = await renderComponent(BlobPage);

		await click(element(rendered.container, '.file-actions .danger'));
		await dispatchClickTwice(element(rendered.container, '.delete-actions .btn-danger'));
		expect(repos.deleteContent).toHaveBeenCalledOnce();

		setTestPage('/bob/other/blob/notes/readme.md?ref=main', {
			owner: 'bob',
			repo: 'other',
			path: 'notes/readme.md',
		});
		await settle();
		setTestPage('/alice/demo/blob/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		await settle();
		await click(element(rendered.container, '.file-actions .danger'));
		await dispatchClickTwice(element(rendered.container, '.delete-actions .btn-danger'));
		expect(repos.deleteContent).toHaveBeenCalledTimes(2);

		firstDelete.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(
			element<HTMLButtonElement>(rendered.container, '.delete-actions .btn-danger').disabled,
		).toBe(true);

		currentDelete.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo?ref=main');
	});

	it('binds new-file save and busy state to the owning route generation', async () => {
		const firstSave = deferred<void>();
		const currentSave = deferred<void>();
		repos.saveContent.mockReturnValueOnce(firstSave.promise).mockReturnValueOnce(currentSave.promise);
		setTestPage('/alice/demo/new?path=notes/readme.md&ref=main', {
			owner: 'alice',
			repo: 'demo',
		});
		rendered = await renderComponent(NewFilePage);
		await input(element(rendered.container, '#file-content'), 'first content');
		await dispatchClickTwice(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).toHaveBeenCalledOnce();

		setTestPage('/bob/other/new?path=notes/readme.md&ref=main', {
			owner: 'bob',
			repo: 'other',
		});
		await settle();
		setTestPage('/alice/demo/new?path=notes/readme.md&ref=main', {
			owner: 'alice',
			repo: 'demo',
		});
		await settle();
		await input(element(rendered.container, '#file-content'), 'current content');
		await dispatchClickTwice(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).toHaveBeenCalledTimes(2);

		firstSave.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(element<HTMLButtonElement>(rendered.container, '.form-actions .btn-primary').disabled).toBe(
			true,
		);

		currentSave.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith(
			'/alice/demo/blob/notes/readme.md?ref=main',
		);
	});

	it('reloads the edit form by route and rejects the first A -> B -> A blob', async () => {
		const firstVisit = deferred<ReturnType<typeof blob>>();
		repos.blob
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(blob('middle content', 'bbbb'))
			.mockResolvedValueOnce(blob('current content', 'cccc'));
		setTestPage('/alice/demo/edit/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		rendered = await renderComponent(EditFilePage);

		setTestPage('/bob/other/edit/notes/readme.md?ref=main', {
			owner: 'bob',
			repo: 'other',
			path: 'notes/readme.md',
		});
		await settle();
		setTestPage('/alice/demo/edit/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		await settle();
		expect(element<HTMLTextAreaElement>(rendered.container, '#file-content').value).toBe(
			'current content',
		);

		firstVisit.resolve(blob('stale content', 'aaaa'));
		await settle();
		expect(element<HTMLTextAreaElement>(rendered.container, '#file-content').value).toBe(
			'current content',
		);
	});

	it('serializes edit save and rejects stale navigation after route re-entry', async () => {
		const firstSave = deferred<void>();
		const currentSave = deferred<void>();
		repos.blob
			.mockResolvedValueOnce(blob('first content', 'aaaa'))
			.mockResolvedValueOnce(blob('middle content', 'bbbb'))
			.mockResolvedValueOnce(blob('current content', 'cccc'));
		repos.saveContent.mockReturnValueOnce(firstSave.promise).mockReturnValueOnce(currentSave.promise);
		setTestPage('/alice/demo/edit/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		rendered = await renderComponent(EditFilePage);
		await input(element(rendered.container, '#file-content'), 'first edit');
		await dispatchClickTwice(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).toHaveBeenCalledOnce();

		setTestPage('/bob/other/edit/notes/readme.md?ref=main', {
			owner: 'bob',
			repo: 'other',
			path: 'notes/readme.md',
		});
		await settle();
		setTestPage('/alice/demo/edit/notes/readme.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'notes/readme.md',
		});
		await settle();
		await input(element(rendered.container, '#file-content'), 'current edit');
		await dispatchClickTwice(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).toHaveBeenCalledTimes(2);

		firstSave.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(element<HTMLButtonElement>(rendered.container, '.form-actions .btn-primary').disabled).toBe(
			true,
		);

		currentSave.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith(
			'/alice/demo/blob/notes/readme.md?ref=main',
		);
	});

	it('keeps the newest release form after the first A -> B -> A load finishes', async () => {
		const firstVisit = deferred<ReturnType<typeof release>>();
		releases.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(release('Middle release'))
			.mockResolvedValueOnce(release('Current release'));
		setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
		rendered = await renderComponent(EditReleasePage);

		setTestPage('/bob/other/releases/edit/8', { owner: 'bob', repo: 'other', id: '8' });
		await settle();
		setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
		await settle();
		expect(element<HTMLInputElement>(rendered.container, '#release-title').value).toBe(
			'Current release',
		);

		firstVisit.resolve(release('Stale release'));
		await settle();
		expect(element<HTMLInputElement>(rendered.container, '#release-title').value).toBe(
			'Current release',
		);
	});

	it('serializes release update and rejects stale navigation and busy release', async () => {
		const firstUpdate = deferred<void>();
		const currentUpdate = deferred<void>();
		releases.get
			.mockResolvedValueOnce(release('First release'))
			.mockResolvedValueOnce(release('Middle release'))
			.mockResolvedValueOnce(release('Current release'));
		releases.update
			.mockReturnValueOnce(firstUpdate.promise)
			.mockReturnValueOnce(currentUpdate.promise);
		setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
		rendered = await renderComponent(EditReleasePage);
		await dispatchSubmitTwice(element(rendered.container, '.release-form'));
		expect(releases.update).toHaveBeenCalledOnce();

		setTestPage('/bob/other/releases/edit/8', { owner: 'bob', repo: 'other', id: '8' });
		await settle();
		setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
		await settle();
		await dispatchSubmitTwice(element(rendered.container, '.release-form'));
		expect(releases.update).toHaveBeenCalledTimes(2);

		firstUpdate.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(
			element<HTMLButtonElement>(rendered.container, '.release-form button[type="submit"]').disabled,
		).toBe(true);

		currentUpdate.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/releases');
	});

	it('binds package publish, feedback and busy state to the owning route generation', async () => {
		const firstPublish = deferred<void>();
		const currentPublish = deferred<void>();
		packages.publish
			.mockReturnValueOnce(firstPublish.promise)
			.mockReturnValueOnce(currentPublish.promise);
		setTestPage('/alice/demo/packages/upload', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageUploadPage);
		await choosePackageFile(element(rendered.container, '#package-file'), 'first.crate');
		await dispatchSubmitTwice(element(rendered.container, '.package-form'));
		expect(packages.publish).toHaveBeenCalledOnce();

		setTestPage('/bob/other/packages/upload', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/packages/upload', { owner: 'alice', repo: 'demo' });
		await settle();
		await choosePackageFile(element(rendered.container, '#package-file'), 'current.crate');
		await dispatchSubmitTwice(element(rendered.container, '.package-form'));
		expect(packages.publish).toHaveBeenCalledTimes(2);

		firstPublish.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(rendered.container.querySelector('.success-banner')).toBeNull();
		expect(
			element<HTMLButtonElement>(rendered.container, '.package-form button[type="submit"]').disabled,
		).toBe(true);

		currentPublish.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/packages');
	});

	it('does not publish a package error from an obsolete route visit', async () => {
		const firstPublish = deferred<void>();
		const currentPublish = deferred<void>();
		packages.publish
			.mockReturnValueOnce(firstPublish.promise)
			.mockReturnValueOnce(currentPublish.promise);
		setTestPage('/alice/demo/packages/upload', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageUploadPage);
		await choosePackageFile(element(rendered.container, '#package-file'), 'first.crate');
		await dispatchSubmitTwice(element(rendered.container, '.package-form'));

		setTestPage('/bob/other/packages/upload', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/packages/upload', { owner: 'alice', repo: 'demo' });
		await settle();
		await choosePackageFile(element(rendered.container, '#package-file'), 'current.crate');
		await dispatchSubmitTwice(element(rendered.container, '.package-form'));

		firstPublish.reject(new Error('obsolete upload failed'));
		await settle();
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
		expect(
			element<HTMLButtonElement>(rendered.container, '.package-form button[type="submit"]').disabled,
		).toBe(true);

		currentPublish.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/packages');
	});
});
