<script lang="ts">
  // Which notifications are also mailed (card_349c2b6a0d7c). Every mail footer
  // links here. A toggle saves on its own — `PUT` carries only the key that
  // changed, so a second tab's choice for another category is not overwritten —
  // and the switches then show what the server stored, not what was clicked.
  import { goto } from '$app/navigation';
  import {
    EMAIL_NOTIFICATION_KEYS,
    notifications,
    type EmailNotificationKey,
    type EmailNotificationSettings,
  } from '$lib/api/client.svelte';
  import { formatTranslationFallback, t } from '$lib/i18n';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';

  let settings = $state<EmailNotificationSettings | null>(null);
  let loading = $state(true);
  let loadError = $state('');
  let savingKey = $state<EmailNotificationKey | null>(null);
  let saveError = $state('');
  let saved = $state(false);
  let loadGeneration = 0;

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    void load();
  });

  async function load() {
    const generation = ++loadGeneration;
    loading = true;
    loadError = '';
    try {
      const response = await notifications.settings();
      if (generation === loadGeneration) settings = response.email;
    } catch (err: any) {
      if (generation === loadGeneration) loadError = err?.message || t('notification_settings.load_failed');
    } finally {
      if (generation === loadGeneration) loading = false;
    }
  }

  async function toggle(key: EmailNotificationKey, event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const value = input.checked;
    if (!settings || savingKey !== null) {
      input.checked = settings?.[key] ?? !value;
      return;
    }
    savingKey = key;
    saveError = '';
    saved = false;
    try {
      const response = await notifications.updateSettings({ [key]: value });
      settings = response.email;
      saved = true;
    } catch (err: any) {
      saveError = err?.message || t('notification_settings.save_failed');
      // The switch goes back to what the server still holds.
      input.checked = settings[key];
    } finally {
      savingKey = null;
    }
  }
</script>

<svelte:head>
  <title>{t('notification_settings.title')} · Plombir Git</title>
</svelte:head>

<div class="page-container notification-settings-page">
  <header class="page-header">
    <h1>{t('notification_settings.title')}</h1>
    <p>{t('notification_settings.description')}</p>
  </header>

  {#if loading}
    <p class="muted">{t('common.loading')}</p>
  {:else if loadError}
    <div class="message error-box" role="alert">{loadError}</div>
  {:else if settings}
    {#if saveError}<div class="message error-box save-error" role="alert">{saveError}</div>{/if}
    {#if saved}<div class="message success-box" role="status">{t('notification_settings.saved')}</div>{/if}

    <section class="section">
      <h2>{t('notification_settings.email_title')}</h2>
      <ul class="category-list">
        {#each EMAIL_NOTIFICATION_KEYS as key (key)}
          <li class="category" data-category={key}>
            <label>
              <input
                type="checkbox"
                checked={settings[key]}
                disabled={savingKey !== null}
                onchange={(event) => toggle(key, event)}
              />
              <span class="category-text">
                <span class="category-name">{t(`notification_settings.category.${key}.name`, undefined, formatTranslationFallback(key))}</span>
                <span class="category-hint">{t(`notification_settings.category.${key}.hint`, undefined, formatTranslationFallback(key))}</span>
              </span>
            </label>
          </li>
        {/each}
      </ul>
      <p class="muted footnote">{t('notification_settings.in_app_note')}</p>
    </section>
  {/if}
</div>

<style>
  .notification-settings-page { max-width: 760px; }
  .page-header { margin-bottom: 24px; }
  .page-header p { color: var(--text-secondary); margin-top: 6px; }
  h2 { font-size: 18px; margin: 0 0 12px; }
  .section { margin-bottom: 32px; }
  .message { margin-bottom: 20px; padding: 14px 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error-box { color: var(--red); background: color-mix(in srgb, var(--red) 10%, transparent); }
  .success-box { color: var(--green); background: color-mix(in srgb, var(--green) 10%, transparent); }
  .category-list { list-style: none; margin: 0; padding: 0; border: 1px solid var(--border); border-radius: var(--radius); }
  .category { padding: 12px 16px; border-bottom: 1px solid var(--border); }
  .category:last-child { border-bottom: none; }
  .category label { display: flex; align-items: flex-start; gap: 12px; cursor: pointer; }
  .category input { margin-top: 3px; }
  .category-text { display: flex; flex-direction: column; gap: 2px; }
  .category-name { font-weight: 600; }
  .category-hint { color: var(--text-secondary); font-size: 13px; }
  .muted { color: var(--text-secondary); }
  .footnote { margin-top: 12px; font-size: 13px; }
</style>
