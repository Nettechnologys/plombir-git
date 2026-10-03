<script lang="ts">
  import { goto } from '$app/navigation';
  import { bots, splitList, type Bot, type BotToken } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';

  let botList = $state<Bot[]>([]);
  let loading = $state(true);
  let error = $state('');
  let success = $state('');

  let botName = $state('');
  let botDisplayName = $state('');
  let creatingBot = $state(false);
  let busyBots = $state<Set<string>>(new Set());

  // The bot whose tokens are open, and what is known about them.
  let selected = $state<string | null>(null);
  let tokenList = $state<BotToken[]>([]);
  let tokensLoading = $state(false);
  let busyTokenIds = $state<Set<number>>(new Set());

  let tokenName = $state('');
  let tokenExpires = $state('');
  let tokenRepositories = $state('');
  let tokenTools = $state('');
  let denyProtectedMerge = $state(true);
  let mintingToken = $state(false);
  let newToken = $state('');

  const botRequests = new LatestRequestFence<'bots'>();
  const tokenRequests = new LatestRequestFence<string>();

  const mcpEndpoint = typeof window === 'undefined' ? '/api/v1/mcp' : `${window.location.origin}/api/v1/mcp`;

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadBots();
  });

  async function loadBots() {
    const claim = botRequests.begin('bots');
    try {
      loading = true;
      error = '';
      const next = await bots.list();
      if (botRequests.owns(claim, 'bots')) botList = next;
    } catch (err: any) {
      if (botRequests.owns(claim, 'bots')) error = err.message || 'Failed to load agents';
    } finally {
      if (botRequests.owns(claim, 'bots')) loading = false;
    }
  }

  // A late answer for a bot that is no longer open must not land in the panel
  // of the one that is.
  const ownsTokens = (claim: ReturnType<typeof tokenRequests.begin>) =>
    tokenRequests.owns(claim, selected ?? '');

  async function loadTokens(username: string) {
    const claim = tokenRequests.begin(username);
    try {
      tokensLoading = true;
      const next = await bots.listTokens(username);
      if (ownsTokens(claim)) tokenList = next;
    } catch (err: any) {
      if (ownsTokens(claim)) error = err.message || 'Failed to load tokens';
    } finally {
      if (ownsTokens(claim)) tokensLoading = false;
    }
  }

  async function createBot(event: SubmitEvent) {
    event.preventDefault();
    if (creatingBot) return;
    if (!botName.trim()) {
      error = 'An agent needs a username';
      return;
    }
    try {
      creatingBot = true;
      error = '';
      success = '';
      const created = await bots.create(botName.trim(), botDisplayName.trim() || undefined);
      success = `Agent @${created.username} created. Add it as a collaborator on the repositories it should work on.`;
      botName = '';
      botDisplayName = '';
      await loadBots();
    } catch (err: any) {
      error = err.message || 'Failed to create agent';
    } finally {
      creatingBot = false;
    }
  }

  async function deleteBot(bot: Bot) {
    if (!confirm(`Delete agent @${bot.username}? Its tokens stop working and its repositories are deleted.`)) return;
    const username = bot.username;
    if (busyBots.has(username)) return;
    busyBots = new Set(busyBots).add(username);
    try {
      error = '';
      success = '';
      await bots.delete(username);
      if (selected === username) closeTokens();
      success = `Agent @${username} deleted`;
      await loadBots();
    } catch (err: any) {
      error = err.message || 'Failed to delete agent';
    } finally {
      const next = new Set(busyBots);
      next.delete(username);
      busyBots = next;
    }
  }

  function closeTokens() {
    selected = null;
    tokenList = [];
    newToken = '';
  }

  async function openTokens(bot: Bot) {
    if (selected === bot.username) {
      closeTokens();
      return;
    }
    selected = bot.username;
    tokenList = [];
    newToken = '';
    await loadTokens(bot.username);
  }

  function expiresAtIso() {
    if (!tokenExpires) return undefined;
    const parsed = new Date(`${tokenExpires}T23:59:59`);
    return Number.isNaN(parsed.getTime()) ? undefined : parsed.toISOString();
  }

  async function mintToken(event: SubmitEvent) {
    event.preventDefault();
    const username = selected;
    if (!username || mintingToken) return;
    if (!tokenName.trim()) {
      error = 'Token name is required';
      return;
    }
    const repositories = splitList(tokenRepositories);
    const tools = splitList(tokenTools);
    try {
      mintingToken = true;
      error = '';
      success = '';
      newToken = '';
      const created = await bots.createToken(username, tokenName.trim(), expiresAtIso(), {
        repositories: repositories.length ? repositories : undefined,
        mcp_tools: tools.length ? tools : undefined,
        deny_protected_merge: denyProtectedMerge,
      });
      newToken = created.token;
      success = 'Token created. Copy it now; it will not be shown again.';
      tokenName = '';
      tokenExpires = '';
      tokenRepositories = '';
      tokenTools = '';
      denyProtectedMerge = true;
      await loadTokens(username);
    } catch (err: any) {
      error = err.message || 'Failed to create token';
    } finally {
      mintingToken = false;
    }
  }

  async function revokeToken(token: BotToken) {
    const username = selected;
    if (!username) return;
    if (!confirm(`Revoke token "${token.name}" of @${username}?`)) return;
    const tokenId = token.id;
    if (busyTokenIds.has(tokenId)) return;
    busyTokenIds = new Set(busyTokenIds).add(tokenId);
    try {
      error = '';
      success = '';
      await bots.deleteToken(username, tokenId);
      success = 'Token revoked';
      await loadTokens(username);
    } catch (err: any) {
      error = err.message || 'Failed to revoke token';
    } finally {
      const next = new Set(busyTokenIds);
      next.delete(tokenId);
      busyTokenIds = next;
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
  <title>Agents · ForgeKeep</title>
</svelte:head>

<div class="page-container agents-page">
  <header class="page-header">
    <h1>Agents</h1>
    <p>
      Give each AI agent a bot account of its own. Its issues, pull requests and commits carry its name and
      yours as its owner; it has no password, and it stops working when you delete it or your account is disabled.
    </p>
  </header>

  <section class="how-to">
    <ol>
      <li>Create an agent below.</li>
      <li>Add it as a collaborator on each repository it should work on — that is its access.</li>
      <li>
        Mint a token for it, optionally confined to repositories, MCP tools, and kept off protected branches,
        then point an MCP client at <code>{mcpEndpoint}</code> with <code>Authorization: Bearer &lt;token&gt;</code>.
      </li>
    </ol>
  </section>

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  <section class="section">
    <h2>Create agent</h2>
    <form class="create-form" onsubmit={createBot}>
      <label>
        Username
        <input bind:value={botName} placeholder="alice-agent" disabled={creatingBot} />
      </label>
      <label>
        Display name
        <input bind:value={botDisplayName} placeholder="Review assistant" disabled={creatingBot} />
      </label>
      <button type="submit" class="btn btn-primary create-bot" disabled={creatingBot || !botName.trim()}>
        {creatingBot ? 'Creating...' : 'Create'}
      </button>
    </form>
  </section>

  <section class="section">
    <h2>Your agents</h2>

    {#if loading}
      <p class="muted">Loading...</p>
    {:else if botList.length === 0}
      <div class="empty-state">No agents yet.</div>
    {:else}
      <ul class="bot-list">
        {#each botList as bot (bot.id)}
          <li class="bot-row">
            <div class="bot-head">
              <div>
                <a href={`/${bot.username}`}><strong>@{bot.username}</strong></a>
                {#if bot.display_name}<span class="muted"> · {bot.display_name}</span>{/if}
                {#if !bot.is_active}<span class="muted"> · disabled</span>{/if}
                <div class="muted small">Created {formatDate(bot.created_at)}</div>
              </div>
              <div class="bot-actions">
                <button type="button" class="btn manage-tokens" onclick={() => openTokens(bot)}>
                  {selected === bot.username ? 'Close' : 'Tokens'}
                </button>
                <button
                  type="button"
                  class="btn btn-danger delete-bot"
                  disabled={busyBots.has(bot.username)}
                  onclick={() => deleteBot(bot)}
                >
                  {busyBots.has(bot.username) ? 'Deleting...' : 'Delete'}
                </button>
              </div>
            </div>

            {#if selected === bot.username}
              <div class="token-panel">
                {#if newToken}
                  <section class="token-created" aria-label="New agent token">
                    <div>
                      <strong>New token</strong>
                      <p>Copy this value before leaving the page.</p>
                    </div>
                    <code>{newToken}</code>
                    <button type="button" class="btn btn-primary copy-token" onclick={copyNewToken}>Copy</button>
                  </section>
                {/if}

                <form class="token-form" onsubmit={mintToken}>
                  <label>
                    Token name
                    <input bind:value={tokenName} placeholder="claude-code" disabled={mintingToken} />
                  </label>
                  <label>
                    Expires
                    <input type="date" bind:value={tokenExpires} disabled={mintingToken} />
                  </label>
                  <label class="wide">
                    Repositories (owner/name, one per line or comma-separated; empty = every repository the agent can reach)
                    <textarea bind:value={tokenRepositories} rows="2" placeholder="alice/app" disabled={mintingToken}></textarea>
                  </label>
                  <label class="wide">
                    MCP tools (comma-separated; empty = an ordinary token, set = usable only through the MCP endpoint)
                    <input bind:value={tokenTools} placeholder="get_issue, create_issue, create_pr" disabled={mintingToken} />
                  </label>
                  <label class="checkbox wide">
                    <input type="checkbox" bind:checked={denyProtectedMerge} disabled={mintingToken} />
                    Keep off protected branches (no merge, push or commit to them)
                  </label>
                  <button type="submit" class="btn btn-primary mint-token" disabled={mintingToken || !tokenName.trim()}>
                    {mintingToken ? 'Creating...' : 'Create token'}
                  </button>
                </form>

                {#if tokensLoading}
                  <p class="muted">Loading...</p>
                {:else if tokenList.length === 0}
                  <div class="empty-state">No tokens for @{bot.username} yet.</div>
                {:else}
                  <div class="table-wrap">
                    <table>
                      <thead>
                        <tr>
                          <th>Name</th>
                          <th>Confined to</th>
                          <th>Last used</th>
                          <th>Expires</th>
                          <th></th>
                        </tr>
                      </thead>
                      <tbody>
                        {#each tokenList as token (token.id)}
                          <tr>
                            <td>{token.name}</td>
                            <td class="narrowing">
                              <div>Repositories: {token.repositories ? token.repositories.join(', ') || 'none' : 'any'}</div>
                              <div>MCP tools: {token.mcp_tools ? token.mcp_tools.join(', ') : 'any (REST allowed)'}</div>
                              <div>{token.deny_protected_merge ? 'Kept off protected branches' : 'May write to protected branches'}</div>
                            </td>
                            <td>{formatDate(token.last_used_at)}</td>
                            <td>{formatDate(token.expires_at)}</td>
                            <td class="actions">
                              <button
                                type="button"
                                class="btn btn-danger revoke-token"
                                disabled={busyTokenIds.has(token.id)}
                                onclick={() => revokeToken(token)}
                              >
                                {busyTokenIds.has(token.id) ? 'Revoking...' : 'Revoke'}
                              </button>
                            </td>
                          </tr>
                        {/each}
                      </tbody>
                    </table>
                  </div>
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .agents-page {
    max-width: 980px;
  }

  .page-header {
    margin-bottom: 16px;
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

  .how-to {
    margin-bottom: 24px;
    color: var(--text-secondary);
  }

  .how-to ol {
    margin: 0;
    padding-left: 20px;
  }

  .section {
    margin-bottom: 32px;
    padding-bottom: 28px;
    border-bottom: 1px solid var(--border);
  }

  .create-form {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) minmax(180px, 1fr) auto;
    align-items: end;
    gap: 12px;
  }

  .token-form {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) minmax(150px, 0.6fr);
    gap: 12px;
    margin-bottom: 16px;
  }

  .token-form .wide {
    grid-column: 1 / -1;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--text-secondary);
    font-size: 13px;
    font-weight: 600;
  }

  label.checkbox {
    flex-direction: row;
    align-items: center;
    font-weight: 500;
  }

  input:not([type='checkbox']),
  textarea {
    min-height: 36px;
    padding: 7px 10px;
  }

  .bot-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .bot-row {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px 16px;
    margin-bottom: 12px;
  }

  .bot-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }

  .bot-actions {
    display: flex;
    gap: 8px;
  }

  .token-panel {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
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

  .narrowing {
    font-size: 12px;
    color: var(--text-secondary);
  }

  .actions {
    text-align: right;
  }

  .muted {
    color: var(--text-secondary);
  }

  .small {
    font-size: 12px;
  }

  @media (max-width: 760px) {
    .create-form,
    .token-form,
    .token-created {
      grid-template-columns: 1fr;
    }

    .bot-head {
      flex-direction: column;
      align-items: flex-start;
    }
  }
</style>
