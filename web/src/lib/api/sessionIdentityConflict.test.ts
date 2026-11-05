import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api/client.svelte', async () => ({
  ...await import('./_base.svelte'),
  ...await import('./auth'),
  ...await import('./passkeys'),
}));

import { getToken, request, setToken } from './_base.svelte';
import {
  adoptConfirmedSession, fetchUser, getSessionCheckError, getUser, isAuthReady, logout,
} from '../stores/auth.svelte';

const fetchMock = vi.fn();
const alice = { id: 1, username: 'alice', email: 'alice@example.test', is_admin: false, display_name: null };
const bob = { ...alice, id: 2, username: 'bob' };
const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), {
  status, headers: { 'content-type': 'application/json' },
});
const conflict = () => reply(401, {
  error: { code: 'UNAUTHORIZED', reason: 'identity_conflict', message: 'Different accounts' },
});

beforeEach(async () => {
  vi.stubGlobal('fetch', fetchMock);
  setToken('alice');
  fetchMock.mockResolvedValueOnce(reply(200, alice));
  await fetchUser();
  fetchMock.mockClear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetAllMocks();
  setToken(null);
});

describe('browser account reconciliation', () => {
  it('rechecks the cookie identity without repeating a refused mutation', async () => {
    fetchMock.mockResolvedValueOnce(conflict()).mockResolvedValueOnce(reply(200, bob));
    await expect(request('/repos', { method: 'POST', body: '{"name":"draft"}' }))
      .rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe('Bearer alice');
    expect(fetchMock.mock.calls[1][0]).toBe('/api/v1/users/me');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBeUndefined();
    expect(getUser()?.id).toBe(bob.id);
    expect(isAuthReady()).toBe(true);
    expect(getToken()).toBeNull();
  });

  it('does not retry logout as the other account', async () => {
    fetchMock.mockResolvedValueOnce(conflict()).mockResolvedValueOnce(reply(200, bob));
    await expect(logout()).rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/v1/users/logout', '/api/v1/users/me',
    ]);
    expect(getUser()?.id).toBe(bob.id);
  });

  it('hides old account forms and retains the conflicting bearer during one shared probe', async () => {
    let resolveProbe!: (response: Response) => void;
    fetchMock.mockResolvedValueOnce(conflict()).mockImplementationOnce(() =>
      new Promise<Response>((resolve) => { resolveProbe = resolve; }));
    const first = request('/repos', { method: 'POST' });
    const firstResult = expect(first).rejects.toMatchObject({ reason: 'identity_conflict' });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(isAuthReady()).toBe(false);
    expect(getUser()).toBeNull();
    expect(getToken()).toBe('alice');

    fetchMock.mockResolvedValueOnce(conflict());
    const second = request('/repos', { method: 'POST' });
    const secondResult = expect(second).rejects.toMatchObject({ reason: 'identity_conflict' });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(3));
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer alice');
    resolveProbe(reply(200, bob));
    await Promise.all([firstResult, secondResult]);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(getUser()?.id).toBe(bob.id);
  });

  it('keeps failing closed after an opaque profile failure and can retry reconciliation', async () => {
    fetchMock.mockResolvedValueOnce(conflict()).mockResolvedValueOnce(new Response('Proxy', { status: 401 }));
    await expect(request('/repos', { method: 'POST' })).rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(isAuthReady()).toBe(false);
    expect(getUser()).toBeNull();
    expect(getToken()).toBe('alice');
    expect(getSessionCheckError()).toContain('HTTP 401');
    fetchMock.mockResolvedValueOnce(conflict()).mockResolvedValueOnce(reply(200, bob));
    await fetchUser();
    expect(getUser()?.id).toBe(bob.id);
    expect(getSessionCheckError()).toBeNull();
  });

  it('does not overwrite a new login with a late cookie profile', async () => {
    let resolveProbe!: (response: Response) => void;
    fetchMock.mockResolvedValueOnce(conflict()).mockImplementationOnce(() =>
      new Promise<Response>((resolve) => { resolveProbe = resolve; }));
    const pending = expect(request('/repos', { method: 'POST' }))
      .rejects.toMatchObject({ reason: 'identity_conflict' });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const carol = { ...alice, id: 3, username: 'carol' };
    fetchMock.mockResolvedValueOnce(reply(200, carol));
    await adoptConfirmedSession('carol');
    resolveProbe(reply(200, bob));
    await pending;
    expect(getUser()?.id).toBe(carol.id);
    expect(getToken()).toBe('carol');
  });

  it('follows an ended browser session instead of silently returning to the old tab account', async () => {
    fetchMock.mockResolvedValueOnce(conflict()).mockResolvedValueOnce(reply(401, {
      error: { code: 'UNAUTHORIZED', message: 'authentication required' },
    }));
    await expect(request('/repos', { method: 'POST' })).rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(getUser()).toBeNull();
    expect(getToken()).toBeNull();
    expect(isAuthReady()).toBe(true);
  });

  it('leaves a retryable error if a failed logout supersedes a pending identity probe', async () => {
    let resolveProbe!: (response: Response) => void;
    fetchMock.mockResolvedValueOnce(conflict()).mockImplementationOnce(() =>
      new Promise<Response>((resolve) => { resolveProbe = resolve; }));
    const pending = expect(request('/repos', { method: 'POST' }))
      .rejects.toMatchObject({ reason: 'identity_conflict' });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    fetchMock.mockResolvedValueOnce(reply(503, { error: { code: 'DB_UNAVAILABLE', message: 'Database unavailable' } }));
    await expect(logout()).rejects.toMatchObject({ status: 503 });
    resolveProbe(reply(200, bob));
    await pending;
    expect(getUser()).toBeNull();
    expect(isAuthReady()).toBe(false);
    expect(getSessionCheckError()).toBe('Database unavailable');
    expect(getToken()).toBe('alice');
  });
});
