import { afterEach, expect, it, vi } from 'vitest';
import { auth } from './auth';
import { setToken } from './_base.svelte';

afterEach(() => { vi.unstubAllGlobals(); setToken(null); });
it('renews with the browser cookie, never a stale bearer', async () => {
  const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ user_id: 1, remember_me: true, refreshed: true })));
  vi.stubGlobal('fetch', fetchMock);
  setToken('stale');
  await auth.refreshSession();
  expect(fetchMock.mock.calls[0][0]).toBe('/api/v1/users/me/session/refresh');
  expect(fetchMock.mock.calls[0][1].credentials).toBe('include');
  expect(fetchMock.mock.calls[0][1].headers.Authorization).toBeUndefined();
});
it('passes the choice only at authentication start', async () => {
  const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(new Response('{}')));
  vi.stubGlobal('fetch', fetchMock);
  await auth.login('alice', 'password', true);
  expect(JSON.parse(fetchMock.mock.calls[0][1].body).remember_me).toBe(true);
  await auth.setInitialPassword('alice', 'old', 'new', false);
  expect(JSON.parse(fetchMock.mock.calls[1][1].body).remember_me).toBe(false);
  expect(auth.ssoAuthorizeUrl('company', true)).toBe('/api/v1/auth/sso/company?remember_me=true');
});
