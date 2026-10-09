import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import LoginPage from '../../routes/login/+page.svelte';
import RegisterPage from '../../routes/register/+page.svelte';
import { isPasswordChangeRequired, logout } from '../stores/auth.svelte';
import { auth, instance, resetTestClient } from '../test/client';
import { element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn>;

beforeEach(async () => {
	await logout();
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
	auth.listSsoProviders.mockResolvedValue([]);
	instance.get.mockResolvedValue({
		maintenance_mode: false,
		banner_message: null,
		banner_type: 'info',
		attestation_enabled: false,
		source_url: 'https://source.example.test/fork',
		source_commit: null,
		setup_required: false,
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	warn.mockRestore();
	await logout();
});

// card_9f18b657580b: a password an administrator chose opens no session; the
// login form asks for the holder's own and proves the old one again with it.
describe('first sign-in with an administrator-chosen password', () => {
	it('replaces the password before any session exists', async () => {
		auth.login.mockResolvedValue({
			token: '',
			user_id: 3,
			username: 'carol',
			mfa_required: false,
			password_change_required: true,
		});
		auth.setInitialPassword.mockResolvedValue({ token: 'session', user_id: 3, username: 'carol' });
		auth.me.mockResolvedValue({
			id: 3,
			username: 'carol',
			email: 'carol@example.com',
			is_admin: false,
			display_name: null,
		});

		rendered = await renderComponent(LoginPage);
		const login = rendered.container.querySelectorAll<HTMLInputElement>('form input');
		await input(login[0], 'carol');
		await input(login[1], 'abcde-FGHJK-23456-mnpqr');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));

		expect(isPasswordChangeRequired()).toBe(true);
		const fresh = rendered.container.querySelectorAll<HTMLInputElement>('.initial-password-form input');
		await input(fresh[0], 'Carol-own-9');
		await input(fresh[1], 'Carol-own-9');
		await submit(element<HTMLFormElement>(rendered.container, 'form.initial-password-form'));

		expect(auth.setInitialPassword).toHaveBeenCalledWith(
			'carol',
			'abcde-FGHJK-23456-mnpqr',
			'Carol-own-9',
		);
		expect(isPasswordChangeRequired()).toBe(false);
	});
});

// card_45f98ab2fe1a: on a `verify-email` instance the registration answers
// that a link went out, and the page says so instead of signing in.
describe('registration that waits for the address', () => {
	it('tells the visitor to check their inbox', async () => {
		auth.register.mockResolvedValue({
			status: 'confirmation_sent',
			message: 'Check your inbox',
		});
		rendered = await renderComponent(RegisterPage);
		const fields = rendered.container.querySelectorAll<HTMLInputElement>('form input');
		await input(fields[0], 'dave');
		await input(fields[1], 'dave@example.com');
		await input(fields[2], 'Dave-pass-1');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));

		expect(auth.register).toHaveBeenCalledWith('dave', 'dave@example.com', 'Dave-pass-1', undefined);
		expect(auth.login).not.toHaveBeenCalled();
		expect(element(rendered.container, '.confirmation-sent').textContent).toContain(
			'Check your inbox',
		);
		expect(rendered.container.querySelector('input[name="setup_token"]')).toBeNull();
	});
});

// Security audit finding #13: the first account on an empty instance needs the
// one-time setup token from the server's startup log, and the page asks for it
// exactly when `GET /instance` says the instance is still waiting.
describe('the first account on an empty instance', () => {
	it('asks for the setup token and sends it with the registration', async () => {
		instance.get.mockResolvedValue({
			maintenance_mode: false,
			banner_message: null,
			banner_type: 'info',
			attestation_enabled: false,
			source_url: 'https://source.example.test/fork',
			source_commit: null,
			setup_required: true,
		});
		auth.register.mockResolvedValue({ token: 'session', user_id: 1, username: 'founder' });
		auth.login.mockResolvedValue({ token: 'session', user_id: 1, username: 'founder' });
		auth.me.mockResolvedValue({
			id: 1,
			username: 'founder',
			email: 'founder@example.com',
			is_admin: true,
			display_name: null,
		});

		rendered = await renderComponent(RegisterPage);
		const tokenField = await (async () => {
			for (let attempt = 0; attempt < 20; attempt += 1) {
				const field = rendered!.container.querySelector<HTMLInputElement>('input[name="setup_token"]');
				if (field) return field;
				await new Promise((resolve) => setTimeout(resolve, 5));
			}
			throw new Error('the setup token field never appeared');
		})();

		const fields = rendered.container.querySelectorAll<HTMLInputElement>('form input');
		await input(fields[0], 'founder');
		await input(fields[1], 'founder@example.com');
		await input(fields[2], 'Founder-pass-1');
		await input(tokenField, '  one-time-token \n');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));

		expect(auth.register).toHaveBeenCalledWith(
			'founder',
			'founder@example.com',
			'Founder-pass-1',
			'one-time-token',
		);
	});
});
