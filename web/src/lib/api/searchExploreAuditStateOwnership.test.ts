import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AuditPage from '../../routes/admin/audit/+page.svelte';
import ExplorePage from '../../routes/explore/+page.svelte';
import SearchPage from '../../routes/search/+page.svelte';
import { setTestPage } from '../test/app';
import { admin, repos, resetTestClient, search } from '../test/client';
import { click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

const timestamp = '2026-08-30T00:00:00Z';
const searchResult = (id: number, title: string) => ({
	result_type: 'repo',
	id,
	title,
	excerpt: null,
	repo_owner: 'alice',
	repo_name: title,
});
const searchResponse = (id: number, title: string, page: number) => ({
	results: [searchResult(id, title)],
	total: 80,
	page,
	per_page: 20,
});
const repository = (id: number, name: string) => ({
	id,
	owner_name: 'alice',
	name,
	description: `${name} description`,
	stars_count: 0,
	updated_at: timestamp,
});
const exploreResponse = (id: number, name: string, page: number) => ({
	data: [repository(id, name)],
	pagination: { page, per_page: 24, total: 96, total_pages: 4 },
});
const auditLog = (id: number, action: string, details: string | null = null) => ({
	id,
	user_id: 1,
	username: 'alice',
	action,
	resource_type: 'repo',
	resource_id: id,
	resource_name: `repo-${id}`,
	ip_address: '127.0.0.1',
	details,
	created_at: timestamp,
});
const auditResponse = (id: number, action: string, page: number) => ({
	total: 80,
	page,
	per_page: 20,
	logs: [auditLog(id, action)],
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/', {});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('search, explore and audit async state ownership', () => {
	it('keeps the newest URL search data, error and loading state', async () => {
		const oldest = deferred<ReturnType<typeof searchResponse>>();
		const staleFailure = deferred<ReturnType<typeof searchResponse>>();
		const current = deferred<ReturnType<typeof searchResponse>>();
		search.search
			.mockReturnValueOnce(oldest.promise)
			.mockReturnValueOnce(staleFailure.promise)
			.mockReturnValueOnce(current.promise);
		setTestPage('/search?q=old&type=repos&page=1', {});
		rendered = await renderComponent(SearchPage);

		setTestPage('/search?q=middle&type=issues&page=2', {});
		await settle();
		setTestPage('/search?q=current&type=wiki&page=3', {});
		await settle();
		expect(search.search).toHaveBeenNthCalledWith(3, 'current', 'wiki', 3, 20);

		staleFailure.reject(new Error('stale search failure'));
		await settle();
		expect(rendered.container.querySelector('.loading')).not.toBeNull();
		expect(rendered.container.textContent).not.toContain('stale search failure');

		current.resolve(searchResponse(3, 'current-search-result', 3));
		await settle();
		oldest.resolve(searchResponse(1, 'stale-search-result', 1));
		await settle();
		expect(rendered.container.textContent).toContain('current-search-result');
		expect(rendered.container.textContent).not.toContain('stale-search-result');
		expect(rendered.container.querySelector('.loading')).toBeNull();
	});

	it('keeps the newest explore page when same-page requests finish in reverse', async () => {
		const oldest = deferred<ReturnType<typeof exploreResponse>>();
		const staleFailure = deferred<ReturnType<typeof exploreResponse>>();
		const current = deferred<ReturnType<typeof exploreResponse>>();
		repos.explore
			.mockResolvedValueOnce(exploreResponse(1, 'page-one', 1))
			.mockReturnValueOnce(oldest.promise)
			.mockReturnValueOnce(staleFailure.promise)
			.mockReturnValueOnce(current.promise);
		rendered = await renderComponent(ExplorePage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(repos.explore).toHaveBeenNthCalledWith(4, 2, 24);

		staleFailure.reject(new Error('stale explore failure'));
		await settle();
		expect(rendered.container.textContent).toContain('Loading');
		expect(rendered.container.textContent).not.toContain('stale explore failure');

		current.resolve(exploreResponse(4, 'current-explore-result', 2));
		await settle();
		oldest.resolve(exploreResponse(2, 'stale-explore-result', 2));
		await settle();
		expect(rendered.container.textContent).toContain('current-explore-result');
		expect(rendered.container.textContent).not.toContain('stale-explore-result');
	});

	it('keeps the newest audit page when pagination responses finish in reverse', async () => {
		const oldest = deferred<ReturnType<typeof auditResponse>>();
		const staleFailure = deferred<ReturnType<typeof auditResponse>>();
		const current = deferred<ReturnType<typeof auditResponse>>();
		admin.listAuditLogs
			.mockResolvedValueOnce(auditResponse(1, 'page.one', 1))
			.mockReturnValueOnce(oldest.promise)
			.mockReturnValueOnce(staleFailure.promise)
			.mockReturnValueOnce(current.promise);
		rendered = await renderComponent(AuditPage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(admin.listAuditLogs).toHaveBeenNthCalledWith(4, {
			page: 4,
			per_page: 20,
			action: undefined,
			resource_type: undefined,
		});

		staleFailure.reject(new Error('stale audit failure'));
		await settle();
		expect(rendered.container.textContent).toContain('Loading');
		expect(rendered.container.textContent).not.toContain('stale audit failure');

		current.resolve(auditResponse(4, 'current.audit', 4));
		await settle();
		oldest.resolve(auditResponse(2, 'stale.audit', 2));
		await settle();
		expect(rendered.container.textContent).toContain('current.audit');
		expect(rendered.container.textContent).not.toContain('stale.audit');
	});

	it('does not let an old or closed audit detail request publish', async () => {
		const stale = deferred<ReturnType<typeof auditLog>>();
		const closed = deferred<ReturnType<typeof auditLog>>();
		admin.listAuditLogs.mockResolvedValue({
			total: 2,
			page: 1,
			per_page: 20,
			logs: [auditLog(1, 'first.audit'), auditLog(2, 'second.audit')],
		});
		admin.getAuditLog.mockReturnValueOnce(stale.promise).mockReturnValueOnce(closed.promise);
		rendered = await renderComponent(AuditPage);

		const details = Array.from(rendered.container.querySelectorAll<HTMLButtonElement>('.actions button'));
		await click(details[0]);
		await click(details[1]);
		stale.resolve(auditLog(1, 'first.audit', 'stale detail'));
		await settle();
		expect(element(rendered.container, '[role="dialog"]').textContent).toContain('second.audit');
		expect(rendered.container.textContent).not.toContain('stale detail');

		await click(element(rendered.container, '.modal-actions button'));
		closed.resolve(auditLog(2, 'second.audit', 'closed detail'));
		await settle();
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();
		expect(rendered.container.textContent).not.toContain('closed detail');
	});
});
