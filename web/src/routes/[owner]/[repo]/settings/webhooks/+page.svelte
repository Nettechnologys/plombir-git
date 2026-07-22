<script lang="ts">
  import { page } from '$app/stores';
  import { createT } from '$lib/i18n';
  import { webhooks, type RepositoryWebhook } from '$lib/api/client.svelte';

  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  const eventOptions = [
    'push',
    'issue.opened',
    'issue.closed',
    'issue.comment',
    'pull_request.opened',
    'pull_request.closed',
    'pull_request.merged',
    'release.created',
    'release.deleted',
    'branch.created',
    'branch.deleted',
    'tag.created',
    'tag.deleted',
    'milestone.closed',
  ];

  let hooks = $state<RepositoryWebhook[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let deletingId = $state<number | null>(null);
  let error = $state('');
  let success = $state('');
  let url = $state('');
  let secret = $state('');
  let contentType = $state<'json' | 'form'>('json');
  let active = $state(true);
  let selectedEvents = $state<string[]>(['push']);

  $effect(() => {
    loadWebhooks();
  });

  async function loadWebhooks() {
    try {
      loading = true;
      error = '';
      hooks = await webhooks.list(owner, repo);
    } catch (err: any) {
      error = err.message || t('settings.webhooks.load_failed', 'Failed to load webhooks');
    } finally {
      loading = false;
    }
  }

  function toggleEvent(event: string, checked: boolean) {
    selectedEvents = checked
      ? Array.from(new Set([...selectedEvents, event]))
      : selectedEvents.filter((item) => item !== event);
  }

  async function createWebhook(event: SubmitEvent) {
    event.preventDefault();
    if (!url.trim()) {
      error = t('settings.webhooks.url_required', 'Enter a payload URL.');
      return;
    }
    if (selectedEvents.length === 0) {
      error = t('settings.webhooks.events_required', 'Select at least one event.');
      return;
    }

    try {
      saving = true;
      error = '';
      success = '';
      await webhooks.create(owner, repo, {
        url: url.trim(),
        content_type: contentType,
        secret: secret.trim() || undefined,
        active,
        events: selectedEvents,
      });
      url = '';
      secret = '';
      contentType = 'json';
      active = true;
      selectedEvents = ['push'];
      success = t('settings.webhooks.created', 'Webhook created.');
      await loadWebhooks();
    } catch (err: any) {
      error = err.message || t('settings.webhooks.create_failed', 'Failed to create webhook');
    } finally {
      saving = false;
    }
  }

  async function setActive(hook: RepositoryWebhook, nextActive: boolean) {
    try {
      error = '';
      success = '';
      const updated = await webhooks.update(owner, repo, hook.id, { active: nextActive });
      hooks = hooks.map((item) => item.id === hook.id ? updated : item);
      success = t('settings.webhooks.updated', 'Webhook updated.');
    } catch (err: any) {
      error = err.message || t('settings.webhooks.update_failed', 'Failed to update webhook');
    }
  }

  async function removeWebhook(hook: RepositoryWebhook) {
    if (!confirm(t('settings.webhooks.delete_confirm', { url: hook.url }))) return;

    try {
      deletingId = hook.id;
      error = '';
      success = '';
      await webhooks.remove(owner, repo, hook.id);
      hooks = hooks.filter((item) => item.id !== hook.id);
      success = t('settings.webhooks.deleted', 'Webhook deleted.');
    } catch (err: any) {
      error = err.message || t('settings.webhooks.delete_failed', 'Failed to delete webhook');
    } finally {
      deletingId = null;
    }
  }

  function eventList(hook: RepositoryWebhook) {
    return hook.events.split(',').map((event) => event.trim()).filter(Boolean).join(', ');
  }
</script>

<div class="webhooks-page">
  <div class="page-header">
    <div>
      <h1>{t('settings.webhooks.title', 'Webhooks')}</h1>
      <p>{t('settings.webhooks.desc', 'Send repository events to external services over HTTP.')}</p>
    </div>
  </div>

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  <section class="section">
    <h2>{t('settings.webhooks.create_title', 'Add webhook')}</h2>
    <form class="webhook-form" onsubmit={createWebhook}>
      <div class="form-group">
        <label for="webhook-url">{t('settings.webhooks.url', 'Payload URL')}</label>
        <input id="webhook-url" type="url" bind:value={url} placeholder="https://example.com/webhook" disabled={saving} />
      </div>

      <div class="form-row">
        <div class="form-group">
          <label for="webhook-content-type">{t('settings.webhooks.content_type', 'Content type')}</label>
          <select id="webhook-content-type" bind:value={contentType} disabled={saving}>
            <option value="json">application/json</option>
            <option value="form">application/x-www-form-urlencoded</option>
          </select>
        </div>

        <div class="form-group">
          <label for="webhook-secret">{t('settings.webhooks.secret', 'Secret')}</label>
          <input id="webhook-secret" type="password" bind:value={secret} autocomplete="new-password" disabled={saving} />
        </div>
      </div>

      <fieldset class="event-grid">
        <legend>{t('settings.webhooks.events', 'Events')}</legend>
        {#each eventOptions as event}
          <label>
            <input
              type="checkbox"
              checked={selectedEvents.includes(event)}
              disabled={saving}
              onchange={(e) => toggleEvent(event, e.currentTarget.checked)}
            />
            <span>{event}</span>
          </label>
        {/each}
      </fieldset>

      <label class="checkbox-row">
        <input type="checkbox" bind:checked={active} disabled={saving} />
        <span>{t('settings.webhooks.active', 'Active')}</span>
      </label>

      <div class="actions">
        <button class="btn btn-primary" type="submit" disabled={saving || !url.trim() || selectedEvents.length === 0}>
          {saving ? t('common.loading') : t('settings.webhooks.create', 'Add webhook')}
        </button>
      </div>
    </form>
  </section>

  <section class="section">
    <h2>{t('settings.webhooks.current', 'Configured webhooks')}</h2>
    {#if loading}
      <div class="loading">{t('common.loading')}</div>
    {:else if hooks.length === 0}
      <div class="empty-state">{t('settings.webhooks.empty', 'No webhooks configured yet.')}</div>
    {:else}
      <div class="hook-list">
        {#each hooks as hook}
          <article class="hook-item">
            <div class="hook-main">
              <div class="hook-url">{hook.url}</div>
              <div class="hook-meta">
                <span>{hook.content_type}</span>
                <span>{eventList(hook)}</span>
              </div>
            </div>
            <div class="hook-actions">
              <label class="checkbox-row compact">
                <input type="checkbox" checked={hook.active} onchange={(e) => setActive(hook, e.currentTarget.checked)} />
                <span>{hook.active ? t('settings.webhooks.enabled', 'Enabled') : t('settings.webhooks.disabled', 'Disabled')}</span>
              </label>
              <button class="btn btn-danger" type="button" onclick={() => removeWebhook(hook)} disabled={deletingId === hook.id}>
                {deletingId === hook.id ? t('common.loading') : t('common.delete')}
              </button>
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .webhooks-page {
    max-width: 900px;
  }

  .page-header {
    margin-bottom: 2rem;
  }

  h1 {
    font-size: 1.75rem;
    margin: 0 0 0.5rem;
    color: var(--text-primary);
  }

  h2 {
    font-size: 1.1rem;
    margin: 0 0 1rem;
    color: var(--text-primary);
  }

  p {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }

  .section {
    margin-bottom: 2.5rem;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
  }

  .webhook-form {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .form-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 1rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  label,
  legend {
    color: var(--text-primary);
    font-size: 0.9rem;
    font-weight: 500;
  }

  input,
  select {
    padding: 0.6rem 0.75rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
  }

  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
  }

  .event-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 0.75rem;
    margin: 0;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
  }

  .event-grid label,
  .checkbox-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--text-primary);
    font-weight: 400;
  }

  .checkbox-row.compact {
    white-space: nowrap;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
  }

  .btn {
    padding: 0.6rem 1.25rem;
    border: none;
    border-radius: 6px;
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-primary {
    background: var(--accent);
    color: white;
  }

  .btn-danger {
    background: var(--red, #ff4444);
    color: white;
  }

  .hook-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .hook-item {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-secondary);
  }

  .hook-main {
    min-width: 0;
  }

  .hook-url {
    overflow-wrap: anywhere;
    color: var(--text-primary);
    font-weight: 600;
  }

  .hook-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-top: 0.4rem;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .hook-actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-shrink: 0;
  }

  .empty-state,
  .loading {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
    background: var(--bg-secondary);
    border-radius: 6px;
  }

  .error-box,
  .success-box {
    padding: 0.75rem;
    border-radius: 6px;
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }

  .error-box {
    background: rgba(255, 0, 0, 0.1);
    border: 1px solid var(--red, #ff4444);
    color: var(--red, #ff4444);
  }

  .success-box {
    background: rgba(0, 255, 0, 0.1);
    border: 1px solid var(--green, #28a745);
    color: var(--green, #28a745);
  }

  @media (max-width: 720px) {
    .form-row,
    .hook-item,
    .hook-actions {
      display: flex;
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
