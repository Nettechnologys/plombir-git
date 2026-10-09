<script lang="ts">
  import { copyToClipboard } from '$lib/clipboard';
  import { goto } from '$app/navigation';
  import { tokens } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import { createT, formatDate as formatLocaleDate } from '$lib/i18n';

  const t = createT();

  interface AccessToken {
    id: number;
    name: string;
    scopes: string;
    expires_at?: string | null;
    last_used_at?: string | null;
    created_at: string;
    repositories?: string[] | null;
    mcp_tools?: string[] | null;
    deny_protected_merge?: boolean;
  }

  // What narrows a token beyond its scopes, in one line; empty for a token
  // that is not narrowed.
  function narrowing(token: AccessToken): string {
    const parts: string[] = [];
    if (token.repositories) {
      parts.push(t('access_tokens.narrowing_repos', {
        list: token.repositories.join(', ') || t('access_tokens.narrowing_none'),
      }));
    }
    if (token.mcp_tools) parts.push(t('access_tokens.narrowing_mcp', { list: token.mcp_tools.join(', ') }));
    if (token.deny_protected_merge) parts.push(t('access_tokens.narrowing_protected'));
    return parts.join(' · ');
  }

  let tokenList = $state<AccessToken[]>([]);
  let loading = $state(true);
  let creating = $state(false);
  let busyTokenIds = $state<Set<number>>(new Set());
  let error = $state('');
  let success = $state('');
  let newToken = $state('');
  let name = $state('');
  let scopes = $state('repo');
  let expiresAt = $state('');
  const listRequests = new LatestRequestFence<'tokens'>();

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadTokens();
  });

  async function loadTokens() {
    const claim = listRequests.begin('tokens');
    try {
      loading = true;
      error = '';
      const next = await tokens.list();
      if (listRequests.owns(claim, 'tokens')) tokenList = next;
    } catch (err: any) {
      if (listRequests.owns(claim, 'tokens')) {
        error = err.message || t('access_tokens.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, 'tokens')) loading = false;
    }
  }

  function claimToken(id: number): boolean {
    if (busyTokenIds.has(id)) return false;
    busyTokenIds = new Set(busyTokenIds).add(id);
    return true;
  }

  function releaseToken(id: number): void {
    const next = new Set(busyTokenIds);
    next.delete(id);
    busyTokenIds = next;
  }

  function expiresAtIso() {
    if (!expiresAt) return undefined;
    const parsed = new Date(`${expiresAt}T23:59:59`);
    return Number.isNaN(parsed.getTime()) ? undefined : parsed.toISOString();
  }

  async function createToken(event: SubmitEvent) {
    event.preventDefault();
    if (creating) return;
    if (!name.trim()) {
      error = t('access_tokens.name_required');
      return;
    }

    try {
      creating = true;
      error = '';
      success = '';
      newToken = '';
      const created = await tokens.create(name.trim(), scopes.trim() || 'repo', expiresAtIso());
      newToken = created.token;
      success = t('access_tokens.created_notice');
      name = '';
      scopes = 'repo';
      expiresAt = '';
      await loadTokens();
    } catch (err: any) {
      error = err.message || t('access_tokens.create_failed');
    } finally {
      creating = false;
    }
  }

  async function revokeToken(token: AccessToken) {
    if (!confirm(t('access_tokens.revoke_confirm', { name: token.name }))) return;
    const tokenId = token.id;
    if (!claimToken(tokenId)) return;

    try {
      error = '';
      success = '';
      await tokens.delete(tokenId);
      success = t('access_tokens.revoked');
      await loadTokens();
    } catch (err: any) {
      error = err.message || t('access_tokens.revoke_failed');
    } finally {
      releaseToken(tokenId);
    }
  }

  async function copyNewToken() {
    if (!newToken) return;
    if (await copyToClipboard(newToken)) success = t('access_tokens.copied');
    else error = t('common.copy_failed', 'Copying failed. Select the text and copy it yourself.');
  }

  function formatDate(value?: string | null) {
    if (!value) return t('common.never');
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return formatLocaleDate(value);
  }
</script>

<svelte:head>
  <title>{t('access_tokens.title')} · Plombir Git</title>
</svelte:head>

<div class="page-container tokens-page">
  <header class="page-header">
    <div>
      <h1>{t('access_tokens.title')}</h1>
      <p>{t('access_tokens.description')}</p>
    </div>
  </header>

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if newToken}
    <section class="token-created" aria-label={t('access_tokens.new_token_label')}>
      <div>
        <strong>{t('access_tokens.new_token')}</strong>
        <p>{t('access_tokens.copy_hint')}</p>
      </div>
      <code>{newToken}</code>
      <button type="button" class="btn btn-primary" onclick={copyNewToken}>{t('common.copy')}</button>
    </section>
  {/if}

  <section class="section">
    <h2>{t('access_tokens.create_title')}</h2>
    <form class="create-form" onsubmit={createToken}>
      <label>
        {t('access_tokens.name')}
        <input bind:value={name} placeholder={t('access_tokens.name_placeholder')} disabled={creating} />
      </label>
      <label>
        {t('access_tokens.scopes')}
        <input bind:value={scopes} placeholder="repo" disabled={creating} />
      </label>
      <label>
        {t('access_tokens.expires')}
        <input type="date" bind:value={expiresAt} disabled={creating} />
      </label>
      <button type="submit" class="btn btn-primary" disabled={creating || !name.trim()}>
        {creating ? t('access_tokens.creating') : t('common.create')}
      </button>
    </form>
  </section>

  <section class="section">
    <h2>{t('access_tokens.existing_title')}</h2>

    {#if loading}
      <p class="muted">{t('common.loading')}</p>
    {:else if tokenList.length === 0}
      <div class="empty-state">{t('access_tokens.empty')}</div>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{t('access_tokens.name')}</th>
              <th>{t('access_tokens.scopes')}</th>
              <th>{t('access_tokens.created')}</th>
              <th>{t('access_tokens.last_used')}</th>
              <th>{t('access_tokens.expires')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each tokenList as token (token.id)}
              <tr>
                <td>{token.name}</td>
                <td>
                  <code>{token.scopes}</code>
                  {#if narrowing(token)}<div class="narrowing">{narrowing(token)}</div>{/if}
                </td>
                <td>{formatDate(token.created_at)}</td>
                <td>{formatDate(token.last_used_at)}</td>
                <td>{formatDate(token.expires_at)}</td>
                <td class="actions">
                  <button
                    type="button"
                    class="btn btn-danger"
                    disabled={busyTokenIds.has(token.id)}
                    onclick={() => revokeToken(token)}
                  >
                    {busyTokenIds.has(token.id) ? t('access_tokens.revoking') : t('access_tokens.revoke')}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<style>
  .tokens-page {
    max-width: 980px;
  }

  .page-header {
    margin-bottom: 24px;
  }

  h1 {
    margin: 0 0 6px;
    font-size: 28px;
  }

  h2 {
    margin: 0 0 16px;
    font-size: 18px;
  }

  p {
    margin: 0;
    color: var(--text-secondary);
  }

  .section {
    margin-bottom: 32px;
    padding-bottom: 28px;
    border-bottom: 1px solid var(--border);
  }

  .create-form {
    display: grid;
    grid-template-columns: minmax(180px, 1.2fr) minmax(140px, 0.8fr) minmax(150px, 0.8fr) auto;
    align-items: end;
    gap: 12px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--text-secondary);
    font-size: 13px;
    font-weight: 600;
  }

  input {
    min-height: 36px;
    padding: 7px 10px;
  }

  .token-created,
  .error-box,
  .success-box,
  .empty-state {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px 16px;
    margin-bottom: 20px;
  }

  .token-created {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 12px;
    align-items: center;
    background: var(--bg-secondary);
  }

  .token-created code {
    grid-column: 1 / -1;
    display: block;
    padding: 10px;
    overflow-x: auto;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .error-box {
    color: var(--red);
    background: color-mix(in srgb, var(--red) 10%, transparent);
  }

  .success-box {
    color: var(--green);
    background: color-mix(in srgb, var(--green) 10%, transparent);
  }

  .table-wrap {
    overflow-x: auto;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    text-align: left;
    vertical-align: middle;
  }

  th {
    color: var(--text-secondary);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
  }

  .actions {
    text-align: right;
  }

  .muted {
    color: var(--text-secondary);
  }

  .narrowing {
    margin-top: 4px;
    color: var(--text-secondary);
    font-size: 12px;
  }

  @media (max-width: 760px) {
    .create-form {
      grid-template-columns: 1fr;
    }

    .token-created {
      grid-template-columns: 1fr;
    }
  }
</style>
