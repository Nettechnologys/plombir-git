// Plombir Git API Client — shared internals

import { ApiError } from './error';

export { ApiError } from './error';

const configuredApiBase =
  typeof import.meta !== 'undefined'
    ? (import.meta as { env?: { VITE_API_BASE?: string } }).env?.VITE_API_BASE
    : undefined;
const configuredSshHost =
  typeof import.meta !== 'undefined'
    ? (import.meta as { env?: { VITE_SSH_HOST?: string } }).env?.VITE_SSH_HOST
    : undefined;
const configuredSshPort =
  typeof import.meta !== 'undefined'
    ? (import.meta as { env?: { VITE_SSH_PORT?: string } }).env?.VITE_SSH_PORT
    : undefined;

function normalizeApiBase(value?: string): string {
  if (!value) return '/api/v1';
  const trimmed = value.trim();
  if (!trimmed || trimmed === '/') return '/api/v1';
  return trimmed.endsWith('/') ? trimmed.slice(0, -1) : trimmed;
}

export const API_BASE = normalizeApiBase(configuredApiBase);

export function withApiBase(path: string): string {
  return `${API_BASE}${path.startsWith('/') ? path : `/${path}`}`;
}

export function withBackendBase(path: string): string {
  const backendBase = API_BASE.replace(/\/api\/v1$/, '');
  return `${backendBase}${path.startsWith('/') ? path : `/${path}`}`;
}

/**
 * The HTTP clone URL for a repository — always absolute in the browser.
 *
 * `withBackendBase` alone returns what the configured API base gives it, and
 * the default base is the relative `/api/v1`, so the clone box used to display
 * `/git/owner/repo`: correct as a fetch target, useless as the thing a user
 * copies into `git clone`. A relative base means "the backend is this origin",
 * so that is what the URL says here. A base configured as an absolute URL —
 * frontend and backend on different hosts — is still the authority and is used
 * verbatim, which is the case the repo-actions contract check guards.
 *
 * Server-side rendering has no origin to name; the relative form is returned
 * there and re-derived on hydration.
 */
export function buildHttpCloneUrl(owner: string, repo: string): string {
  const path = withBackendBase(`/git/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`);
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(path)) return path;
  if (typeof window === 'undefined') return path;
  return `${window.location.origin}${path}`;
}

function normalizeSshHost(host: string): string {
  if (host.includes(':') && !host.startsWith('[')) {
    return `[${host}]`;
  }
  return host;
}

export function buildSshCloneUrl(owner: string, repo: string, fallbackHost?: string): string {
  const host = (configuredSshHost || fallbackHost || '').trim();
  if (!host) return '';

  const port = (configuredSshPort || '2222').trim();
  const portPart = port ? `:${port}` : '';
  return `ssh://git@${normalizeSshHost(host)}${portPart}/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`;
}

let authToken = $state<string | null>(null);

/**
 * M-4: Returns the in-memory token if set (e.g., from login response for
 * WebSocket subprotocol). localStorage is no longer used for token storage.
 * Browser API calls rely on the HttpOnly cookie set by the backend.
 */
export function getToken(): string | null {
  if (typeof window === 'undefined') return null;
  // M-4: Clean up legacy localStorage tokens on first access
  const legacy = localStorage.getItem('plombir_git_token');
  if (legacy) {
    localStorage.removeItem('plombir_git_token');
  }
  return authToken;
}

/**
 * M-4: Sets the in-memory token. localStorage is no longer used.
 * The HttpOnly cookie is set by the backend and cannot be read by JS.
 */
export function setToken(token: string | null) {
  authToken = token;
  if (typeof window === 'undefined') return;
  // M-4: Clean up legacy localStorage tokens
  localStorage.removeItem('plombir_git_token');
}

/** Default request timeout: 30 seconds. */
const DEFAULT_TIMEOUT_MS = 30_000;

async function responseError(res: Response): Promise<ApiError> {
  const body: any = await res.json().catch(() => ({}));
  // Backend error envelope is { error: { code, message, request_id } }.
  // Keep the older/plain shapes readable, but never discard the HTTP status:
  // consumers need it to separate a real absence from a failed read.
  const detail = body?.error && typeof body.error === 'object' ? body.error : null;
  const message = detail?.message || body?.error || body?.message || `HTTP ${res.status}`;
  return new ApiError(
    typeof message === 'string' ? message : `HTTP ${res.status}`,
    res.status,
    typeof detail?.code === 'string' ? detail.code : null,
    typeof detail?.request_id === 'string' ? detail.request_id : null,
  );
}

export async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const token = getToken();
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(options.headers as Record<string, string> || {}),
  };
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  // M-3: Add timeout via AbortSignal to prevent indefinite hangs.
  // If the caller already provided a signal, respect it.
  const timeoutMs = (options as { timeoutMs?: number }).timeoutMs ?? DEFAULT_TIMEOUT_MS;
  let signal = options.signal;
  if (!signal && timeoutMs > 0) {
    signal = AbortSignal.timeout(timeoutMs);
  }

  const res = await fetch(withApiBase(path), { ...options, headers, signal, credentials: 'include' });

  if (!res.ok) {
    throw await responseError(res);
  }

  if (res.status === 204) {
    return undefined as T;
  }

  const text = await res.text();
  if (!text.trim()) {
    return undefined as T;
  }

  return JSON.parse(text) as T;
}

function filenameFromContentDisposition(value: string | null): string | null {
  if (!value) return null;

  const encoded = value.match(/filename\*=UTF-8''([^;]+)/i);
  if (encoded?.[1]) {
    try {
      return decodeURIComponent(encoded[1]);
    } catch {
      return encoded[1];
    }
  }

  const quoted = value.match(/filename="([^"]+)"/i);
  if (quoted?.[1]) return quoted[1];

  const plain = value.match(/filename=([^;]+)/i);
  return plain?.[1]?.trim() || null;
}

export async function downloadApiFile(path: string, fallbackFilename: string): Promise<void> {
  const token = getToken();
  const headers: Record<string, string> = {};
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  // M-3: 5-minute timeout for file downloads (large artifacts).
  // M-4: credentials: 'include' sends the HttpOnly auth cookie.
  const res = await fetch(withApiBase(path), {
    headers,
    signal: AbortSignal.timeout(300_000),
    credentials: 'include',
  });
  if (!res.ok) {
    throw await responseError(res);
  }

  const blob = await res.blob();
  const filename = filenameFromContentDisposition(res.headers.get('content-disposition')) || fallbackFilename;
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const parts = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  return parts.length > 0 ? '?' + parts.join('&') : '';
}

// ── Pagination types ─────────────────────────────────
export interface PaginationMeta {
  page: number;
  per_page: number;
  total: number;
  total_pages: number;
  has_next: boolean;
  has_prev: boolean;
}

export interface PaginatedResponse<T> {
  data: T[];
  pagination: PaginationMeta;
}
