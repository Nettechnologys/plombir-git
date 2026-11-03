import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import SecurityPage from '../../routes/settings/security/+page.svelte';
import Navbar from '../components/Navbar.svelte';
import SessionStatusBanner from '../components/SessionStatusBanner.svelte';
import {
	fetchUser,
	getSessionCheckError,
	isAuthReady,
	isLoggedIn,
	logout,
} from '../stores/auth.svelte';
import { navigation } from '../test/app';
import { ApiError, auth, resetTestClient, setToken } from '../test/client';
import {
	button,
	click,
	element,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

const user = {
	id: 1,
	username: 'alice',
	email: 'alice@example.test',
	is_admin: false,
	display_name: 'Alice',
};

beforeEach(async () => {
	resetTestClient();
	await logout();
	resetTestClient();
	navigation.goto.mockReset();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function renderLoggedInNavbar(): Promise<RenderedComponent> {
	auth.me.mockResolvedValueOnce(user);
	await fetchUser();
	setToken.mockClear();

	const navbar = await renderComponent(Navbar);
	await click(element(navbar.container, 'button[aria-label="User menu"]'));
	return navbar;
}

describe('session probe failure semantics', () => {
	it.each([403, 500])('keeps a %i unknown, does not clear credentials or redirect, and can retry', async (status) => {
		auth.me.mockRejectedValueOnce(new ApiError('session probe refused', status));

		await fetchUser();

		expect(isLoggedIn()).toBeNull();
		expect(isAuthReady()).toBe(false);
		expect(getSessionCheckError()).toBe('session probe refused');
		expect(setToken).not.toHaveBeenCalled();

		rendered = await renderComponent(SecurityPage);
		expect(navigation.goto).not.toHaveBeenCalled();
		await rendered.destroy();

		auth.me.mockResolvedValueOnce(user);
		rendered = await renderComponent(SessionStatusBanner);
		expect(rendered.container.querySelector('[role="alert"]')?.textContent)
			.toContain('session probe refused');
		await click(button(rendered.container, 'Retry'));

		expect(isLoggedIn()).toBe(true);
		expect(isAuthReady()).toBe(true);
		expect(getSessionCheckError()).toBeNull();
		expect(rendered.container.querySelector('[role="alert"]')).toBeNull();
	});

	it.each([401])(
		'still treats an explicit %i as anonymous and redirects a protected page',
		async (status) => {
			auth.me.mockRejectedValueOnce(new ApiError('authentication required', status));

			await fetchUser();

			expect(isLoggedIn()).toBe(false);
			expect(isAuthReady()).toBe(true);
			expect(getSessionCheckError()).toBeNull();
			expect(setToken).toHaveBeenCalledWith(null);

			rendered = await renderComponent(SecurityPage);
			expect(navigation.goto).toHaveBeenCalledWith('/login');
		},
	);

	it('preserves a known user when a profile refresh is forbidden', async () => {
		auth.me.mockResolvedValueOnce(user);
		await fetchUser();
		setToken.mockClear();
		auth.me.mockRejectedValueOnce(new ApiError('HTTP 403 [content-type: text/html; server: nginx]', 403));

		await fetchUser();

		expect(isLoggedIn()).toBe(true);
		expect(isAuthReady()).toBe(true);
		expect(getSessionCheckError()).toContain('HTTP 403');
		expect(setToken).not.toHaveBeenCalled();
	});
});

describe('logout failure semantics', () => {
	it('waits for server confirmation before clearing the session and navigating', async () => {
		let confirmLogout!: (value: { logged_out: boolean }) => void;
		auth.logout.mockImplementationOnce(
			() => new Promise<{ logged_out: boolean }>((resolve) => { confirmLogout = resolve; }),
		);
		rendered = await renderLoggedInNavbar();

		await click(button(rendered.container, 'Sign out'));

		expect(auth.logout).toHaveBeenCalledOnce();
		expect(isLoggedIn()).toBe(true);
		expect(setToken).not.toHaveBeenCalled();
		expect(navigation.goto).not.toHaveBeenCalled();

		confirmLogout({ logged_out: true });
		await settle();

		expect(isLoggedIn()).toBe(false);
		expect(setToken).toHaveBeenCalledWith(null);
		expect(navigation.goto).toHaveBeenCalledWith('/login');
	});

	it('keeps a known session on 500, exposes the failure, and retries', async () => {
		auth.logout.mockRejectedValueOnce(new ApiError('database unavailable', 500));
		rendered = await renderLoggedInNavbar();

		await click(button(rendered.container, 'Sign out'));

		expect(isLoggedIn()).toBe(true);
		expect(setToken).not.toHaveBeenCalled();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(rendered.container.querySelector('[role="alert"]')?.textContent)
			.toContain('Sign out failed; your session is still active.');
		expect(rendered.container.querySelector('[role="alert"]')?.textContent)
			.toContain('database unavailable');

		auth.logout.mockResolvedValueOnce({ logged_out: true });
		await click(button(rendered.container, 'Retry'));

		expect(auth.logout).toHaveBeenCalledTimes(2);
		expect(isLoggedIn()).toBe(false);
		expect(setToken).toHaveBeenCalledWith(null);
		expect(navigation.goto).toHaveBeenCalledWith('/login');
	});

	it.each([401, 403])('does not claim logout succeeded after an unhandled %i', async (status) => {
        auth.logout.mockRejectedValueOnce(new ApiError('request refused', status));
        rendered = await renderLoggedInNavbar();
        await click(button(rendered.container, 'Sign out'));
        expect(isLoggedIn()).toBe(true);
        expect(setToken).not.toHaveBeenCalled();
        expect(navigation.goto).not.toHaveBeenCalled();
        expect(rendered.container.querySelector('[role="alert"]')?.textContent)
            .toContain('Sign out failed; your session is still active.');
    });

    it('ignores a profile response that arrives after logout', async () => {
        rendered = await renderLoggedInNavbar();
        let resolve!: (value: typeof user) => void;
        auth.me.mockImplementationOnce(() => new Promise<typeof user>((done) => { resolve = done; }));
        const pending = fetchUser();
        await logout();
        resolve(user);
        await pending;
        expect(isLoggedIn()).toBe(false);
    });
});
