// Auth state store using Svelte 5 runes

import { setToken, getToken, auth } from '$lib/api/client.svelte';

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
let pendingMfaUsername = $state<string | null>(null);

export function getUser() {
  return currentUser;
}

export function isLoggedIn() {
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

export function isAuthReady() {
  return authReady;
}

export async function login(username: string, password: string) {
  isLoading = true;
  error = null;
  try {
    const res = await auth.login(username, password);
    if (res.mfa_required) {
      setToken(null);
      currentUser = null;
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
    return true;
  } catch (e: any) {
    error = e.message || 'Login failed';
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
    return true;
  } catch (e: any) {
    error = e.message || 'MFA verification failed';
    return false;
  } finally {
    isLoading = false;
  }
}

export async function register(username: string, email: string, password: string) {
  isLoading = true;
  error = null;
  try {
    await auth.register(username, email, password);
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
  } catch {
    setToken(null);
    currentUser = null;
  } finally {
    authReady = true;
  }
}

export async function logout() {
  // M-4: Call backend to clear the HttpOnly cookie (JS cannot clear it directly)
  try {
    await auth.logout();
  } catch {
    // Ignore errors — cookie may already be cleared
  }
  setToken(null);
  currentUser = null;
  pendingMfaUsername = null;
}
