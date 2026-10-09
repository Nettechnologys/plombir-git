import { afterEach, beforeEach, describe, expect, it } from 'vitest';
// This test reads the server's password rules to hold both ends together.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join } from 'node:path';

declare const process: { cwd(): string };

import LoginPage from '../../routes/login/+page.svelte';
import RegisterPage from '../../routes/register/+page.svelte';
import Navbar from '../components/Navbar.svelte';
import { PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH } from '../passwordPolicy';
import { logout } from '../stores/auth.svelte';
import { setRegistrationOpen } from '../stores/instance.svelte';
import { auth, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

// card_e1baa94866ed: "Sign up" was shown on an instance that answers every
// sign-up with 403, and the form asked for 6 characters where the server
// wants 8 with four character classes.

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	await logout();
	resetTestClient();
	auth.listSsoProviders.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	setRegistrationOpen(null);
	document.body.innerHTML = '';
});

describe('a closed instance', () => {
	it('offers no sign-up link in the navigation bar', async () => {
		setRegistrationOpen(false);
		rendered = await renderComponent(Navbar);
		await settle();
		expect(rendered.container.querySelector('a[href="/register"]')).toBeNull();
	});

	it('offers no sign-up link under the sign-in form', async () => {
		setRegistrationOpen(false);
		rendered = await renderComponent(LoginPage);
		await settle();
		expect(rendered.container.querySelector('a[href="/register"]')).toBeNull();
		expect(rendered.container.querySelector('a[href="/forgot-password"]')).not.toBeNull();
	});

	it('says so on the sign-up page instead of offering a form that will be refused', async () => {
		setRegistrationOpen(false);
		rendered = await renderComponent(RegisterPage);
		await settle();
		expect(rendered.container.querySelector('form')).toBeNull();
		expect(rendered.container.textContent).toContain('Self-service sign-up is closed on this instance.');
	});
});

describe('an open instance, or one that has not said', () => {
	it.each([true, null])('keeps the sign-up links (registration_open = %s)', async (open) => {
		setRegistrationOpen(open);
		rendered = await renderComponent(LoginPage);
		await settle();
		expect(rendered.container.querySelector('a[href="/register"]')).not.toBeNull();
	});

	it('states the server’s password rules on the form', async () => {
		setRegistrationOpen(true);
		rendered = await renderComponent(RegisterPage);
		await settle();
		const field = rendered.container.querySelector<HTMLInputElement>('input[type="password"]')!;
		expect(field.minLength).toBe(PASSWORD_MIN_LENGTH);
		expect(field.maxLength).toBe(PASSWORD_MAX_LENGTH);
		expect(rendered.container.textContent).toContain(
			'At least 8 characters, with an uppercase and a lowercase letter, a digit and a symbol.',
		);
	});
});

describe('the password rules', () => {
	it('are the lengths the server enforces', () => {
		const rust = readFileSync(join(process.cwd(), '..', 'crates/rg-core/src/auth/password.rs'), 'utf8');
		const defaults = rust.slice(rust.indexOf('impl Default for PasswordValidator'));
		const body = defaults.slice(0, defaults.indexOf('\n}\n'));
		expect(Number(body.match(/min_length:\s*(\d+)/)?.[1])).toBe(PASSWORD_MIN_LENGTH);
		expect(Number(body.match(/max_length:\s*(\d+)/)?.[1])).toBe(PASSWORD_MAX_LENGTH);
		for (const rule of ['require_uppercase', 'require_lowercase', 'require_digit', 'require_special', 'reject_common', 'reject_username']) {
			expect(body).toMatch(new RegExp(`${rule}:\\s*true`));
		}
	});
});
