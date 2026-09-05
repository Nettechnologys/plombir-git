import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import PackageIndexPage from '../../routes/[owner]/[repo]/packages/+page.svelte';
import DashboardPage from '../../routes/dashboard/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { fetchUser, logout } from '../stores/auth.svelte';
import { navigation, setTestPage } from '../test/app';
import { auth, orgs, packages, repos, resetTestClient } from '../test/client';
import {
	change,
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

const timestamp = '2026-08-31T12:00:00Z';

function packageResponse(name: string, page = 1, totalPages = 1, failedRegistryTypes: string[] = []) {
	return {
		data: [
			{
				name,
				format: 'npm',
				description: `${name} description`,
				latest_version: '1.0.0',
				created_at: timestamp,
			},
		],
		pagination: {
			page,
			per_page: 20,
			total: totalPages * 20,
			total_pages: totalPages,
			has_next: page < totalPages,
			has_prev: page > 1,
		},
		failedRegistryTypes,
	};
}

function emptyPackageResponse(failedRegistryTypes: string[] = []) {
	return {
		...packageResponse('unused', 1, 1, failedRegistryTypes),
		data: [],
		pagination: {
			page: 1,
			per_page: 20,
			total: 0,
			total_pages: 1,
			has_next: false,
			has_prev: false,
		},
	};
}

function repository(name: string) {
	return {
		id: 1,
		name,
		description: `${name} description`,
		is_private: false,
		default_branch: 'main',
		created_at: timestamp,
	};
}

function repositoryList(name: string) {
	return {
		data: [repository(name)],
		pagination: {
			page: 1,
			per_page: 20,
			total: 1,
			total_pages: 1,
			has_next: false,
			has_prev: false,
		},
	};
}

function organization(name: string) {
	return {
		id: 1,
		name,
		display_name: name,
		description: `${name} description`,
		visibility: 'public',
		created_at: timestamp,
	};
}

function template(name: string) {
	return { data: [{ key: name, name, description: `${name} description` }] };
}

function user(username: string) {
	return {
		id: username === 'alice' ? 1 : 2,
		username,
		email: `${username}@example.test`,
		is_admin: false,
		display_name: username,
	};
}

async function setAccount(username: string): Promise<void> {
	auth.me.mockResolvedValueOnce(user(username));
	await fetchUser();
	await settle();
}

function prepareRepoHeader(): void {
	repos.get.mockResolvedValue(repository('demo'));
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
}

function prepareDashboardLoads(): void {
	repos.list.mockResolvedValue(repositoryList('current-repo'));
	orgs.list.mockResolvedValue([organization('current-org')]);
	repos.templates.gitignores.mockResolvedValue(template('current-ignore'));
	repos.templates.licenses.mockResolvedValue(template('current-license'));
	repos.templates.readmes.mockResolvedValue(template('current-readme'));
	repos.templates.labels.mockResolvedValue(template('current-labels'));
}

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	await logout();
	resetTestClient();
	navigation.goto.mockReset();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('package index and dashboard state ownership', () => {
	it('rejects a package list from the first A -> B -> A repository visit', async () => {
		const firstVisit = deferred<ReturnType<typeof packageResponse>>();
		prepareRepoHeader();
		packages.list
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(packageResponse('middle-package'))
			.mockResolvedValueOnce(packageResponse('current-package'));
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageIndexPage);

		setTestPage('/bob/other/packages', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current-package');

		firstVisit.resolve(packageResponse('stale-package'));
		await settle();
		expect(rendered.container.textContent).toContain('current-package');
		expect(rendered.container.textContent).not.toContain('stale-package');
	});

	it('sends one package request per filter/search intent and keeps the newest result', async () => {
		const oldFormat = deferred<ReturnType<typeof packageResponse>>();
		prepareRepoHeader();
		packages.list
			.mockResolvedValueOnce(packageResponse('initial-package'))
			.mockReturnValueOnce(oldFormat.promise)
			.mockResolvedValueOnce(packageResponse('current-search'));
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageIndexPage);

		await change(element(rendered.container, '#format-filter'), 'npm');
		await input(element(rendered.container, '.search-group input'), 'current');
		await click(element(rendered.container, '.search-group button'));

		expect(packages.list).toHaveBeenCalledTimes(3);
		expect(packages.list.mock.calls[1]).toEqual(['alice', 'demo', 'npm', 1, 20, '']);
		expect(packages.list.mock.calls[2]).toEqual(['alice', 'demo', 'npm', 1, 20, 'current']);
		expect(rendered.container.textContent).toContain('current-search');

		oldFormat.resolve(packageResponse('stale-format'));
		await settle();
		expect(rendered.container.textContent).toContain('current-search');
		expect(rendered.container.textContent).not.toContain('stale-format');
	});

	it('binds package pagination responses to the requested page', async () => {
		const pageTwo = deferred<ReturnType<typeof packageResponse>>();
		prepareRepoHeader();
		packages.list
			.mockResolvedValueOnce(packageResponse('page-one', 1, 3))
			.mockReturnValueOnce(pageTwo.promise)
			.mockResolvedValueOnce(packageResponse('page-three', 3, 3));
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageIndexPage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(packages.list).toHaveBeenCalledTimes(3);
		expect(packages.list.mock.calls[1]).toEqual(['alice', 'demo', undefined, 2, 20, '']);
		expect(packages.list.mock.calls[2]).toEqual(['alice', 'demo', undefined, 3, 20, '']);
		expect(rendered.container.textContent).toContain('page-three');

		pageTwo.resolve(packageResponse('stale-page-two', 2, 3));
		await settle();
		expect(rendered.container.textContent).toContain('page-three');
		expect(rendered.container.textContent).not.toContain('stale-page-two');
	});

	it('qualifies an incomplete package list and retries instead of claiming it is empty', async () => {
		prepareRepoHeader();
		packages.list
			.mockResolvedValueOnce(emptyPackageResponse(['npm', 'cargo']))
			.mockResolvedValueOnce(packageResponse('recovered-package'));
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PackageIndexPage);

		const banner = element(rendered.container, '.partial-banner');
		expect(banner.textContent).toContain('npm');
		expect(banner.textContent).toContain('Cargo');
		expect(rendered.container.textContent).not.toContain(en.packages.no_packages);
		expect(rendered.container.querySelector('.empty')).toBeNull();

		await click(element(rendered.container, '.partial-retry'));
		expect(packages.list).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('recovered-package');
		expect(rendered.container.querySelector('.partial-banner')).toBeNull();
	});

	it('has the incomplete-list message in both catalogs', () => {
		expect(en.packages).toHaveProperty('partial_unavailable');
		expect(zhCN.packages).toHaveProperty('partial_unavailable');
	});

	it('owns dashboard repositories, organizations and templates by account visit', async () => {
		const firstRepos = deferred<ReturnType<typeof repositoryList>>();
		const firstOrgs = deferred<ReturnType<typeof organization>[]>();
		const firstGitignores = deferred<ReturnType<typeof template>>();
		const currentTemplates = [
			'current-ignore',
			'current-license',
			'current-readme',
			'current-labels',
		];
		const staleTemplates = [
			'stale-ignore',
			'stale-license',
			'stale-readme',
			'stale-labels',
		];
		repos.list
			.mockReturnValueOnce(firstRepos.promise)
			.mockResolvedValueOnce(repositoryList('middle-repo'))
			.mockResolvedValueOnce(repositoryList('current-repo'));
		orgs.list
			.mockReturnValueOnce(firstOrgs.promise)
			.mockResolvedValueOnce([organization('middle-org')])
			.mockResolvedValueOnce([organization('current-org')]);
		repos.templates.gitignores
			.mockReturnValueOnce(firstGitignores.promise)
			.mockResolvedValueOnce(template('middle-ignore'))
			.mockResolvedValueOnce(template('current-ignore'));
		for (const [method, prefix] of [
			[repos.templates.licenses, 'license'],
			[repos.templates.readmes, 'readme'],
			[repos.templates.labels, 'labels'],
		] as const) {
			method
				.mockResolvedValueOnce(template(`stale-${prefix}`))
				.mockResolvedValueOnce(template(`middle-${prefix}`))
				.mockResolvedValueOnce(template(`current-${prefix}`));
		}
		setTestPage('/dashboard', {});
		await setAccount('alice');
		rendered = await renderComponent(DashboardPage);

		await setAccount('bob');
		await setAccount('alice');
		await click(element(rendered.container, '.dashboard-header .btn-primary'));
		expect(rendered.container.textContent).toContain('current-repo');
		expect(rendered.container.textContent).toContain('current-org');
		expect(repos.templates.gitignores).toHaveBeenCalledTimes(3);
		expect(repos.templates.licenses).toHaveBeenCalledTimes(3);
		expect(repos.templates.readmes).toHaveBeenCalledTimes(3);
		expect(repos.templates.labels).toHaveBeenCalledTimes(3);
		for (const currentTemplate of currentTemplates) {
			expect(rendered.container.textContent).toContain(currentTemplate);
		}

		firstRepos.resolve(repositoryList('stale-repo'));
		firstOrgs.resolve([organization('stale-org')]);
		firstGitignores.resolve(template('stale-ignore'));
		await settle();
		expect(rendered.container.textContent).toContain('current-repo');
		expect(rendered.container.textContent).toContain('current-org');
		for (const currentTemplate of currentTemplates) {
			expect(rendered.container.textContent).toContain(currentTemplate);
		}
		expect(rendered.container.textContent).not.toContain('stale-repo');
		expect(rendered.container.textContent).not.toContain('stale-org');
		for (const staleTemplate of staleTemplates) {
			expect(rendered.container.textContent).not.toContain(staleTemplate);
		}
	});

	it('does not let an old account create clear or navigate the current dashboard form', async () => {
		const firstCreate = deferred<{ id: number; name: string }>();
		prepareDashboardLoads();
		repos.create.mockReturnValueOnce(firstCreate.promise);
		setTestPage('/dashboard', {});
		await setAccount('alice');
		rendered = await renderComponent(DashboardPage);

		await click(element(rendered.container, '.dashboard-header .btn-primary'));
		await input(element(rendered.container, 'form input[type="text"]'), 'alice-old');
		const firstForm = element<HTMLFormElement>(rendered.container, 'form');
		await submit(firstForm);
		await submit(firstForm);
		expect(repos.create).toHaveBeenCalledOnce();
		expect(element<HTMLButtonElement>(rendered.container, 'button[type="submit"]').disabled).toBe(true);

		await setAccount('bob');
		await click(element(rendered.container, '.dashboard-header .btn-primary'));
		const currentName = element<HTMLInputElement>(rendered.container, 'form input[type="text"]');
		await input(currentName, 'bob-current');

		firstCreate.resolve({ id: 1, name: 'alice-old' });
		await settle();
		expect(currentName.value).toBe('bob-current');
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(element<HTMLButtonElement>(rendered.container, 'button[type="submit"]').disabled).toBe(false);
	});
});
