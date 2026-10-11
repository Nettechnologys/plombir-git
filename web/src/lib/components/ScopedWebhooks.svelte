<script lang="ts">
  import { organizationWebhooks, systemWebhooks, type RepositoryWebhook, type WebhookDelivery, type WebhookPayload } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';
  import Modal from '$lib/components/Modal.svelte';

  const t = createT();

  let { scope, name = '' }: { scope: 'org' | 'system'; name?: string } = $props();
  const eventOptions = [
    'push', 'branch.created', 'branch.deleted', 'tag.created', 'tag.deleted',
    'release.created', 'release.deleted', 'issue.opened', 'issue.closed',
    'issue.comment', 'issue.edited', 'issue.labeled', 'issue.assigned',
    'milestone.closed', 'pull_request.opened', 'pull_request.closed',
    'pull_request.merged', 'pull_request.synchronize',
    'pull_request.review_submitted', 'pull_request.review_requested',
    'ci.status', 'wiki.created', 'wiki.updated', 'wiki.deleted',
  ];

  let hooks = $state<RepositoryWebhook[]>([]);
  let deliveries = $state<WebhookDelivery[]>([]);
  let selectedHook = $state<number | null>(null);
  let drafts = $state<Record<number, string[]>>({});
  let url = $state('');
  let secret = $state('');
  let contentType = $state<WebhookPayload['content_type']>('json');
  let selectedEvents = $state<string[]>(['push']);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let success = $state('');
  let deleteTarget = $state<RepositoryWebhook | null>(null);
  let generation = 0;

  $effect(() => {
    const expectedScope = scope;
    const expectedName = name;
    const current = ++generation;
    hooks = [];
    deliveries = [];
    selectedHook = null;
    deleteTarget = null;
    loading = true;
    void load(expectedScope, expectedName, current);
  });

  async function load(expectedScope: 'org' | 'system', expectedName: string, current: number) {
    try {
      const next = expectedScope === 'org'
        ? await organizationWebhooks.list(expectedName)
        : await systemWebhooks.list();
      if (generation !== current) return;
      hooks = next;
      drafts = Object.fromEntries(next.map((hook) => [hook.id, hook.events.split(',').filter(Boolean)]));
      error = '';
    } catch (cause) {
      if (generation === current) error = message(cause);
    } finally {
      if (generation === current) loading = false;
    }
  }

  function message(cause: unknown): string {
    return cause instanceof Error ? cause.message : t('errors.load_failed');
  }

  function toggle(events: string[], event: string, checked: boolean): string[] {
    return checked ? Array.from(new Set([...events, event])) : events.filter((value) => value !== event);
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (!url.trim() || selectedEvents.length === 0) {
      error = !url.trim() ? t('settings.webhooks.url_required') : t('settings.webhooks.events_required');
      return;
    }
    if (busy) return;
    busy = true;
    error = '';
    success = '';
    const current = generation;
    const expectedScope = scope;
    const expectedName = name;
    const payload: WebhookPayload = {
      url: url.trim(), secret: secret || undefined, content_type: contentType,
      active: true, events: [...selectedEvents],
    };
    try {
      if (expectedScope === 'org') await organizationWebhooks.create(expectedName, payload);
      else await systemWebhooks.create(payload);
      if (generation !== current) return;
      url = '';
      secret = '';
      success = t('settings.webhooks.created');
      await load(expectedScope, expectedName, current);
    } catch (cause) {
      if (generation === current) error = message(cause);
    } finally {
      if (generation === current) busy = false;
    }
  }

  async function update(hook: RepositoryWebhook, payload: Partial<WebhookPayload>) {
    if (busy) return;
    busy = true;
    error = '';
    success = '';
    const current = generation;
    const expectedScope = scope;
    const expectedName = name;
    try {
      if (expectedScope === 'org') await organizationWebhooks.update(expectedName, hook.id, payload);
      else await systemWebhooks.update(hook.id, payload);
      if (generation !== current) return;
      success = t('settings.webhooks.updated');
      await load(expectedScope, expectedName, current);
    } catch (cause) {
      if (generation === current) error = message(cause);
    } finally {
      if (generation === current) busy = false;
    }
  }

  async function remove(hook: RepositoryWebhook) {
    if (busy) return;
    busy = true;
    error = '';
    success = '';
    const current = generation;
    const expectedScope = scope;
    const expectedName = name;
    try {
      if (expectedScope === 'org') await organizationWebhooks.remove(expectedName, hook.id);
      else await systemWebhooks.remove(hook.id);
      if (generation !== current) return;
      if (selectedHook === hook.id) { selectedHook = null; deliveries = []; }
      success = t('settings.webhooks.deleted');
      deleteTarget = null;
      await load(expectedScope, expectedName, current);
    } catch (cause) {
      if (generation === current) error = message(cause);
    } finally {
      if (generation === current) busy = false;
    }
  }

  async function showDeliveries(id: number) {
    const current = generation;
    selectedHook = id;
    deliveries = [];
    try {
      const next = scope === 'org'
        ? await organizationWebhooks.deliveries(name, id)
        : await systemWebhooks.deliveries(id);
      if (generation === current && selectedHook === id) deliveries = next;
    } catch (cause) {
      if (generation === current) error = message(cause);
    }
  }

  async function redeliver(hookId: number, deliveryId: number) {
    if (busy) return;
    busy = true;
    const current = generation;
    try {
      if (scope === 'org') await organizationWebhooks.redeliver(name, hookId, deliveryId);
      else await systemWebhooks.redeliver(hookId, deliveryId);
      if (generation === current) {
        success = t('settings.webhooks.redelivery_triggered');
        await showDeliveries(hookId);
      }
    } catch (cause) {
      if (generation === current) error = message(cause);
    } finally {
      if (generation === current) busy = false;
    }
  }
</script>

<section class="scoped-webhooks">
  <h1>{scope === 'org' ? t('settings.webhooks.org_title', { name }) : t('settings.webhooks.system_title')}</h1>
  <p>{scope === 'org' ? t('settings.webhooks.org_desc', { name }) : t('settings.webhooks.system_desc')}</p>
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if success}<p role="status">{success}</p>{/if}

  <form onsubmit={create}>
    <h2>{t('settings.webhooks.create_title')}</h2>
    <label>{t('settings.webhooks.url')} <input type="url" bind:value={url} required disabled={busy} /></label>
    <label>{t('settings.webhooks.content_type')}
      <select bind:value={contentType} disabled={busy}>
        <option value="json">{t('settings.webhooks.format_json')}</option><option value="form">{t('settings.webhooks.format_form')}</option>
        <option value="slack">{t('settings.webhooks.format_slack')}</option><option value="discord">{t('settings.webhooks.format_discord')}</option>
        <option value="matrix">{t('settings.webhooks.format_matrix')}</option>
      </select>
    </label>
    <label>{t('settings.webhooks.signing_secret')} <input type="password" bind:value={secret} autocomplete="new-password" disabled={busy} /></label>
    <fieldset><legend>{t('settings.webhooks.events')}</legend>
      {#each eventOptions as event}
        <label><input type="checkbox" checked={selectedEvents.includes(event)} disabled={busy}
          onchange={(input) => selectedEvents = toggle(selectedEvents, event, input.currentTarget.checked)} />{event}</label>
      {/each}
    </fieldset>
    <button type="submit" disabled={busy}>{t('settings.webhooks.create')}</button>
  </form>

  <h2>{t('settings.webhooks.current')}</h2>
  {#if loading}<p>{t('common.loading')}</p>{:else if hooks.length === 0}<p>{t('settings.webhooks.empty')}</p>{/if}
  {#each hooks as hook (hook.id)}
    <article>
      <h3>{hook.url}</h3>
      <p>{hook.content_type} · {hook.active ? t('settings.webhooks.enabled') : t('settings.webhooks.disabled')}</p>
      <label><input type="checkbox" checked={hook.active} disabled={busy}
        onchange={(input) => void update(hook, { active: input.currentTarget.checked })} />{t('settings.webhooks.active')}</label>
      <fieldset><legend>{t('settings.webhooks.events_for_hook')}</legend>
        {#each eventOptions as event}
          <label><input type="checkbox" checked={(drafts[hook.id] ?? []).includes(event)} disabled={busy}
            onchange={(input) => drafts = { ...drafts, [hook.id]: toggle(drafts[hook.id] ?? [], event, input.currentTarget.checked) } } />{event}</label>
        {/each}
      </fieldset>
      <button type="button" disabled={busy} onclick={() => void update(hook, { events: drafts[hook.id] ?? [] })}>{t('settings.webhooks.save_events')}</button>
      <button type="button" disabled={busy} onclick={() => void showDeliveries(hook.id)}>{t('settings.webhooks.view_deliveries')}</button>
      <button type="button" disabled={busy} onclick={() => deleteTarget = hook}>{t('common.delete')}</button>
      {#if selectedHook === hook.id}
        <ul>
          {#each deliveries as delivery (delivery.id)}
            <li>{delivery.event} · {delivery.response_status ?? delivery.response_body ?? 'pending'}
              <button type="button" disabled={busy} onclick={() => void redeliver(hook.id, delivery.id)}>{t('settings.webhooks.redeliver')}</button>
            </li>
          {/each}
        </ul>
      {/if}
    </article>
  {/each}
</section>

{#if deleteTarget}
  <Modal onclose={() => deleteTarget = null} labelledby="scoped-webhook-delete-title" width="420px">
    <h2 id="scoped-webhook-delete-title">{t('settings.webhooks.delete_confirm_title')}</h2>
    <p>{t('settings.webhooks.delete_confirm', { url: deleteTarget.url })}</p>
    <button type="button" disabled={busy} onclick={() => deleteTarget && void remove(deleteTarget)}>{t('common.delete')}</button>
    <button type="button" disabled={busy} onclick={() => deleteTarget = null}>{t('common.cancel')}</button>
  </Modal>
{/if}

<style>
  .scoped-webhooks { max-width: 70rem; margin: 2rem auto; padding: 0 1rem; }
  form, article { border: 1px solid var(--border-color, #888); border-radius: .5rem; padding: 1rem; margin: 1rem 0; }
  form > label { display: block; margin: .75rem 0; }
  input[type="url"] { width: min(100%, 35rem); }
  fieldset { display: flex; flex-wrap: wrap; gap: .5rem 1rem; margin: 1rem 0; }
  fieldset label { white-space: nowrap; }
  button { margin: .25rem; }
  .error { color: #b00; }
</style>
