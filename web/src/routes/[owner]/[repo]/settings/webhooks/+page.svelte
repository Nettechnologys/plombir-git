<script lang="ts">
  import { page } from '$app/stores';
  import { createT, formatDateTime } from '$lib/i18n';
  import { webhooks, type RepositoryWebhook, type WebhookDelivery } from '$lib/api/client.svelte';
  import {
    reloadDeliveriesAfterRedelivery,
    webhookDeliveryOutcome,
  } from '$lib/api/webhookDelivery';

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
  let selectedHook = $state<RepositoryWebhook | null>(null);
  let deliveries = $state<WebhookDelivery[]>([]);
  let deliveriesLoading = $state(false);
  let redeliveringId = $state<number | null>(null);
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
      if (selectedHook?.id === hook.id) selectedHook = updated;
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
      if (selectedHook?.id === hook.id) closeDeliveries();
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

  function closeDeliveries() {
    selectedHook = null;
    deliveries = [];
  }

  async function openDeliveries(hook: RepositoryWebhook) {
    if (deliveriesLoading || redeliveringId !== null) return;

    selectedHook = hook;
    deliveries = [];
    deliveriesLoading = true;
    error = '';
    success = '';
    try {
      const [freshHook, history] = await Promise.all([
        webhooks.get(owner, repo, hook.id),
        webhooks.deliveries(owner, repo, hook.id),
      ]);
      selectedHook = freshHook;
      hooks = hooks.map((item) => item.id === freshHook.id ? freshHook : item);
      deliveries = history;
    } catch (err: any) {
      error = err.message || t('settings.webhooks.deliveries_load_failed');
    } finally {
      deliveriesLoading = false;
    }
  }

  async function refreshDeliveries() {
    if (!selectedHook || deliveriesLoading || redeliveringId !== null) return;

    deliveriesLoading = true;
    error = '';
    try {
      deliveries = await webhooks.deliveries(owner, repo, selectedHook.id);
    } catch (err: any) {
      error = err.message || t('settings.webhooks.deliveries_load_failed');
    } finally {
      deliveriesLoading = false;
    }
  }

  async function redeliverDelivery(delivery: WebhookDelivery) {
    if (!selectedHook || redeliveringId !== null) return;

    const hookId = selectedHook.id;
    const previous = deliveries;
    redeliveringId = delivery.id;
    error = '';
    success = '';
    try {
      await webhooks.redeliver(owner, repo, hookId, delivery.id);
      success = t('settings.webhooks.redelivery_triggered');
      try {
        deliveries = await reloadDeliveriesAfterRedelivery(
          () => webhooks.deliveries(owner, repo, hookId),
          previous,
          delivery,
        );
      } catch (refreshError: any) {
        error = t('settings.webhooks.redelivery_refresh_failed', {
          message: refreshError.message || t('settings.webhooks.deliveries_load_failed'),
        });
      }
    } catch (err: any) {
      error = err.message || t('settings.webhooks.redelivery_failed');
    } finally {
      redeliveringId = null;
    }
  }

  function deliveryStatus(delivery: WebhookDelivery): string {
    const outcome = webhookDeliveryOutcome(delivery);
    if (outcome === 'pending') return t('settings.webhooks.delivery_pending');
    if (delivery.response_status === null) return t('settings.webhooks.delivery_failed');
    return `HTTP ${delivery.response_status}`;
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
              <button
                class="btn btn-secondary"
                type="button"
                aria-expanded={selectedHook?.id === hook.id}
                onclick={() => selectedHook?.id === hook.id ? closeDeliveries() : openDeliveries(hook)}
                disabled={deliveriesLoading || redeliveringId !== null}
              >
                {selectedHook?.id === hook.id
                  ? t('settings.webhooks.hide_deliveries')
                  : t('settings.webhooks.view_deliveries')}
              </button>
              <button class="btn btn-danger" type="button" onclick={() => removeWebhook(hook)} disabled={deletingId === hook.id}>
                {deletingId === hook.id ? t('common.loading') : t('common.delete')}
              </button>
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>

  {#if selectedHook}
    <section class="section delivery-section" aria-labelledby="webhook-deliveries-title">
      <div class="delivery-section-header">
        <div>
          <h2 id="webhook-deliveries-title">{t('settings.webhooks.deliveries_title')}</h2>
          <p class="selected-hook-url">{selectedHook.url}</p>
        </div>
        <div class="actions">
          <button
            class="btn btn-secondary"
            type="button"
            onclick={refreshDeliveries}
            disabled={deliveriesLoading || redeliveringId !== null}
          >
            {deliveriesLoading ? t('common.loading') : t('settings.webhooks.refresh_deliveries')}
          </button>
          <button class="btn btn-secondary" type="button" onclick={closeDeliveries} disabled={redeliveringId !== null}>
            {t('common.close')}
          </button>
        </div>
      </div>

      {#if deliveriesLoading && deliveries.length === 0}
        <div class="loading">{t('common.loading')}</div>
      {:else if deliveries.length === 0}
        <div class="empty-state">{t('settings.webhooks.deliveries_empty')}</div>
      {:else}
        <div class="delivery-list">
          {#each deliveries as delivery (delivery.id)}
            <article class="delivery-item">
              <div class="delivery-header">
                <div class="delivery-heading">
                  <span
                    class="delivery-status"
                    class:success={webhookDeliveryOutcome(delivery) === 'success'}
                    class:failure={webhookDeliveryOutcome(delivery) === 'failure'}
                    class:pending={webhookDeliveryOutcome(delivery) === 'pending'}
                  >
                    {deliveryStatus(delivery)}
                  </span>
                  <strong>{delivery.event}</strong>
                  <span>{formatDateTime(delivery.created_at)}</span>
                </div>
                <button
                  class="btn btn-secondary"
                  type="button"
                  onclick={() => redeliverDelivery(delivery)}
                  disabled={redeliveringId !== null || deliveriesLoading}
                >
                  {redeliveringId === delivery.id
                    ? t('settings.webhooks.redelivering')
                    : t('settings.webhooks.redeliver')}
                </button>
              </div>

              <div class="delivery-meta">
                <span>
                  {t('settings.webhooks.delivery_id')}:
                  <code>{delivery.delivery_id}</code>
                </span>
                <span>
                  {t('settings.webhooks.delivery_duration')}:
                  {delivery.duration_ms === null ? '—' : `${delivery.duration_ms} ms`}
                </span>
              </div>

              <details>
                <summary>{t('settings.webhooks.request_payload')}</summary>
                <pre>{delivery.request_payload ?? t('settings.webhooks.not_recorded')}</pre>
              </details>
              <details open={webhookDeliveryOutcome(delivery) === 'failure'}>
                <summary>{t('settings.webhooks.response_details')}</summary>
                <pre>{delivery.response_body ?? t('settings.webhooks.not_recorded')}</pre>
              </details>
            </article>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
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

  .btn-secondary {
    background: var(--bg-tertiary, var(--bg-primary));
    border: 1px solid var(--border);
    color: var(--text-primary);
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

  .delivery-section {
    scroll-margin-top: 1rem;
  }

  .delivery-section-header,
  .delivery-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
  }

  .selected-hook-url {
    overflow-wrap: anywhere;
  }

  .delivery-list {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .delivery-item {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-secondary);
  }

  .delivery-heading,
  .delivery-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
  }

  .delivery-heading {
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .delivery-heading strong {
    color: var(--text-primary);
  }

  .delivery-status {
    padding: 0.2rem 0.45rem;
    border-radius: 999px;
    font-weight: 600;
  }

  .delivery-status.success {
    background: rgba(63, 185, 80, 0.14);
    color: var(--green, #28a745);
  }

  .delivery-status.failure {
    background: rgba(248, 81, 73, 0.14);
    color: var(--red, #ff4444);
  }

  .delivery-status.pending {
    background: rgba(210, 153, 34, 0.14);
    color: var(--yellow, #d29922);
  }

  .delivery-meta {
    margin-top: 0.75rem;
    color: var(--text-secondary);
    font-size: 0.8rem;
  }

  .delivery-meta code {
    overflow-wrap: anywhere;
  }

  .delivery-item details {
    margin-top: 0.75rem;
  }

  .delivery-item summary {
    cursor: pointer;
    color: var(--text-primary);
    font-size: 0.85rem;
    font-weight: 600;
  }

  .delivery-item pre {
    max-height: 18rem;
    margin: 0.5rem 0 0;
    padding: 0.75rem;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 0.78rem;
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
    .hook-actions,
    .delivery-section-header,
    .delivery-header {
      display: flex;
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
