// Auth state store using Svelte 5 runes

import { setToken, getToken, auth, passkeys } from '$lib/api/client.svelte';
import { ApiError } from '$lib/api/error';

interface User {
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

function sessionUser(me: Awaited<ReturnType<typeof auth.me>>): User {
  return {
    id: me.id,
    username: me.username,
    email: me.email,
    is_admin: me.is_admin ?? false,
    display_name: me.display_name,
  };
}

/** Take the session a successful answer opened, and load who it belongs to. */
async function adoptSession(token: string) {
  pendingMfaUsername = null;
  pendingPasswordChangeLogin = null;
  setToken(token);
  currentUser = sessionUser(await auth.me());
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

export async function login(username: string, password: string) {
  isLoading = true;
  error = null;
  try {
    const res = await auth.login(username, password);
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

    pendingMfaUsername = null;
    setToken(res.token);
    // Fetch full profile to get is_admin
    const me = await auth.me();
    currentUser = {
      id: me.id,
      username: me.username,
      email: me.email,
      is_admin: me.is_admin ?? false,
      display_name: me.display_name,
    };
    authReady = true;
    sessionCheckError = null;
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
export async function completeInitialPassword(password: string, newPassword: string) {
  if (!pendingPasswordChangeLogin) {
    error = 'No password change is pending';
    return false;
  }
  isLoading = true;
  error = null;
  try {
    const res = await auth.setInitialPassword(pendingPasswordChangeLogin, password, newPassword);
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
  if (!pendingMfaUsername) {
    error = 'MFA verification is not pending';
    return false;
  }

  isLoading = true;
  error = null;
  try {
    const res = await auth.verifyMfa(pendingMfaUsername, code, backup);
    setToken(res.token);
    pendingMfaUsername = null;
    const me = await auth.me();
    currentUser = {
      id: me.id,
      username: me.username,
      email: me.email,
      is_admin: me.is_admin ?? false,
      display_name: me.display_name,
    };
    authReady = true;
    sessionCheckError = null;
    return true;
  } catch (e: any) {
    error = e.message || 'MFA verification failed';
    return false;
  } finally {
    isLoading = false;
  }
}

export async function loginWithPasskey(username: string) {
  if (!username.trim()) {
    error = 'Enter your username to sign in with a passkey';
    return false;
  }

  isLoading = true;
  error = null;
  try {
    const res = await passkeys.login(username.trim());
    pendingMfaUsername = null;
    setToken(res.token);
    const me = await auth.me();
    currentUser = {
      id: me.id,
      username: me.username,
      email: me.email,
      is_admin: me.is_admin ?? false,
      display_name: me.display_name,
    };
    authReady = true;
    sessionCheckError = null;
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
): Promise<boolean | 'confirmation_sent'> {
  isLoading = true;
  error = null;
  try {
    const res = await auth.register(username, email, password);
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
  // M-4: Always try to fetch user profile — the HttpOnly cookie is sent
  // automatically. If the cookie is absent or invalid, the API returns 401.
  try {
    const me = await auth.me();
    currentUser = {
      id: me.id,
      username: me.username,
      email: me.email,
      is_admin: me.is_admin ?? false,
      display_name: me.display_name,
    };
    authReady = true;
    sessionCheckError = null;
  } catch (cause: unknown) {
    if (cause instanceof ApiError && (cause.status === 401 || cause.status === 403)) {
      setToken(null);
      currentUser = null;
      authReady = true;
      sessionCheckError = null;
      return;
    }

    // A failed session probe is not proof that the session ended. Preserve a
    // profile we already know, and keep a first-load route behind the root
    // layout until Retry can establish whether this browser is authenticated.
    sessionCheckError = cause instanceof Error ? cause.message : 'Session check failed';
    authReady = currentUser !== null;
  }
}

export async function logout() {
  // M-4: Call backend to clear the HttpOnly cookie (JS cannot clear it directly)
  try {
    await auth.logout();
  } catch (cause: unknown) {
    // An explicit authentication rejection means there is no live server
    // session left to revoke, so logout is already complete. Transport and 5xx
    // failures prove no such thing: keep the known local session and let the UI
    // offer a retry instead of falsely confirming a security operation.
    if (!(cause instanceof ApiError && (cause.status === 401 || cause.status === 403))) {
      throw cause;
    }
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
  setToken(null);
  currentUser = null;
  authReady = true;
  sessionCheckError = null;
}
