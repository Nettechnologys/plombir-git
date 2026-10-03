import { request } from './_base.svelte';

/**
 * What narrows a token beyond its scopes — every field optional; a token with
 * none of them is an ordinary token.
 */
export interface TokenNarrowing {
  /** Confine the token to these repositories, as `owner/name`. */
  repositories?: string[];
  /** Confine the token to these MCP tools; it then works only through `/api/v1/mcp`. */
  mcp_tools?: string[];
  /** Refuse merges, pushes and server-side commits to protected branches. */
  deny_protected_merge?: boolean;
}

/** The narrowing as a token listing reports it; `null` means "not confined". */
export interface TokenNarrowingFields {
  repositories?: string[] | null;
  mcp_tools?: string[] | null;
  deny_protected_merge?: boolean;
}

export const tokens = {
  list: () =>
    request<Array<{
      id: number;
      name: string;
      scopes: string;
      expires_at?: string | null;
      last_used_at?: string | null;
      created_at: string;
      repositories?: string[] | null;
      mcp_tools?: string[] | null;
      deny_protected_merge?: boolean;
    }>>('/users/tokens'),
  create: (name: string, scopes?: string, expires_at?: string, narrowing: TokenNarrowing = {}) =>
    request<{
      id: number;
      name: string;
      token: string;
      scopes: string;
      expires_at?: string;
      created_at: string;
      repositories?: string[] | null;
      mcp_tools?: string[] | null;
      deny_protected_merge?: boolean;
    }>('/users/tokens', {
      method: 'POST',
      body: JSON.stringify({ name, scopes, expires_at, ...narrowing }),
    }),
  delete: (id: number) =>
    request<void>(`/users/tokens/${id}`, { method: 'DELETE' }),
};

/** Split a comma- or newline-separated list, dropping blanks. */
export function splitList(raw: string): string[] {
  return raw
    .split(/[,\n]/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}
