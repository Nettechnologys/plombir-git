import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', async () => {
	const session = await import('$lib/test/authSession.svelte');
	return {
		getUser: () => ({ id: 999, username: 'admin' }),
		isAdmin: () => true,
		isAuthReady: session.isAuthReady,
		isLoggedIn: () => true,
	};
});

import AdminSettingsPage from '../../routes/admin/settings/+page.svelte';
import { setTestPage } from '../test/app';
import { setAuthReady } from '../test/authSession.svelte';
import { getBanner } from '../stores/instance.svelte';
import { admin, resetTestClient } from '../test/client';
import {
	answerConfirm,
	button,
	change,
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

const settings = (banner: string | null = null) => ({
	maintenance_mode: false,
	banner_message: banner,
	banner_type: 'info' as const,
});

const provider = (id: number, name: string, type: 'oauth2' | 'ldap' = 'oauth2') => ({
	id,
	name,
	slug: name,
	provider_type: type,
	client_id: null,
	discovery_url: null,
	scopes: null,
	ldap_host: type === 'ldap' ? 'ldap.example.com' : null,
	ldap_port: null,
	ldap_bind_dn: null,
	ldap_base_dn: null,
	ldap_user_filter: null,
	enabled: true,
	auto_provision: false,
	allowed_email_domains: null,
	icon_url: null,
	created_at: timestamp,
	updated_at: timestamp,
});

const attempt = (id: number, username: string) => ({
	id,
	user_id: id,
	username,
	auth_provider: 'password',
	ip_address: '127.0.0.1',
	user_agent: null,
	success: true,
	failure_reason: null,
	created_at: timestamp,
});

const attempts = (id: number, username: string, page: number) => ({
	total: 80,
	page,
	per_page: 20,
	attempts: [attempt(id, username)],
});

function providerRow(container: ParentNode, name: string): HTMLElement {
	const row = Array.from(container.querySelectorAll<HTMLElement>('.provider-row')).find(
		(candidate) => candidate.querySelector('strong')?.textContent?.trim() === name,
	);
	if (!row) throw new Error(`Rendered DOM is missing the provider row for "${name}"`);
	return row;
}

function rowButton(container: ParentNode, name: string, label: string): HTMLButtonElement {
	return button(providerRow(container, name), label);
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setAuthReady(true);
	setTestPage('/admin/settings', {});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

describe('instance settings async state ownership', () => {
	it('gives settings, SSO providers and login attempts independent request owners', async () => {
		const slowSettings = deferred<ReturnType<typeof settings>>();
		admin.getSettings.mockReturnValue(slowSettings.promise);
		admin.listSsoProviders.mockResolvedValue([provider(1, 'keycloak')]);
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'independent-user', 1));

		rendered = await renderComponent(AdminSettingsPage);

		// The composite initial load used to publish all three surfaces in one
		// completion, so a slow `GET /admin/settings` hid the other two.
		expect(rendered.container.textContent).toContain('Loading...');
		expect(rendered.container.querySelector('#admin-maintenance-mode')).toBeNull();
		expect(rendered.container.textContent).toContain('keycloak');
		expect(rendered.container.textContent).toContain('independent-user');

		slowSettings.resolve(settings('scheduled maintenance'));
		await settle();
		expect(rendered.container.querySelector('#admin-maintenance-mode')).not.toBeNull();
		expect(element<HTMLInputElement>(rendered.container, '#admin-banner-message').value).toBe(
			'scheduled maintenance',
		);
		expect(admin.listLoginAttempts).toHaveBeenCalledWith({
			page: 1,
			per_page: 20,
			username: undefined,
			auth_provider: undefined,
			success: undefined,
			start_time: undefined,
			end_time: undefined,
		});
	});

	it('keeps the newest settings load when a re-checked session overlaps the first', async () => {
		const oldest = deferred<ReturnType<typeof settings>>();
		const current = deferred<ReturnType<typeof settings>>();
		admin.getSettings.mockReturnValueOnce(oldest.promise).mockReturnValueOnce(current.promise);
		admin.listSsoProviders.mockResolvedValue([]);
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'unrelated', 1));

		rendered = await renderComponent(AdminSettingsPage);
		// A session re-check re-runs the auth effect, so two initial loads of the
		// same singleton can be in flight at once.
		setAuthReady(false);
		await settle();
		setAuthReady(true);
		await settle();
		expect(admin.getSettings).toHaveBeenCalledTimes(2);

		current.resolve(settings('newest banner'));
		await settle();
		oldest.resolve(settings('stale banner'));
		await settle();
		expect(element<HTMLInputElement>(rendered.container, '#admin-banner-message').value).toBe(
			'newest banner',
		);
	});

	it('does not let a submitted save publish under a settings load that started after it', async () => {
		const firstLoad = deferred<ReturnType<typeof settings>>();
		const reload = deferred<ReturnType<typeof settings>>();
		const save = deferred<ReturnType<typeof settings>>();
		admin.getSettings.mockReturnValueOnce(firstLoad.promise).mockReturnValueOnce(reload.promise);
		admin.updateSettings.mockReturnValue(save.promise);
		admin.listSsoProviders.mockResolvedValue([]);
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'unrelated', 1));

		rendered = await renderComponent(AdminSettingsPage);
		firstLoad.resolve(settings('first banner'));
		await settle();

		await input(
			element<HTMLInputElement>(rendered.container, '#admin-banner-message'),
			'submitted banner',
		);
		await click(button(rendered.container, 'Save Settings'));
		expect(admin.updateSettings).toHaveBeenCalledWith({
			maintenance_mode: false,
			banner_message: 'submitted banner',
			banner_type: 'info',
		});

		setAuthReady(false);
		await settle();
		setAuthReady(true);
		await settle();
		reload.resolve(settings('reloaded banner'));
		await settle();
		expect(element<HTMLInputElement>(rendered.container, '#admin-banner-message').value).toBe(
			'reloaded banner',
		);

		save.resolve(settings('submitted banner'));
		await settle();
		expect(getBanner().message).toBe('reloaded banner');
		expect(element<HTMLInputElement>(rendered.container, '#admin-banner-message').value).toBe(
			'reloaded banner',
		);
	});

	it('does not let a slower provider reload resurrect a row a newer mutation removed', async () => {
		const staleReload = deferred<ReturnType<typeof provider>[]>();
		const currentReload = deferred<ReturnType<typeof provider>[]>();
		admin.getSettings.mockResolvedValue(settings());
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'unrelated', 1));
		admin.listSsoProviders
			.mockResolvedValueOnce([provider(1, 'keycloak'), provider(2, 'removed-provider')])
			.mockReturnValueOnce(staleReload.promise)
			.mockReturnValueOnce(currentReload.promise);
		admin.updateSsoProvider.mockResolvedValue(provider(1, 'keycloak'));
		admin.deleteSsoProvider.mockResolvedValue({ deleted: true });

		rendered = await renderComponent(AdminSettingsPage);
		expect(rendered.container.textContent).toContain('removed-provider');

		await click(rowButton(rendered.container, 'keycloak', 'Disable'));
		await click(rowButton(rendered.container, 'removed-provider', 'Delete'));
		expect(await answerConfirm()).toContain('removed-provider');
		expect(admin.deleteSsoProvider).toHaveBeenCalledWith(2);
		expect(admin.listSsoProviders).toHaveBeenCalledTimes(3);

		currentReload.resolve([provider(1, 'keycloak')]);
		await settle();
		expect(rendered.container.textContent).not.toContain('removed-provider');

		staleReload.resolve([provider(1, 'keycloak'), provider(2, 'removed-provider')]);
		await settle();
		expect(rendered.container.textContent).not.toContain('removed-provider');
	});

	it('keeps the newest login-attempt filter page when responses finish in reverse', async () => {
		const oldest = deferred<ReturnType<typeof attempts>>();
		const staleFailure = deferred<ReturnType<typeof attempts>>();
		const current = deferred<ReturnType<typeof attempts>>();
		admin.getSettings.mockResolvedValue(settings());
		admin.listSsoProviders.mockResolvedValue([]);
		admin.listLoginAttempts
			.mockResolvedValueOnce(attempts(1, 'initial-user', 1))
			.mockReturnValueOnce(oldest.promise)
			.mockReturnValueOnce(staleFailure.promise)
			.mockReturnValueOnce(current.promise);

		rendered = await renderComponent(AdminSettingsPage);

		const username = element<HTMLInputElement>(rendered.container, 'input[placeholder="Username"]');
		const status = element<HTMLSelectElement>(
			rendered.container,
			'select[aria-label="Filter login attempts by status"]',
		);

		await input(username, 'alice');
		await click(button(rendered.container, 'Apply'));
		await input(username, 'bob');
		await click(button(rendered.container, 'Apply'));
		await change(status, 'failure');
		await click(button(rendered.container, 'Apply'));
		expect(admin.listLoginAttempts).toHaveBeenNthCalledWith(4, {
			page: 1,
			per_page: 20,
			username: 'bob',
			auth_provider: undefined,
			success: false,
			start_time: undefined,
			end_time: undefined,
		});

		staleFailure.reject(new Error('stale login attempt failure'));
		await settle();
		expect(rendered.container.textContent).not.toContain('stale login attempt failure');
		expect(rendered.container.textContent).toContain('Loading...');

		current.resolve(attempts(4, 'current-user', 1));
		await settle();
		expect(rendered.container.textContent).toContain('current-user');
		expect(rendered.container.textContent).not.toContain('Loading...');

		oldest.resolve(attempts(2, 'stale-user', 1));
		await settle();
		expect(rendered.container.textContent).toContain('current-user');
		expect(rendered.container.textContent).not.toContain('stale-user');
	});

	it('shares one busy claim across every control of the same provider row', async () => {
		const pendingTest = deferred<{ ok: boolean; message: string }>();
		admin.getSettings.mockResolvedValue(settings());
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'unrelated', 1));
		admin.listSsoProviders.mockResolvedValue([provider(1, 'corp-ldap', 'ldap')]);
		admin.testSsoProvider.mockReturnValue(pendingTest.promise);

		rendered = await renderComponent(AdminSettingsPage);
		await click(rowButton(rendered.container, 'corp-ldap', 'Test connection'));

		for (const label of ['Testing...', 'Disable', 'Edit', 'Delete']) {
			expect(rowButton(rendered.container, 'corp-ldap', label).disabled).toBe(true);
		}

		await click(rowButton(rendered.container, 'corp-ldap', 'Disable'));
		await click(rowButton(rendered.container, 'corp-ldap', 'Edit'));
		await click(rowButton(rendered.container, 'corp-ldap', 'Delete'));
		expect(admin.updateSsoProvider).not.toHaveBeenCalled();
		expect(admin.deleteSsoProvider).not.toHaveBeenCalled();
		expect(document.querySelector('[role="dialog"]')).toBeNull();
		expect(element<HTMLInputElement>(rendered.container, '#sso-name').value).toBe('');

		pendingTest.resolve({ ok: true, message: 'corp-ldap reachable' });
		await settle();
		expect(rendered.container.textContent).toContain('corp-ldap reachable');
		expect(rowButton(rendered.container, 'corp-ldap', 'Delete').disabled).toBe(false);
	});

	it('keeps the newest connection-test result when two rows are tested at once', async () => {
		const slowFirst = deferred<{ ok: boolean; message: string }>();
		const fastSecond = deferred<{ ok: boolean; message: string }>();
		admin.getSettings.mockResolvedValue(settings());
		admin.listLoginAttempts.mockResolvedValue(attempts(1, 'unrelated', 1));
		admin.listSsoProviders.mockResolvedValue([
			provider(1, 'first-ldap', 'ldap'),
			provider(2, 'second-ldap', 'ldap'),
		]);
		admin.testSsoProvider
			.mockReturnValueOnce(slowFirst.promise)
			.mockReturnValueOnce(fastSecond.promise);

		rendered = await renderComponent(AdminSettingsPage);
		await click(rowButton(rendered.container, 'first-ldap', 'Test connection'));
		await click(rowButton(rendered.container, 'second-ldap', 'Test connection'));

		fastSecond.resolve({ ok: true, message: 'second-ldap reachable' });
		await settle();
		expect(rendered.container.textContent).toContain('second-ldap reachable');

		slowFirst.resolve({ ok: false, message: 'first-ldap unreachable' });
		await settle();
		expect(rendered.container.textContent).toContain('second-ldap reachable');
		expect(rendered.container.textContent).not.toContain('first-ldap unreachable');
	});
});
