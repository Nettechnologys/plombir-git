import { request } from './_base.svelte';
import type { TokenNarrowing, TokenNarrowingFields } from './tokens';

/** A bot account — an AI agent's own identity — as its owner sees it. */
export interface Bot {
  id: number;
  username: string;
  display_name?: string | null;
  is_active: boolean;
  created_at: string;
}

/** A token of a bot. Bots' tokens always carry the `repo` scope only. */
export interface BotToken extends TokenNarrowingFields {
  id: number;
  name: string;
  scopes: string;
  expires_at?: string | null;
  last_used_at?: string | null;
  created_at: string;
}

export const bots = {
  list: () => request<Bot[]>('/users/bots'),
  create: (username: string, display_name?: string) =>
    request<Bot>('/users/bots', {
      method: 'POST',
      body: JSON.stringify({ username, display_name }),
    }),
  delete: (bot: string) => request<void>(`/users/bots/${encodeURIComponent(bot)}`, { method: 'DELETE' }),
  listTokens: (bot: string) => request<BotToken[]>(`/users/bots/${encodeURIComponent(bot)}/tokens`),
  createToken: (bot: string, name: string, expires_at?: string, narrowing: TokenNarrowing = {}) =>
    request<BotToken & { token: string }>(`/users/bots/${encodeURIComponent(bot)}/tokens`, {
      method: 'POST',
      body: JSON.stringify({ name, expires_at, ...narrowing }),
    }),
  deleteToken: (bot: string, id: number) =>
    request<void>(`/users/bots/${encodeURIComponent(bot)}/tokens/${id}`, { method: 'DELETE' }),
};
