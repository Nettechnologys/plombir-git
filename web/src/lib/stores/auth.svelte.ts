// Auth state store using Svelte 5 runes

import { setToken, getToken, auth, passkeys, setIdentityConflictHandler } from '$lib/api/client.svelte';
import type { PasskeyDevice } from '$lib/api/passkeys';
import { ApiError } from '$lib/api/error';
import { t } from '$lib/i18n';
import { tick } from 'svelte';

interface User {
  remember_me?: boolean;
  id: number;
  username: string;
  email: string;
  is_admin: boolean;
  display_name: string | null;
}

let currentUser = $state<User | null>(null);
let isLoading = $state(false);
let error = $state<string | null>(null);
let authReady = $state(false); // True after initial fetchUser() completes
let sessionCheckError = $state<string | null>(null);
let pendingMfaUsername = $state<string | null>(null);
// The login (username or email) whose administrator-chosen password has to be
// replaced before any session exists.
let pendingPasswordChangeLogin = $state<string | null>(null);
let sessionGeneration = 0;
let lastRefreshAttempt = -Infinity;
let refreshInFlight: Promise<void> | null = null;
const SESSION_REFRESH_INTERVAL_MS = 5 * 60 * 1000;

setIdentityConflictHandler(async () => {
  const generation = ++sessionGeneration;
  // Unmount account-specific forms while the browser identity is unknown.
  // Keep the old bearer in the request layer until this read completes, so
  // pending clicks cannot silently execute with the other account's cookie.
  currentUser = null;
  authReady = false;
  pendingMfaUsername = null;
  pendingPasswordChangeLogin = null;
  await tick();
  if (generation !== sessionGeneration) return;
  try {
    const me = await auth.me({ omitBearer: true });
    if (generation !== sessionGeneration) return;
    currentUser = sessionUser(me);
    setToken(null);
    authReady = true;
    sessionCheckError = null;
    lastRefreshAttempt = -Infinity;
  } catch (cause) {
    if (generation !== sessionGeneration) return;
    if (cause instanceof ApiError && cause.status === 401 && cause.code === 'UNAUTHORIZED'
        && (cause.reason === null || cause.reason === 'session_revoked')) {
      // The browser session ended during reconciliation. Follow that explicit
      // result rather than silently returning to the previous tab's account.
      setToken(null);
      authReady = true;
      sessionCheckError = null;
      return;
    }
    sessionCheckError = cause instanceof Error ? cause.message : 'Session check failed';
  }
});

function sessionUser(me: Awaited<ReturnType<typeof auth.me>>): User {
  return {
    id: me.id,
    username: me.username,
    email: me.email,
    is_admin: me.is_admin ?? false,
    display_name: me.display_name,
    ...(me.remember_me ? { remember_me: true } : {}),
  };
}

/** Take the session a successful answer opened, and load who it belongs to. */
async function adoptSession(token: string) {
  const generation = ++sessionGeneration;
  lastRefreshAttempt = -Infinity;
  pendingMfaUsername = null;
  pendingPasswordChangeLogin = null;
  setToken(token);
  const me = await auth.me();
  if (generation !== sessionGeneration) return;
  currentUser = sessionUser(me);
  authReady = true;
  sessionCheckError = null;
}

export function getUser() {
  return currentUser;
}

export function isLoggedIn() {
  if (sessionCheckError !== null && currentUser === null) return null;
  return currentUser !== null;
}

export function isAdmin() {
  return currentUser?.is_admin === true;
}

export function getAuthError() {
  return error;
}

export function getAuthLoading() {
  return isLoading;
}

export function isMfaRequired() {
  return pendingMfaUsername !== null;
}

export function beginMfa(username: string) {
  pendingMfaUsername = username;
  error = null;
}

export function isPasswordChangeRequired() {
  return pendingPasswordChangeLogin !== null;
}

export function isAuthReady() {
  return authReady;
}

export function getSessionCheckError() {
  return sessionCheckError;
}

export async function login(username: string, password: string, rememberMe = false) {
  sessionGeneration++;
  isLoading = true;
  error = null;
  try {
    const res = await auth.login(username, password, rememberMe);
    if (res.password_change_required) {
      setToken(null);
      currentUser = null;
      authReady = true;
      sessionCheckError = null;
      pendingMfaUsername = null;
      pendingPasswordChangeLogin = username;
      return false;
    }
    if (res.mfa_required) {
      setToken(null);
      currentUser = null;
      authReady = true;
      sessionCheckError = null;
      pendingMfaUsername = res.username || username;
      return false;
    }

    await adoptSession(res.token);
    return true;
  } catch (e: any) {
    error = e.message || 'Login failed';
    return false;
  } finally {
    isLoading = false;
  }
}

/**
 * Replace the administrator-chosen password the last login answered with
 * `password_change_required`. `password` is that password, proved again by
 * the server. Ends signed in, or at the second-factor form.
 */
export async function completeInitialPassword(password: string, newPassword: string, rememberMe = false) {
  if (!pendingPasswordChangeLogin) {
    error = 'No password change is pending';
    return false;
  }
  isLoading = true;
  error = null;
  try {
    const res = await auth.setInitialPassword(pendingPasswordChangeLogin, password, newPassword, rememberMe);
    if (res.mfa_required) {
      pendingMfaUsername = res.username;
      pendingPasswordChangeLogin = null;
      return false;
    }
    await adoptSession(res.token);
    return true;
  } catch (e: any) {
    error = e.message || 'Could not set the new password';
    return false;
  } finally {
    isLoading = false;
  }
}

export async function verifyMfa(code: string, backup = false) {
  sessionGeneration++;
  if (!pendingMfaUsername) {
    error = 'MFA verification is not pending';
    return false;
  }

  isLoading = true;
  error = null;
  try {
    const res = await auth.verifyMfa(pendingMfaUsername, code, backup);
    await adoptSession(res.token);
    return true;
  } catch (e: any) {
    error = e.message || 'MFA verification failed';
    return false;
  } finally {
    isLoading = false;
  }
}

export async function loginWithPasskey(username: string, device: PasskeyDevice = 'auto', rememberMe = false) {
  sessionGeneration++;
  if (!username.trim()) {
    error = 'Enter your username to sign in with a passkey';
    return false;
  }

  isLoading = true;
  error = null;
  try {
    const res = await passkeys.login(username.trim(), device, rememberMe);
    await adoptSession(res.token);
    return true;
  } catch (e: any) {
    error = e.message || 'Passkey login failed';
    return false;
  } finally {
    isLoading = false;
  }
}

/**
 * `true` when the account exists and is signed in, `'confirmation_sent'` when
 * the instance waits for the address to be proved first, `false` on failure.
 */
export async function register(
  username: string,
  email: string,
  password: string,
  setupToken?: string,
): Promise<boolean | 'confirmation_sent'> {
  isLoading = true;
  error = null;
  try {
    const res = await auth.register(username, email, password, setupToken);
    if ('status' in res && res.status === 'confirmation_sent') {
      return 'confirmation_sent';
    }
    // Auto login after register
    return await login(username, password);
  } catch (e: any) {
    error = e.message || 'Registration failed';
    return false;
  } finally {
    isLoading = false;
  }
}

export async function fetchUser() {
  const generation = sessionGeneration;
  // M-4: Always try to fetch user profile — the HttpOnly cookie is sent
  // automatically. If the cookie is absent or invalid, the API returns 401.
  try {
    const me = await auth.me();
    if (generation !== sessionGeneration) return;
    currentUser = sessionUser(me);
    authReady = true;
    sessionCheckError = null;
  } catch (cause: unknown) {
    if (generation !== sessionGeneration) return;
    if (cause instanceof ApiError && cause.status === 401) {
      setToken(null);
      currentUser = null;
      authReady = true;
      sessionCheckError = null;
      return;
    }

    // A 403 can come from an intermediary's access policy, not a revoked
    // session. A failed probe is not proof that the session ended. Preserve a
    // profile we already know, and keep a first-load route behind the root
    // layout until Retry can establish whether this browser is authenticated.
    sessionCheckError = cause instanceof Error ? cause.message : 'Session check failed';
    authReady = currentUser !== null;
  }
}

export async function logout() {
  // Invalidate pending profile probes so a late answer cannot undo logout.
  sessionGeneration++;
  try {
    const result = await auth.logout();
    if (!result?.logged_out) throw new Error(t('nav.sign_out_failed_detail'));
  } catch (cause) {
    // Logout can invalidate a pending identity probe. If it also fails, leave
    // a retryable check error instead of an indefinitely hidden account UI.
    if (!authReady) {
      sessionCheckError = cause instanceof Error ? cause.message : t('nav.sign_out_failed_detail');
    }
    throw cause;
  } finally {
    sessionGeneration++;
  }
  setToken(null);
  currentUser = null;
  authReady = true;
  sessionCheckError = null;
  pendingMfaUsername = null;
  pendingPasswordChangeLogin = null;
}

/** Sign in with the session a confirmed registration opened. */
export async function adoptConfirmedSession(token: string) {
  await adoptSession(token);
}

/** Forget the account locally once the server has deleted it. */
export function forgetDeletedAccount() {
  sessionGeneration++;
  setToken(null);
  currentUser = null;
  authReady = true;
  sessionCheckError = null;
}

/** Renew on navigation only. Notification polls and idle tabs never call this. */
export function refreshSessionAfterNavigation(): Promise<void> {
  if (refreshInFlight) return refreshInFlight;
  if (!authReady || !currentUser?.remember_me || document.visibilityState === 'hidden'
      || Date.now() - lastRefreshAttempt < SESSION_REFRESH_INTERVAL_MS) return Promise.resolve();
  const generation = sessionGeneration;
  const userId = currentUser.id;
  lastRefreshAttempt = Date.now();
  refreshInFlight = (async () => {
    try {
      const result = await auth.refreshSession();
      if (generation !== sessionGeneration || currentUser?.id !== userId) return;
      if (result.user_id !== userId || !result.remember_me) {
        // Another tab changed the browser identity. Never adopt its bearer
        // while rendering the previous account, and never replay a mutation.
        setToken(null);
        await fetchUser();
        return;
      }
      // The refreshed HttpOnly cookie is authoritative. Retire any stale
      // in-memory copy; refresh never exports a cookie token to JavaScript.
      setToken(null);
    } catch (cause) {
      if (generation !== sessionGeneration) return;
      // A renewal failure is not proof of logout. Let a profile probe decide
      // who is signed in; do not turn transient proxy failures into logout.
      if (cause instanceof ApiError && cause.status === 401) {
        lastRefreshAttempt = Infinity;
        // A cross-site frontend may still have a valid bearer while the
        // browser withholds its Strict cookie. Let the probe verify it.
        await fetchUser();
      }
    } finally {
      refreshInFlight = null;
    }
  })();
  return refreshInFlight;
}
