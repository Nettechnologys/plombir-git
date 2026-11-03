import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Exercise the store and real request layer together, including bearer removal.
vi.mock('$lib/api/client.svelte', async () => ({
  ...await import('./_base.svelte'),
  ...await import('./auth'),
  ...await import('./passkeys'),
}));

import { getToken, setToken } from './_base.svelte';
import { fetchUser, isLoggedIn, logout } from '../stores/auth.svelte';

const fetchMock = vi.fn();
const user = { id: 1, username: 'alice', email: 'alice@example.test', is_admin: false, display_name: null };
const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), {
  status, headers: { 'content-type': 'application/json' },
});
const refused = (status: number, reason?: string) => reply(status, {
  error: { code: 'UNAUTHORIZED', message: 'Request refused', reason },
});

beforeEach(async () => {
  vi.stubGlobal('fetch', fetchMock);
  setToken(null);
  fetchMock.mockResolvedValueOnce(reply(200, user));
  await fetchUser();
  fetchMock.mockClear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetAllMocks();
  setToken(null);
});

describe('logout with mixed browser credentials', () => {
  it('revokes a newer cookie after a stale bearer refusal, so reload stays signed out', async () => {
    setToken('stale');
    fetchMock.mockResolvedValueOnce(refused(401, 'session_revoked'))
      .mockResolvedValueOnce(reply(200, { logged_out: true }));

    await logout();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe('Bearer stale');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBeUndefined();
    expect(fetchMock.mock.calls[1][1].credentials).toBe('include');
    expect(getToken()).toBeNull();
    expect(isLoggedIn()).toBe(false);
    fetchMock.mockResolvedValueOnce(refused(401));
    await fetchUser();
    expect(isLoggedIn()).toBe(false);
  });

  it('confirms an already absent cookie session without retrying', async () => {
    fetchMock.mockResolvedValueOnce(refused(401));
    await logout();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(isLoggedIn()).toBe(false);
  });

  it('bounds recovery to one cookie-only attempt when both credentials have expired', async () => {
    setToken('stale');
    fetchMock.mockResolvedValueOnce(refused(401, 'session_revoked'))
      .mockResolvedValueOnce(refused(401));
    await logout();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(isLoggedIn()).toBe(false);
  });

  it.each([403, 503])('keeps the signed-in UI on a %i logout refusal', async (status) => {
    setToken('valid');
    fetchMock.mockResolvedValueOnce(refused(status));
    await expect(logout()).rejects.toMatchObject({ status });
    expect(isLoggedIn()).toBe(true);
    expect(getToken()).toBe('valid');
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('does not claim success when the cookie-only retry cannot reach the server', async () => {
    setToken('stale');
    fetchMock.mockResolvedValueOnce(refused(401, 'session_revoked'))
      .mockRejectedValueOnce(new TypeError('Network unavailable'));
    await expect(logout()).rejects.toThrow('Network unavailable');
    expect(isLoggedIn()).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('requires an affirmative server response', async () => {
    fetchMock.mockResolvedValueOnce(reply(200, { logged_out: false }));
    await expect(logout()).rejects.toThrow('did not confirm');
    expect(isLoggedIn()).toBe(true);
  });
});
