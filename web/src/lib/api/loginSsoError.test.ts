import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
// This test reads the server's sources to hold both ends of one contract.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join } from 'node:path';

declare const process: { cwd(): string };

import LoginPage from '../../routes/login/+page.svelte';
import { t } from '../i18n';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { SSO_ERROR_CODES, readSsoError, ssoErrorMessage } from '../ssoError';
import { logout } from '../stores/auth.svelte';
import { auth, resetTestClient } from '../test/client';
import { renderComponent, type RenderedComponent } from '../test/render';

// card_0d7c54cae647: a refused SSO callback used to show the browser a raw
// `{"error":{...}}` envelope — including the 409 every existing user meets on
// their first provider sign-in. It now redirects to `/login?sso_error=<code>`
// and this page says what happened and what to do.

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	await logout();
	resetTestClient();
	auth.listSsoProviders.mockResolvedValue([
		{ slug: 'corp', name: 'Corporate SSO', provider_type: 'oidc', icon_url: null },
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	window.history.replaceState({}, '', '/');
	await logout();
});

describe('a refused SSO sign-in on the login page', () => {
	it('explains link_required with the provider by name, and cleans the URL', async () => {
		window.history.replaceState({}, '', '/login?sso_error=link_required&provider=corp');

		rendered = await renderComponent(LoginPage);

		const alert = rendered.container.querySelector('[role="alert"]');
		expect(alert?.textContent).toBe(
			'An account here already uses this email address. Sign in to that account and link Corporate SSO under Settings → Security.',
		);
		expect(window.location.search).toBe('');
	});

	it('still tells a member of a closed provider how to get in', async () => {
		window.history.replaceState({}, '', '/login?sso_error=auto_provision_disabled&provider=corp');

		rendered = await renderComponent(LoginPage);

		expect(rendered.container.textContent).toContain(
			'If you already have an account, sign in to it and link Corporate SSO under Settings → Security',
		);
	});

	it('reads a code this build does not know as a failure rather than as nothing', () => {
		expect(readSsoError('?sso_error=from_the_future&provider=corp')).toEqual({
			code: 'failed',
			provider: 'corp',
		});
		expect(readSsoError('?provider=corp')).toBeNull();
	});

	it('shows nothing without a refusal', async () => {
		window.history.replaceState({}, '', '/login');

		rendered = await renderComponent(LoginPage);

		expect(rendered.container.querySelector('[role="alert"]')).toBeNull();
	});
});

describe('the sso_error contract', () => {
	// Every code the server can put in the redirect, read from its source: the
	// `SsoFailure::code` arms, the provisioning rules' own labels, and nothing
	// else. A code the server learns and this page does not would fall back to
	// the generic sentence, which is exactly the vagueness this card removed.
	function serverCodes(): string[] {
		const repo = join(process.cwd(), '..');
		const sso = readFileSync(join(repo, 'crates/rg-http/src/api/sso.rs'), 'utf8');
		const codeFn = sso.slice(sso.indexOf('fn code(self) -> &\'static str'));
		const codeBody = codeFn.slice(0, codeFn.indexOf('\n    }\n}'));
		const provisioning = readFileSync(join(repo, 'crates/rg-core/src/user/provisioning.rs'), 'utf8');
		const reasonFn = provisioning.slice(provisioning.indexOf('pub fn reason(self)'));
		const reasonBody = reasonFn.slice(0, reasonFn.indexOf('\n    }\n'));
		const literals = (body: string) => Array.from(body.matchAll(/=>\s*(?:\{\s*)?"([a-z_]+)"/g), (match) => match[1]);
		return [...literals(codeBody), ...literals(reasonBody)].sort();
	}

	it('knows exactly the codes the server sends', () => {
		const codes = serverCodes();
		expect(codes.length).toBeGreaterThan(15);
		expect(codes).toEqual([...SSO_ERROR_CODES].sort());
	});

	it.each(SSO_ERROR_CODES)('has a sentence for %s in both catalogs', (code) => {
		expect((en.auth.sso_error as Record<string, string>)[code]).toBeTruthy();
		expect((zhCN.auth.sso_error as Record<string, string>)[code]).toBeTruthy();
		const sentence = ssoErrorMessage(t, code, 'Corporate SSO');
		expect(sentence).not.toContain('auth.sso_error');
		expect(sentence).not.toContain('{provider}');
	});

	it('never quotes the provider into markup', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		window.history.replaceState({}, '', '/login?sso_error=failed&provider=%3Cimg%20src%3Dx%3E');

		rendered = await renderComponent(LoginPage);

		expect(rendered.container.querySelector('[role="alert"] img')).toBeNull();
		expect(rendered.container.textContent).toContain('Signing in with <img src=x> failed.');
		warn.mockRestore();
	});
});
