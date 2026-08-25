import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import SecurityPage from '../../routes/settings/security/+page.svelte';
import { auth } from './auth';
import { fetchUser, logout } from '../stores/auth.svelte';
import {
	auth as routeAuth,
	mfa,
	passkeys,
	resetTestClient,
} from '../test/client';
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	vi.clearAllMocks();
	resetTestClient();
	vi.stubGlobal('confirm', vi.fn(() => true));
	routeAuth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	routeAuth.listSsoLinks.mockResolvedValue([
		{
			slug: 'corp',
			name: 'Corporate SSO',
			provider_username: 'alice@corp',
			email: 'alice@example.com',
			linked_at: '2026-08-15T12:00:00Z',
			provider_enabled: false,
		},
	]);
	mfa.backup.mockResolvedValue({ total: 0, unused: 0 });
	passkeys.list.mockResolvedValue([]);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

describe('linked SSO identity transport', () => {
  it('reads the links of the signed-in account', () => {
    auth.listSsoLinks();

    expect(base.request).toHaveBeenCalledWith('/users/me/sso');
  });

  it('drops one link by the slug it was created under, escaped', () => {
    auth.unlinkSso('corp idp/staging');

    expect(base.request).toHaveBeenCalledWith('/auth/sso/corp%20idp%2Fstaging/unlink', {
      method: 'DELETE',
    });
  });
});

describe('linked SSO identity production wiring', () => {
  // The defect this file exists for: `DELETE /auth/sso/{slug}/unlink` is
  // mounted, gated `User`, and written so it keeps working after the operator
  // switches the provider off — but nothing in the SPA called it, and no handler
  // listed the links at all. Linking from the web was possible, unlinking was
  // `curl` with a token (card_2cd2d40f27d2).
	it('renders and unlinks an identity whose provider is switched off', async () => {
		rendered = await renderComponent(SecurityPage);
		expect(rendered.container.textContent).toContain('Corporate SSO');
		expect(rendered.container.textContent).toContain('Provider is switched off');

		await click(button(rendered.container, 'Unlink'));
		expect(routeAuth.unlinkSso).toHaveBeenCalledWith('corp');
		expect(
			Array.from(rendered.container.querySelectorAll('button')).some(
				(candidate) => candidate.textContent?.trim() === 'Unlink',
			),
		).toBe(false);
	});
});
