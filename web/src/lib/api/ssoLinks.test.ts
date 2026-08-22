import { describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import securityPageSource from '../../routes/settings/security/+page.svelte?raw';
import { auth } from './auth';

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
  it('reaches the security settings page through both halves of the feature', () => {
    expect(securityPageSource).toContain('auth.listSsoLinks()');
    expect(securityPageSource).toContain('auth.unlinkSso(link.slug)');
  });

  // Listing without an unlink button would be the same defect one step later:
  // the page would name the link it cannot drop.
  it('offers the action on every listed link', () => {
    expect(securityPageSource).toContain('unlinkSsoProvider(link)');
    expect(securityPageSource).toContain('{#each ssoLinks as link (link.slug)}');
  });

  // A link to a provider the operator has since switched off is exactly the
  // case the backend exception was written for, so the page must keep showing
  // it — hiding it would leave its owner with no way to reach the unlink.
  it('keeps a link to a disabled provider visible and says so', () => {
    expect(securityPageSource).toContain('provider_enabled');
    expect(securityPageSource).not.toMatch(/ssoLinks\s*=\s*ssoLinks\.filter\(\s*\(?\w+\)?\s*=>\s*\w+\.provider_enabled/);
  });
});
