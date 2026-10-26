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

/** What a bot runs when a person assigns it an issue (ADR-0001). */
export type AgentPreset = 'claude_code' | 'codex' | 'custom';

/**
 * A bot's agent profile as its owner sees it. The model key never comes back;
 * `has_model_key` is all the page learns.
 */
export interface AgentProfile {
  enabled: boolean;
  preset: AgentPreset;
  image: string | null;
  command: string | null;
  runner_tags: string[];
  has_model_key: boolean;
  timeout_secs: number;
  max_runs_per_day: number;
  max_iterations: number;
  /** `owner/name`, as each repository is called now. */
  repositories: string[];
  /** Whether this instance starts agent runs at all (`[agents].enabled`). */
  agents_enabled: boolean;
  created_at: string;
  updated_at: string;
}

/**
 * A complete profile to save. Every field is the new value except the key:
 * leave `model_key` out to keep the stored one, or set `clear_model_key`.
 */
export interface AgentProfileInput {
  enabled: boolean;
  preset: AgentPreset;
  image?: string;
  command?: string;
  runner_tags?: string[];
  model_key?: string;
  clear_model_key?: boolean;
  timeout_secs?: number;
  max_runs_per_day?: number;
  max_iterations?: number;
  repositories: string[];
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
  getAgentProfile: (bot: string) =>
    request<AgentProfile>(`/users/bots/${encodeURIComponent(bot)}/agent`),
  saveAgentProfile: (bot: string, profile: AgentProfileInput) =>
    request<AgentProfile>(`/users/bots/${encodeURIComponent(bot)}/agent`, {
      method: 'PUT',
      body: JSON.stringify(profile),
    }),
  deleteAgentProfile: (bot: string) =>
    request<void>(`/users/bots/${encodeURIComponent(bot)}/agent`, { method: 'DELETE' }),
};
