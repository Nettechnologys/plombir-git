import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import ImportsPage from '../../routes/imports/+page.svelte';
import PackagePage from '../../routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte';
import PipelinesPage from '../../routes/[owner]/[repo]/pipelines/+page.svelte';
import ReleasesPage from '../../routes/[owner]/[repo]/releases/+page.svelte';
import WikiPage from '../../routes/[owner]/[repo]/wiki/[title]/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import {
	artifacts,
	auth,
	imports,
	instance,
	packages,
	pipelines,
	releases,
	repos,
	resetTestClient,
	wiki,
} from '../test/client';
import {
	answerConfirm,
	button,
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
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const timestamp = '2026-08-30T12:00:00Z';

function wikiPage(content: string, id = 1) {
	return { id, title: 'Home', content, updated_at: timestamp };
}

function release(id: number, tag: string) {
	return {
		id,
		tag_name: tag,
		title: tag,
		body: '',
		is_prerelease: false,
		is_draft: false,
		created_at: timestamp,
	};
}

function releaseAsset(id: number, filename: string) {
	return {
		id,
		release_id: 1,
		filename,
		size: 128,
		download_count: 0,
		content_type: 'application/octet-stream',
		created_at: timestamp,
	};
}

function packageInfo(name: string) {
	return { name, description: `${name} description`, latest_version: '1.0.0', created_at: timestamp };
}

function packageVersion(version: string, isYanked = false) {
	return { version, is_yanked: isYanked, files: [] };
}

function pipelineDetail(id: number, status = 'success') {
	return {
		pipeline: {
			id,
			status,
			commit_sha: `abcdef${id}`,
			commit_message: `Pipeline ${id}`,
			ref_name: 'main',
			started_at: timestamp,
			finished_at: timestamp,
		},
		stages: [],
	};
}

function importTask(id: number, targetName: string) {
	return {
		id,
		platform: 'github',
		source_url: `https://github.com/acme/${targetName}`,
		target_owner: 'alice',
		target_name: targetName,
		status: 'pending',
		progress: 0,
		stage: null,
		error: null,
		created_at: timestamp,
		updated_at: timestamp,
	};
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	// The write controls follow `viewer_permission` (card_3625a7b89abb).
	repos.get.mockResolvedValue({ viewer_permission: 'write' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

describe('repository content and execution state ownership', () => {
	it('rejects a wiki response from the previous A -> B -> A visit', async () => {
		const oldVisit = deferred<ReturnType<typeof wikiPage>>();
		wiki.get
			.mockReturnValueOnce(oldVisit.promise)
			.mockResolvedValueOnce(wikiPage('middle wiki', 2))
			.mockResolvedValueOnce(wikiPage('current wiki', 3));
		wiki.list.mockResolvedValue([]);
		setTestPage('/alice/demo/wiki/Home', { owner: 'alice', repo: 'demo', title: 'Home' });
		rendered = await renderComponent(WikiPage);

		setTestPage('/bob/other/wiki/Home', { owner: 'bob', repo: 'other', title: 'Home' });
		await settle();
		setTestPage('/alice/demo/wiki/Home', { owner: 'alice', repo: 'demo', title: 'Home' });
		await settle();
		expect(rendered.container.textContent).toContain('current wiki');

		oldVisit.resolve(wikiPage('stale wiki'));
		await settle();
		expect(rendered.container.textContent).toContain('current wiki');
		expect(rendered.container.textContent).not.toContain('stale wiki');
	});

	it('keeps the newest wiki revision selection when an older detail finishes last', async () => {
		const oldRevision = deferred<any>();
		wiki.get.mockResolvedValue(wikiPage('current wiki'));
		wiki.list.mockResolvedValue([]);
		wiki.history.mockResolvedValue([
			{ id: 1, version: 1, message: 'old', created_at: timestamp, content: 'old summary' },
			{ id: 2, version: 2, message: 'new', created_at: timestamp, content: 'new summary' },
		]);
		wiki.revision
			.mockReturnValueOnce(oldRevision.promise)
			.mockResolvedValueOnce({ id: 2, version: 2, content: 'current revision' });
		setTestPage('/alice/demo/wiki/Home', { owner: 'alice', repo: 'demo', title: 'Home' });
		rendered = await renderComponent(WikiPage);

		await click(button(rendered.container, 'History'));
		const revisions = rendered.container.querySelectorAll('.revision-header');
		await click(revisions[0]);
		await click(revisions[1]);
		expect(rendered.container.textContent).toContain('current revision');

		oldRevision.resolve({ id: 1, version: 1, content: 'stale revision' });
		await settle();
		expect(rendered.container.textContent).toContain('current revision');
		expect(rendered.container.textContent).not.toContain('stale revision');
	});

	it('rejects a release list from the previous A -> B -> A visit', async () => {
		const oldVisit = deferred<any>();
		instance.get.mockResolvedValue({ attestation_enabled: false });
		releases.list
			.mockReturnValueOnce(oldVisit.promise)
			.mockResolvedValueOnce({ data: [release(2, 'middle-release')], pagination: { total_pages: 1 } })
			.mockResolvedValueOnce({ data: [release(3, 'current-release')], pagination: { total_pages: 1 } });
		releases.listAssets.mockResolvedValue([]);
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);

		setTestPage('/bob/other/releases', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current-release');

		oldVisit.resolve({ data: [release(1, 'stale-release')], pagination: { total_pages: 1 } });
		await settle();
		expect(rendered.container.textContent).toContain('current-release');
		expect(rendered.container.textContent).not.toContain('stale-release');
	});

	it('does not release a new release mutation after returning to the same page', async () => {
		const oldDelete = deferred<void>();
		const currentDelete = deferred<void>();
		instance.get.mockResolvedValue({ attestation_enabled: false });
		releases.list
			.mockResolvedValueOnce({ data: [release(1, 'page-one')], pagination: { total_pages: 2 } })
			.mockResolvedValueOnce({ data: [release(2, 'page-two')], pagination: { total_pages: 2 } })
			.mockResolvedValueOnce({ data: [release(1, 'page-one-current')], pagination: { total_pages: 2 } })
			.mockResolvedValueOnce({ data: [], pagination: { total_pages: 1 } });
		releases.listAssets.mockResolvedValue([]);
		releases.delete
			.mockReturnValueOnce(oldDelete.promise)
			.mockReturnValueOnce(currentDelete.promise);
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);

		await click(element(rendered.container, '.release-actions > button.action-link.danger'));
		await click(element(rendered.container, '.delete-confirm .btn-danger'));
		await click(button(rendered.container, 'Next'));
		await click(button(rendered.container, 'Previous'));
		await click(element(rendered.container, '.release-actions > button.action-link.danger'));
		await click(element(rendered.container, '.delete-confirm .btn-danger'));
		const currentDeleteButton = element<HTMLButtonElement>(rendered.container, '.delete-confirm .btn-danger');
		expect(currentDeleteButton.disabled).toBe(true);

		oldDelete.resolve();
		await settle();
		expect(currentDeleteButton.disabled).toBe(true);
		expect(releases.delete).toHaveBeenCalledTimes(2);

		currentDelete.resolve();
		await settle();
		expect(rendered.container.textContent).not.toContain('page-one-current');
	});

	it('shares one release-asset claim between download and delete', async () => {
		const download = deferred<void>();
		instance.get.mockResolvedValue({ attestation_enabled: false });
		releases.list.mockResolvedValue({
			data: [release(1, 'v1.0.0')],
			pagination: { total_pages: 1 },
		});
		releases.listAssets.mockResolvedValue([releaseAsset(10, 'bundle.zip')]);
		releases.downloadAsset.mockReturnValueOnce(download.promise);
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);

		await click(element(rendered.container, '.asset-link'));
		const deleteButton = element<HTMLButtonElement>(rendered.container, '.asset-delete');
		expect(deleteButton.disabled).toBe(true);
		deleteButton.click();
		await settle();
		expect(releases.deleteAsset).not.toHaveBeenCalled();

		download.resolve();
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.asset-delete').disabled).toBe(false);
	});

	it('rejects package detail from the previous A -> B -> A visit', async () => {
		const oldVisit = deferred<any>();
		packages.get
			.mockReturnValueOnce(oldVisit.promise)
			.mockResolvedValueOnce(packageInfo('middle-package'))
			.mockResolvedValueOnce(packageInfo('current-package'));
		packages.getVersions
			.mockResolvedValueOnce({ versions: [packageVersion('0.1.0')] })
			.mockResolvedValueOnce({ versions: [packageVersion('2.0.0')] })
			.mockResolvedValueOnce({ versions: [packageVersion('3.0.0')] });
		setTestPage('/alice/demo/packages/npm/widget', {
			owner: 'alice', repo: 'demo', format: 'npm', name: 'widget',
		});
		rendered = await renderComponent(PackagePage);

		setTestPage('/bob/other/packages/cargo/crate', {
			owner: 'bob', repo: 'other', format: 'cargo', name: 'crate',
		});
		await settle();
		setTestPage('/alice/demo/packages/npm/widget', {
			owner: 'alice', repo: 'demo', format: 'npm', name: 'widget',
		});
		await settle();
		expect(rendered.container.textContent).toContain('current-package');

		oldVisit.resolve(packageInfo('stale-package'));
		await settle();
		expect(rendered.container.textContent).toContain('current-package');
		expect(rendered.container.textContent).not.toContain('stale-package');
	});

	it('shares one package-version claim between yank and delete', async () => {
		const mutation = deferred<void>();
		packages.get.mockResolvedValue(packageInfo('widget'));
		packages.getVersions
			.mockResolvedValueOnce({ versions: [packageVersion('1.0.0')] })
			.mockResolvedValue({ versions: [packageVersion('1.0.0', true)] });
		packages.yank.mockReturnValueOnce(mutation.promise);
		setTestPage('/alice/demo/packages/npm/widget', {
			owner: 'alice', repo: 'demo', format: 'npm', name: 'widget',
		});
		rendered = await renderComponent(PackagePage);

		await click(button(rendered.container, 'Yank'));
		await click(button(rendered.container, 'Delete'));
		expect(packages.yank).toHaveBeenCalledOnce();
		expect(packages.delete).not.toHaveBeenCalled();

		mutation.resolve();
		await settle();
		expect(button(rendered.container, 'Unyank').disabled).toBe(false);
	});

	it('keeps the newest pipeline selection when an older detail finishes last', async () => {
		const oldSelection = deferred<any>();
		pipelines.list.mockResolvedValue({
			data: [pipelineDetail(1).pipeline, pipelineDetail(2).pipeline],
			pagination: { total_pages: 1 },
		});
		pipelines.get.mockImplementation((_owner: string, _repo: string, id: number) => {
			if (id === 2) return oldSelection.promise;
			return Promise.resolve(pipelineDetail(1));
		});
		repos.branches.mockResolvedValue([]);
		artifacts.list.mockResolvedValue([]);
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);

		const items = rendered.container.querySelectorAll('.pipeline-item');
		await click(items[1]);
		await click(items[0]);
		expect(element(rendered.container, '.pipeline-detail h2').textContent).toContain('1');

		oldSelection.resolve(pipelineDetail(2));
		await settle();
		expect(element(rendered.container, '.pipeline-detail h2').textContent).toContain('1');
		expect(element(rendered.container, '.pipeline-detail h2').textContent).not.toContain('2');
	});

	it('does not release a new pipeline mutation after returning to the same selection', async () => {
		const oldRetry = deferred<void>();
		const currentRetry = deferred<void>();
		pipelines.list.mockResolvedValue({
			data: [pipelineDetail(1, 'failed').pipeline, pipelineDetail(2, 'failed').pipeline],
			pagination: { total_pages: 1 },
		});
		pipelines.get.mockImplementation((_owner: string, _repo: string, id: number) => (
			Promise.resolve(pipelineDetail(id, 'failed'))
		));
		pipelines.retry
			.mockReturnValueOnce(oldRetry.promise)
			.mockReturnValueOnce(currentRetry.promise);
		repos.branches.mockResolvedValue([]);
		artifacts.list.mockResolvedValue([]);
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);

		await click(element(rendered.container, '.detail-actions button'));
		const items = rendered.container.querySelectorAll('.pipeline-item');
		await click(items[1]);
		await click(items[0]);
		await click(element(rendered.container, '.detail-actions button'));
		const currentRetryButton = element<HTMLButtonElement>(rendered.container, '.detail-actions button');
		expect(currentRetryButton.disabled).toBe(true);

		oldRetry.resolve();
		await settle();
		expect(currentRetryButton.disabled).toBe(true);
		expect(pipelines.retry).toHaveBeenCalledTimes(2);

		currentRetry.resolve();
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.detail-actions button').disabled).toBe(false);
	});

	it('keeps a post-create import refresh when the initial list finishes last', async () => {
		const oldList = deferred<any[]>();
		auth.me.mockResolvedValue({
			id: 7,
			username: 'alice',
			email: 'alice@example.com',
			is_admin: false,
			display_name: null,
		});
		await fetchUser();
		imports.list
			.mockReturnValueOnce(oldList.promise)
			.mockResolvedValueOnce([importTask(2, 'current-import')]);
		imports.start.mockResolvedValue({ id: 2 });
		setTestPage('/imports', {});
		rendered = await renderComponent(ImportsPage);

		await input(element(rendered.container, 'input[type="url"]'), 'https://github.com/acme/current-import');
		await submit(element(rendered.container, 'form.import-form'));
		expect(rendered.container.textContent).toContain('current-import');

		oldList.resolve([importTask(1, 'stale-import')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-import');
		expect(rendered.container.textContent).not.toContain('stale-import');
	});

	it('does not let a refresh resurrect an import deleted while it was loading', async () => {
		const removal = deferred<void>();
		const staleRefresh = deferred<any[]>();
		auth.me.mockResolvedValue({
			id: 7,
			username: 'alice',
			email: 'alice@example.com',
			is_admin: false,
			display_name: null,
		});
		await fetchUser();
		imports.list
			.mockResolvedValueOnce([importTask(1, 'remove-me')])
			.mockReturnValueOnce(staleRefresh.promise);
		imports.remove.mockReturnValueOnce(removal.promise);
		setTestPage('/imports', {});
		rendered = await renderComponent(ImportsPage);

		await click(button(rendered.container, 'Delete'));
		await answerConfirm();
		await click(button(rendered.container, 'Refresh'));
		removal.resolve();
		await settle();
		expect(rendered.container.textContent).not.toContain('remove-me');

		staleRefresh.resolve([importTask(1, 'remove-me')]);
		await settle();
		expect(rendered.container.textContent).not.toContain('remove-me');
	});

	it('serializes duplicate import submissions synchronously', async () => {
		const mutation = deferred<any>();
		auth.me.mockResolvedValue({
			id: 7,
			username: 'alice',
			email: 'alice@example.com',
			is_admin: false,
			display_name: null,
		});
		await fetchUser();
		imports.list.mockResolvedValue([]);
		imports.start.mockReturnValueOnce(mutation.promise);
		setTestPage('/imports', {});
		rendered = await renderComponent(ImportsPage);

		await input(element(rendered.container, 'input[type="url"]'), 'https://github.com/acme/demo');
		const form = element<HTMLFormElement>(rendered.container, 'form.import-form');
		await submit(form);
		await submit(form);
		expect(imports.start).toHaveBeenCalledOnce();

		mutation.resolve({ id: 1 });
		await settle();
	});
});
