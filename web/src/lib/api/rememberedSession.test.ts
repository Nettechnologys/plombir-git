import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, auth, resetTestClient, setToken } from '../test/client';
import { adoptConfirmedSession, fetchUser, getUser, logout, refreshSessionAfterNavigation } from '../stores/auth.svelte';

const alice = { id: 1, username: 'alice', email: 'alice@example.test', is_admin: false, display_name: null, remember_me: true };
const renewed = { user_id: 1, remember_me: true, refreshed: true };
async function signIn(remember = true) {
  auth.me.mockResolvedValue({ ...alice, remember_me: remember });
  await adoptConfirmedSession('initial');
  setToken.mockClear();
}
beforeEach(async () => {
  resetTestClient();
  await logout();
  resetTestClient();
  vi.spyOn(Date, 'now').mockReturnValue(1000000);
  auth.refreshSession.mockResolvedValue(renewed);
});
afterEach(() => vi.restoreAllMocks());

describe('remembered session activity', () => {
  it('does not renew anonymous or ordinary sessions, or merely fetching a profile', async () => {
    await refreshSessionAfterNavigation();
    await signIn(false);
    await refreshSessionAfterNavigation();
    await fetchUser();
    expect(auth.refreshSession).not.toHaveBeenCalled();
  });
  it('deduplicates navigation and throttles subsequent activity for five minutes', async () => {
    await signIn();
    let resolve!: (value: typeof renewed) => void;
    auth.refreshSession.mockReturnValueOnce(new Promise(r => { resolve = r; }));
    const first = refreshSessionAfterNavigation();
    const second = refreshSessionAfterNavigation();
    expect(first).toBe(second);
    expect(auth.refreshSession).toHaveBeenCalledTimes(1);
    resolve(renewed);
    await first;
    expect(setToken).toHaveBeenCalledWith(null);
    await refreshSessionAfterNavigation();
    expect(auth.refreshSession).toHaveBeenCalledTimes(1);
    vi.mocked(Date.now).mockReturnValue(1300001);
    await refreshSessionAfterNavigation();
    expect(auth.refreshSession).toHaveBeenCalledTimes(2);
  });
  it('ignores a late refresh after logout', async () => {
    await signIn();
    let resolve!: (value: typeof renewed) => void;
    auth.refreshSession.mockReturnValueOnce(new Promise(r => { resolve = r; }));
    const pending = refreshSessionAfterNavigation();
    await logout();
    setToken.mockClear();
    resolve(renewed);
    await pending;
    expect(setToken).not.toHaveBeenCalled();
    expect(getUser()).toBeNull();
  });
  it('resyncs a changed cookie identity without adopting its token under the old profile', async () => {
    await signIn();
    auth.refreshSession.mockResolvedValueOnce({ ...renewed, user_id: 2 });
    auth.me.mockResolvedValueOnce({ ...alice, id: 2, username: 'bob' });
    await refreshSessionAfterNavigation();
    expect(setToken).toHaveBeenCalledWith(null);
    expect(setToken).not.toHaveBeenCalledWith('renewed');
    expect(getUser()?.username).toBe('bob');
  });
  it('keeps the current session when renewal fails', async () => {
    await signIn();
    auth.refreshSession.mockRejectedValueOnce(new Error('Network unavailable'));
    await refreshSessionAfterNavigation();
    expect(getUser()?.id).toBe(1);
    expect(setToken).not.toHaveBeenCalled();
  });
});

it('does not overwrite a newer login with a late refresh', async () => {
  await signIn();
  let resolve!: (value: typeof renewed) => void;
  auth.refreshSession.mockReturnValueOnce(new Promise(r => { resolve = r; }));
  const pending = refreshSessionAfterNavigation();
  auth.me.mockResolvedValueOnce({ ...alice, id: 2, username: 'bob' });
  await adoptConfirmedSession('bob-token');
  setToken.mockClear();
  resolve(renewed);
  await pending;
  expect(setToken).not.toHaveBeenCalled();
  expect(getUser()?.id).toBe(2);
});

it('checks the profile once after a renewal 401 without assuming logout before its answer', async () => {
  await signIn();
  auth.me.mockClear();
  let resolve!: (value: typeof alice) => void;
  auth.me.mockReturnValueOnce(new Promise(r => { resolve = r; }));
  auth.refreshSession.mockRejectedValueOnce(new ApiError('authentication required', 401));
  const pending = refreshSessionAfterNavigation();
  await vi.waitFor(() => expect(auth.me).toHaveBeenCalledTimes(1));
  expect(getUser()?.id).toBe(1);
  resolve(alice);
  await pending;
  expect(getUser()?.id).toBe(1);
  expect(setToken).not.toHaveBeenCalled();
  vi.mocked(Date.now).mockReturnValue(2000000);
  await refreshSessionAfterNavigation();
  expect(auth.refreshSession).toHaveBeenCalledTimes(1);
});
