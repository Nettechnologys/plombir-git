import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RepositorySettingsPage from '../../routes/[owner]/[repo]/settings/+page.svelte';
import MirrorSettingsPage from '../../routes/[owner]/[repo]/settings/mirror/+page.svelte';
import RetentionSettingsPage from '../../routes/[owner]/[repo]/settings/retention/+page.svelte';
import { setTestPage } from '../test/app';
import { ciRetention, mirrors, repos, resetTestClient } from '../test/client';
import {
	answerConfirm,
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

const repository = (id: number, name: string) => ({
	id,
	name,
	description: null,
	is_private: false,
	default_branch: 'main',
	created_at: '2026-08-30T00:00:00Z',
});

const mirror = (id: number, url: string) => ({
	id,
	repo_id: id,
	url,
	username: null,
	has_credentials: false,
	sync_interval_seconds: 86_400,
	next_sync_at: null,
	last_sync_at: null,
	last_sync_error: null,
	status: 'idle',
	created_at: '2026-08-30T00:00:00Z',
	updated_at: '2026-08-30T00:00:00Z',
});

const policy = (artifactDays: number, cacheDays: number) => ({
	artifact_retention_days: artifactDays,
	cache_retention_days: cacheDays,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

async function revisit(path: string): Promise<void> {
	setTestPage(`/bob/other/${path}`, { owner: 'bob', repo: 'other' });
	await settle();
	setTestPage(`/alice/demo/${path}`, { owner: 'alice', repo: 'demo' });
	await settle();
}

describe('repository settings singleton state ownership', () => {
	it('rejects a general-settings response from the first visit after A -> B -> A', async () => {
		const firstVisit = deferred<ReturnType<typeof repository>>();
		repos.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(repository(2, 'middle-repository'))
			.mockResolvedValueOnce(repository(3, 'current-repository'));
		rendered = await renderComponent(RepositorySettingsPage);

		await revisit('settings');
		expect(rendered.container.textContent).toContain('current-repository');

		firstVisit.resolve(repository(1, 'stale-first-visit'));
		await settle();
		expect(rendered.container.textContent).toContain('current-repository');
		expect(rendered.container.textContent).not.toContain('stale-first-visit');
	});

	it('keeps a confirmed mirror update when the first visit load finishes last', async () => {
		const firstVisit = deferred<ReturnType<typeof mirror>>();
		const update = deferred<ReturnType<typeof mirror>>();
		mirrors.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(mirror(2, 'https://middle.example/repo.git'))
			.mockResolvedValueOnce(mirror(3, 'https://before.example/repo.git'));
		mirrors.update.mockReturnValueOnce(update.promise);
		rendered = await renderComponent(MirrorSettingsPage);

		await revisit('settings/mirror');
		const form = element<HTMLFormElement>(rendered.container, 'form.mirror-form');
		await input(element(rendered.container, '#mirror-url'), 'https://current.example/repo.git');
		await submit(form);
		await submit(form);
		expect(mirrors.update).toHaveBeenCalledOnce();

		update.resolve(mirror(3, 'https://current.example/repo.git'));
		await settle();
		firstVisit.resolve(mirror(1, 'https://stale.example/repo.git'));
		await settle();

		expect(element<HTMLInputElement>(rendered.container, '#mirror-url').value).toBe(
			'https://current.example/repo.git',
		);
	});

	it('keeps a confirmed mirror create when the first visit load finishes last', async () => {
		const firstVisit = deferred<ReturnType<typeof mirror>>();
		mirrors.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(null)
			.mockResolvedValueOnce(null);
		mirrors.create.mockResolvedValueOnce(mirror(3, 'https://created.example/repo.git'));
		rendered = await renderComponent(MirrorSettingsPage);

		await revisit('settings/mirror');
		await input(element(rendered.container, '#mirror-url'), 'https://created.example/repo.git');
		await submit(element(rendered.container, 'form.mirror-form'));
		firstVisit.resolve(mirror(1, 'https://stale.example/repo.git'));
		await settle();

		expect(mirrors.create).toHaveBeenCalledOnce();
		expect(element<HTMLInputElement>(rendered.container, '#mirror-url').value).toBe(
			'https://created.example/repo.git',
		);
	});

	it('keeps a confirmed mirror deletion when the first visit load finishes last', async () => {
		const firstVisit = deferred<ReturnType<typeof mirror>>();
		const removal = deferred<void>();
		mirrors.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(mirror(2, 'https://middle.example/repo.git'))
			.mockResolvedValueOnce(mirror(3, 'https://delete.example/repo.git'));
		mirrors.remove.mockReturnValueOnce(removal.promise);
		rendered = await renderComponent(MirrorSettingsPage);

		await revisit('settings/mirror');
		const remove = element<HTMLButtonElement>(rendered.container, '.mirror-form .btn-danger');
		await click(remove);
		await answerConfirm();
		// The second press finds the removal in flight and asks nothing.
		await click(remove);
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();
		expect(mirrors.remove).toHaveBeenCalledOnce();

		removal.resolve();
		await settle();
		firstVisit.resolve(mirror(1, 'https://stale.example/repo.git'));
		await settle();

		expect(element<HTMLInputElement>(rendered.container, '#mirror-url').value).toBe('');
		expect(rendered.container.textContent).not.toContain('https://stale.example/repo.git');
	});

	it('does not let a mirror mutation from the first visit release the current visit busy claim', async () => {
		const firstUpdate = deferred<ReturnType<typeof mirror>>();
		const currentUpdate = deferred<ReturnType<typeof mirror>>();
		mirrors.get.mockResolvedValue(mirror(3, 'https://before.example/repo.git'));
		mirrors.update
			.mockReturnValueOnce(firstUpdate.promise)
			.mockReturnValueOnce(currentUpdate.promise);
		rendered = await renderComponent(MirrorSettingsPage);

		await input(element(rendered.container, '#mirror-url'), 'https://first.example/repo.git');
		await submit(element(rendered.container, 'form.mirror-form'));
		await revisit('settings/mirror');
		await input(element(rendered.container, '#mirror-url'), 'https://current.example/repo.git');
		const currentForm = element<HTMLFormElement>(rendered.container, 'form.mirror-form');
		await submit(currentForm);
		expect(mirrors.update).toHaveBeenCalledTimes(2);

		firstUpdate.resolve(mirror(3, 'https://first.example/repo.git'));
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.mirror-form .btn-primary').disabled).toBe(true);
		await submit(currentForm);
		expect(mirrors.update).toHaveBeenCalledTimes(2);

		currentUpdate.resolve(mirror(3, 'https://current.example/repo.git'));
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.mirror-form .btn-primary').disabled).toBe(false);
	});

	it('keeps a confirmed retention policy when the first visit load finishes last', async () => {
		const firstVisit = deferred<ReturnType<typeof policy>>();
		const update = deferred<ReturnType<typeof policy>>();
		ciRetention.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(policy(20, 5))
			.mockResolvedValueOnce(policy(30, 7));
		ciRetention.update.mockReturnValueOnce(update.promise);
		rendered = await renderComponent(RetentionSettingsPage);

		await revisit('settings/retention');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await input(element(rendered.container, '#artifact-days'), '91');
		await input(element(rendered.container, '#cache-days'), '15');
		await submit(form);
		await submit(form);
		expect(ciRetention.update).toHaveBeenCalledOnce();

		update.resolve(policy(91, 15));
		await settle();
		firstVisit.resolve(policy(1, 1));
		await settle();

		expect(element<HTMLInputElement>(rendered.container, '#artifact-days').value).toBe('91');
		expect(element<HTMLInputElement>(rendered.container, '#cache-days').value).toBe('15');
	});

	it('does not let a retention update from the first visit release the current visit busy claim', async () => {
		const firstUpdate = deferred<ReturnType<typeof policy>>();
		const currentUpdate = deferred<ReturnType<typeof policy>>();
		ciRetention.get.mockResolvedValue(policy(30, 7));
		ciRetention.update
			.mockReturnValueOnce(firstUpdate.promise)
			.mockReturnValueOnce(currentUpdate.promise);
		rendered = await renderComponent(RetentionSettingsPage);

		await input(element(rendered.container, '#artifact-days'), '45');
		await submit(element(rendered.container, 'form'));
		await revisit('settings/retention');
		await input(element(rendered.container, '#artifact-days'), '90');
		const currentForm = element<HTMLFormElement>(rendered.container, 'form');
		await submit(currentForm);
		expect(ciRetention.update).toHaveBeenCalledTimes(2);

		firstUpdate.resolve(policy(45, 7));
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.btn-primary').disabled).toBe(true);
		await submit(currentForm);
		expect(ciRetention.update).toHaveBeenCalledTimes(2);

		currentUpdate.resolve(policy(90, 7));
		await settle();
		expect(element<HTMLButtonElement>(rendered.container, '.btn-primary').disabled).toBe(false);
	});
});
