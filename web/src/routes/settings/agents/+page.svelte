<script lang="ts">
  import { copyToClipboard } from '$lib/clipboard';
  import { goto } from '$app/navigation';
  import { bots, splitList, type Bot, type BotToken } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import { createT, formatDate as formatLocaleDate } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

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
      if (botRequests.owns(claim, 'bots')) error = err.message || t('agents.load_failed');
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
      if (ownsTokens(claim)) error = err.message || t('agents.tokens_load_failed');
    } finally {
      if (ownsTokens(claim)) tokensLoading = false;
    }
  }

  async function createBot(event: SubmitEvent) {
    event.preventDefault();
    if (creatingBot) return;
    if (!botName.trim()) {
      error = t('agents.username_required');
      return;
    }
    try {
      creatingBot = true;
      error = '';
      success = '';
      const created = await bots.create(botName.trim(), botDisplayName.trim() || undefined);
      success = t('agents.created', { username: created.username });
      botName = '';
      botDisplayName = '';
      await loadBots();
    } catch (err: any) {
      error = err.message || t('agents.create_failed');
    } finally {
      creatingBot = false;
    }
  }

  async function deleteBot(bot: Bot) {
    if (!(await confirmer.ask({
      title: t('agents.delete_confirm_title'),
      message: t('agents.delete_confirm', { username: bot.username }),
      confirmLabel: t('common.delete'),
    }))) return;
    const username = bot.username;
    if (busyBots.has(username)) return;
    busyBots = new Set(busyBots).add(username);
    try {
      error = '';
      success = '';
      await bots.delete(username);
      if (selected === username) closeTokens();
      success = t('agents.deleted', { username });
      await loadBots();
    } catch (err: any) {
      error = err.message || t('agents.delete_failed');
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
      error = t('agents.token_name_required');
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
      success = t('agents.token_created');
      tokenName = '';
      tokenExpires = '';
      tokenRepositories = '';
      tokenTools = '';
      denyProtectedMerge = true;
      await loadTokens(username);
    } catch (err: any) {
      error = err.message || t('agents.token_create_failed');
    } finally {
      mintingToken = false;
    }
  }

  async function revokeToken(token: BotToken) {
    const username = selected;
    if (!username) return;
    if (!(await confirmer.ask({
      title: t('agents.token_revoke_confirm_title'),
      message: t('agents.token_revoke_confirm', { name: token.name, username }),
      confirmLabel: t('agents.revoke'),
    }))) return;
    const tokenId = token.id;
    if (busyTokenIds.has(tokenId)) return;
    busyTokenIds = new Set(busyTokenIds).add(tokenId);
    try {
      error = '';
      success = '';
      await bots.deleteToken(username, tokenId);
      success = t('agents.token_revoked');
      await loadTokens(username);
    } catch (err: any) {
      error = err.message || t('agents.token_revoke_failed');
    } finally {
      const next = new Set(busyTokenIds);
      next.delete(tokenId);
      busyTokenIds = next;
    }
  }

  async function copyNewToken() {
    if (!newToken) return;
    if (await copyToClipboard(newToken)) success = t('agents.token_copied');
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
  <title>{t('agents.title')} · Plombir Git</title>
</svelte:head>

<div class="page-container agents-page">
  <header class="page-header">
    <h1>{t('agents.title')}</h1>
    <p>{t('agents.description')}</p>
  </header>

  <section class="how-to">
    <ol>
      <li>{t('agents.howto.create')}</li>
      <li>{t('agents.howto.collaborator')}</li>
      <li>
        {t('agents.howto.mint_before')} <code>{mcpEndpoint}</code> {t('agents.howto.mint_with')} <code>Authorization: Bearer &lt;token&gt;</code>{t('agents.howto.mint_end')}
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
    <h2>{t('agents.create_title')}</h2>
    <form class="create-form" onsubmit={createBot}>
      <label>
        {t('agents.username')}
        <input bind:value={botName} placeholder="alice-agent" disabled={creatingBot} />
      </label>
      <label>
        {t('agents.display_name')}
        <input bind:value={botDisplayName} placeholder={t('agents.display_name_placeholder')} disabled={creatingBot} />
      </label>
      <button type="submit" class="btn btn-primary create-bot" disabled={creatingBot || !botName.trim()}>
        {creatingBot ? t('agents.creating') : t('common.create')}
      </button>
    </form>
  </section>

  <section class="section">
    <h2>{t('agents.list_title')}</h2>

    {#if loading}
      <p class="muted">{t('common.loading')}</p>
    {:else if botList.length === 0}
      <div class="empty-state">{t('agents.empty')}</div>
    {:else}
      <ul class="bot-list">
        {#each botList as bot (bot.id)}
          <li class="bot-row">
            <div class="bot-head">
              <div>
                <a href={`/${bot.username}`}><strong>@{bot.username}</strong></a>
                {#if bot.display_name}<span class="muted"> · {bot.display_name}</span>{/if}
                {#if !bot.is_active}<span class="muted"> · {t('agents.disabled')}</span>{/if}
                <div class="muted small">{t('common.created', { date: formatDate(bot.created_at) })}</div>
              </div>
              <div class="bot-actions">
                <button type="button" class="btn manage-tokens" onclick={() => openTokens(bot)}>
                  {selected === bot.username ? t('common.close') : t('agents.tokens')}
                </button>
                <button
                  type="button"
                  class="btn btn-danger delete-bot"
                  disabled={busyBots.has(bot.username)}
                  onclick={() => deleteBot(bot)}
                >
                  {busyBots.has(bot.username) ? t('agents.deleting') : t('common.delete')}
                </button>
              </div>
            </div>

            {#if selected === bot.username}
              <div class="token-panel">
                {#if newToken}
                  <section class="token-created" aria-label={t('agents.new_token_label')}>
                    <div>
                      <strong>{t('agents.new_token')}</strong>
                      <p>{t('agents.new_token_hint')}</p>
                    </div>
                    <code>{newToken}</code>
                    <button type="button" class="btn btn-primary copy-token" onclick={copyNewToken}>{t('common.copy')}</button>
                  </section>
                {/if}

                <form class="token-form" onsubmit={mintToken}>
                  <label>
                    {t('agents.token_name')}
                    <input bind:value={tokenName} placeholder="claude-code" disabled={mintingToken} />
                  </label>
                  <label>
                    {t('agents.expires')}
                    <input type="date" bind:value={tokenExpires} disabled={mintingToken} />
                  </label>
                  <label class="wide">
                    {t('agents.repositories_hint')}
                    <textarea bind:value={tokenRepositories} rows="2" placeholder="alice/app" disabled={mintingToken}></textarea>
                  </label>
                  <label class="wide">
                    {t('agents.mcp_tools_hint')}
                    <input bind:value={tokenTools} placeholder="get_issue, create_issue, create_pr" disabled={mintingToken} />
                  </label>
                  <label class="checkbox wide">
                    <input type="checkbox" bind:checked={denyProtectedMerge} disabled={mintingToken} />
                    {t('agents.deny_protected_merge')}
                  </label>
                  <button type="submit" class="btn btn-primary mint-token" disabled={mintingToken || !tokenName.trim()}>
                    {mintingToken ? t('agents.creating') : t('agents.create_token')}
                  </button>
                </form>

                {#if tokensLoading}
                  <p class="muted">{t('common.loading')}</p>
                {:else if tokenList.length === 0}
                  <div class="empty-state">{t('agents.no_tokens', { username: bot.username })}</div>
                {:else}
                  <div class="table-wrap">
                    <table>
                      <thead>
                        <tr>
                          <th>{t('agents.table.name')}</th>
                          <th>{t('agents.table.confined_to')}</th>
                          <th>{t('agents.table.last_used')}</th>
                          <th>{t('agents.expires')}</th>
                          <th></th>
                        </tr>
                      </thead>
                      <tbody>
                        {#each tokenList as token (token.id)}
                          <tr>
                            <td>{token.name}</td>
                            <td class="narrowing">
                              <div>{t('agents.narrowing.repositories', { list: token.repositories ? token.repositories.join(', ') || t('agents.narrowing.none') : t('agents.narrowing.any') })}</div>
                              <div>{t('agents.narrowing.mcp_tools', { list: token.mcp_tools ? token.mcp_tools.join(', ') : t('agents.narrowing.any_rest') })}</div>
                              <div>{token.deny_protected_merge ? t('agents.narrowing.kept_off') : t('agents.narrowing.may_write')}</div>
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
                                {busyTokenIds.has(token.id) ? t('agents.revoking') : t('agents.revoke')}
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

<ConfirmModal {confirmer} />

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
