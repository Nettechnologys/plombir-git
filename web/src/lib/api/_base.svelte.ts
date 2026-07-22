// IronForge API Client — shared internals

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
  const legacy = localStorage.getItem('ironforge_token');
  if (legacy) {
    localStorage.removeItem('ironforge_token');
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
  localStorage.removeItem('ironforge_token');
}

/** Default request timeout: 30 seconds. */
const DEFAULT_TIMEOUT_MS = 30_000;

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
    const body = await res.json().catch(() => ({}));
    // Backend error envelope is { error: { code, message, request_id } }.
    // Reading body.error directly yields "[object Object]" in the UI, so pull
    // out the human-readable message (falling back for older/plain shapes).
    const msg =
      (body?.error && typeof body.error === 'object' ? body.error.message : body?.error) ||
      body?.message ||
      `HTTP ${res.status}`;
    throw new Error(msg);
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
    const body = await res.json().catch(() => ({}));
    const msg =
      (body?.error && typeof body.error === 'object' ? body.error.message : body?.error) ||
      body?.message ||
      `HTTP ${res.status}`;
    throw new Error(msg);
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
