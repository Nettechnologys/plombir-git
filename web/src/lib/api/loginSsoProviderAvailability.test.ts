import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import LoginPage from '../../routes/login/+page.svelte';
import { logout } from '../stores/auth.svelte';
import { auth, resetTestClient } from '../test/client';
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn>;

beforeEach(async () => {
	await logout();
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
	auth.listSsoProviders.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	warn.mockRestore();
	await logout();
});

// `GET /auth/sso/providers` answering 5xx used to write `[]`, the same value
// an instance with no configured provider returns. On an SSO-only instance the
// resulting password form and missing provider buttons looked like a deliberate
// configuration choice rather than a failed read (card_8f2dcb146cf3).
describe('login SSO provider availability', () => {
	it('reports a failed provider read instead of drawing it as an empty list', async () => {
		auth.listSsoProviders.mockRejectedValue(new Error('HTTP 503'));

		rendered = await renderComponent(LoginPage);

		expect(rendered.container.textContent).toContain(
			'Single sign-on options could not be loaded.',
		);
		expect(warn).toHaveBeenCalled();
	});

	it('still treats an empty response as an instance with no SSO options', async () => {
		rendered = await renderComponent(LoginPage);

		expect(rendered.container.textContent).not.toContain(
			'Single sign-on options could not be loaded.',
		);
		expect(rendered.container.querySelector('.sso-button')).toBeNull();
	});

	it('retries the read and publishes the recovered provider list', async () => {
		auth.listSsoProviders
			.mockRejectedValueOnce(new Error('HTTP 503'))
			.mockResolvedValueOnce([
				{
					slug: 'corp',
					name: 'Corporate SSO',
					provider_type: 'oidc',
					icon_url: null,
				},
			]);

		rendered = await renderComponent(LoginPage);
		await click(button(rendered.container, 'Retry'));

		expect(auth.listSsoProviders).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('Continue with Corporate SSO');
		expect(rendered.container.textContent).not.toContain(
			'Single sign-on options could not be loaded.',
		);
	});
});
