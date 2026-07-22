<script lang="ts">
  import { goto } from '$app/navigation';
  import { tokens } from '$lib/api/client.svelte';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';

  interface AccessToken {
    id: number;
    name: string;
    scopes: string;
    expires_at?: string | null;
    last_used_at?: string | null;
    created_at: string;
  }

  let tokenList = $state<AccessToken[]>([]);
  let loading = $state(true);
  let creating = $state(false);
  let deletingId = $state<number | null>(null);
  let error = $state('');
  let success = $state('');
  let newToken = $state('');
  let name = $state('');
  let scopes = $state('repo');
  let expiresAt = $state('');

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadTokens();
  });

  async function loadTokens() {
    try {
      loading = true;
      error = '';
      tokenList = await tokens.list();
    } catch (err: any) {
      error = err.message || 'Failed to load access tokens';
    } finally {
      loading = false;
    }
  }

  function expiresAtIso() {
    if (!expiresAt) return undefined;
    const parsed = new Date(`${expiresAt}T23:59:59`);
    return Number.isNaN(parsed.getTime()) ? undefined : parsed.toISOString();
  }

  async function createToken(event: SubmitEvent) {
    event.preventDefault();
    if (!name.trim()) {
      error = 'Token name is required';
      return;
    }

    try {
      creating = true;
      error = '';
      success = '';
      newToken = '';
      const created = await tokens.create(name.trim(), scopes.trim() || 'repo', expiresAtIso());
      newToken = created.token;
      success = 'Token created. Copy it now; it will not be shown again.';
      name = '';
      scopes = 'repo';
      expiresAt = '';
      await loadTokens();
    } catch (err: any) {
      error = err.message || 'Failed to create token';
    } finally {
      creating = false;
    }
  }

  async function revokeToken(token: AccessToken) {
    if (!confirm(`Revoke token "${token.name}"?`)) return;

    try {
      deletingId = token.id;
      error = '';
      success = '';
      await tokens.delete(token.id);
      success = 'Token revoked';
      await loadTokens();
    } catch (err: any) {
      error = err.message || 'Failed to revoke token';
    } finally {
      deletingId = null;
    }
  }

  async function copyNewToken() {
    if (!newToken) return;
    await navigator.clipboard.writeText(newToken);
    success = 'Token copied';
  }

  function formatDate(value?: string | null) {
    if (!value) return 'Never';
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return date.toLocaleDateString();
  }
</script>

<svelte:head>
  <title>Access Tokens · IronForge</title>
</svelte:head>

<div class="page-container tokens-page">
  <header class="page-header">
    <div>
      <h1>Access Tokens</h1>
      <p>Manage personal tokens for Git over HTTP, API clients, and automation.</p>
    </div>
  </header>

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if newToken}
    <section class="token-created" aria-label="New access token">
      <div>
        <strong>New token</strong>
        <p>Copy this value before leaving the page.</p>
      </div>
      <code>{newToken}</code>
      <button type="button" class="btn btn-primary" onclick={copyNewToken}>Copy</button>
    </section>
  {/if}

  <section class="section">
    <h2>Create Token</h2>
    <form class="create-form" onsubmit={createToken}>
      <label>
        Name
        <input bind:value={name} placeholder="CI deploy token" disabled={creating} />
      </label>
      <label>
        Scopes
        <input bind:value={scopes} placeholder="repo" disabled={creating} />
      </label>
      <label>
        Expires
        <input type="date" bind:value={expiresAt} disabled={creating} />
      </label>
      <button type="submit" class="btn btn-primary" disabled={creating || !name.trim()}>
        {creating ? 'Creating...' : 'Create'}
      </button>
    </form>
  </section>

  <section class="section">
    <h2>Existing Tokens</h2>

    {#if loading}
      <p class="muted">Loading...</p>
    {:else if tokenList.length === 0}
      <div class="empty-state">No personal access tokens yet.</div>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Name</th>
              <th>Scopes</th>
              <th>Created</th>
              <th>Last used</th>
              <th>Expires</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each tokenList as token (token.id)}
              <tr>
                <td>{token.name}</td>
                <td><code>{token.scopes}</code></td>
                <td>{formatDate(token.created_at)}</td>
                <td>{formatDate(token.last_used_at)}</td>
                <td>{formatDate(token.expires_at)}</td>
                <td class="actions">
                  <button
                    type="button"
                    class="btn btn-danger"
                    disabled={deletingId === token.id}
                    onclick={() => revokeToken(token)}
                  >
                    {deletingId === token.id ? 'Revoking...' : 'Revoke'}
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

  @media (max-width: 760px) {
    .create-form {
      grid-template-columns: 1fr;
    }

    .token-created {
      grid-template-columns: 1fr;
    }
  }
</style>
