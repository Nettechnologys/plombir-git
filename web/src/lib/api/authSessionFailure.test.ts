import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import SecurityPage from '../../routes/settings/security/+page.svelte';
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
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

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

describe('session probe failure semantics', () => {
	it('keeps a 500 unknown, does not clear credentials or redirect, and can retry', async () => {
		auth.me.mockRejectedValueOnce(new ApiError('database unavailable', 500));

		await fetchUser();

		expect(isLoggedIn()).toBeNull();
		expect(isAuthReady()).toBe(false);
		expect(getSessionCheckError()).toBe('database unavailable');
		expect(setToken).not.toHaveBeenCalled();

		rendered = await renderComponent(SecurityPage);
		expect(navigation.goto).not.toHaveBeenCalled();
		await rendered.destroy();

		auth.me.mockResolvedValueOnce(user);
		rendered = await renderComponent(SessionStatusBanner);
		await click(button(rendered.container, 'Retry'));

		expect(isLoggedIn()).toBe(true);
		expect(isAuthReady()).toBe(true);
		expect(getSessionCheckError()).toBeNull();
		expect(rendered.container.querySelector('[role="alert"]')).toBeNull();
	});

	it.each([401, 403])(
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
});
