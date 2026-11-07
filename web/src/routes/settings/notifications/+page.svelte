<script lang="ts">
  import { goto } from '$app/navigation';
  import { onDestroy, untrack } from 'svelte';
  import {
    EMAIL_NOTIFICATION_KEYS, NOTIFICATION_EVENTS, notifications,
    type EmailNotificationKey, type NotificationSettings, type NotificationEvent,
    type NotificationChannel, type DeliverySettingsPatch, type NotificationTestResult,
  } from '$lib/api/client.svelte';
  import { activeLocale, formatTranslationFallback, t } from '$lib/i18n';
  import { getUser, isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';

  let settings = $state<NotificationSettings | null>(null);
  let loading = $state(true);
  let loadError = $state('');
  let savingKey = $state<string | null>(null);
  let saveError = $state('');
  let saved = $state(false);
  let mode = $state<'keep' | 'replace' | 'remove'>('keep');
  let token = $state('');
  let chat = $state('');
  let topic = $state('');
  let tests = $state<Partial<Record<NotificationEvent, NotificationTestResult>>>({});
  let testErrors = $state<Partial<Record<NotificationEvent, string>>>({});
  let pending = $state<Partial<Record<NotificationEvent, boolean>>>({});
  let generation = 0;
  let testGeneration = 0;
  let alive = true;
  const timers = new Map<ReturnType<typeof setTimeout>, () => void>();
  const modern = $derived(!!settings?.events);
  const dirty = $derived(mode !== 'keep' || token !== ''
    || chat !== String(settings?.telegram?.chat_id ?? '')
    || topic !== String(settings?.telegram?.message_thread_id ?? ''));
  const testDirty = $derived(dirty || !!settings?.locale && activeLocale() !== settings.locale);
  const busy = $derived(savingKey !== null);
  const keys = $derived(modern ? NOTIFICATION_EVENTS : EMAIL_NOTIFICATION_KEYS);
  const groups = $derived(modern ? ['activity', 'security'] : ['activity']);

  function clearTimers() {
    for (const [timer, resolve] of timers) { clearTimeout(timer); resolve(); }
    timers.clear();
  }
  function resetTests() {
    ++testGeneration;
    clearTimers();
    tests = {}; testErrors = {}; pending = {};
  }
  onDestroy(() => { alive = false; ++generation; token = ''; clearTimers(); });
  $effect(() => {
    if (!isAuthReady()) return;
    const id = getUser()?.id;
    ++generation;
    resetTests();
    settings = null; token = '';
    if (!isLoggedIn() || !id) { goto('/login'); return; }
    // A sudo bearer refresh must not reload this account's form mid-save.
    untrack(() => { void load(id); });
  });
  const owns = (stamp: number, id: number | undefined) => alive && generation === stamp && getUser()?.id === id;
  function apply(response: NotificationSettings) {
    // Test results belong to the saved revision, independently of save ownership.
    if (settings?.revision !== response.revision) resetTests();
    settings = response;
    chat = String(response.telegram?.chat_id ?? '');
    topic = String(response.telegram?.message_thread_id ?? '');
    token = ''; mode = 'keep';
  }
  async function load(id = getUser()?.id) {
    const stamp = ++generation;
    loading = true; loadError = ''; saveError = ''; saved = false; savingKey = null;
    try {
      const response = await notifications.settings();
      if (owns(stamp, id)) apply(response);
    } catch (err: any) {
      if (owns(stamp, id)) loadError = err?.message || t('notification_settings.load_failed');
    } finally { if (owns(stamp, id)) loading = false; }
  }
  function selected(key: NotificationEvent, channel: NotificationChannel) {
    return settings?.events?.[key]?.[channel] ?? (channel === 'email' ? settings?.email[key as EmailNotificationKey] ?? false : false);
  }
  function groupKeys(group: string) {
    return keys.filter(key => group === 'security' ? !EMAIL_NOTIFICATION_KEYS.includes(key as EmailNotificationKey) : EMAIL_NOTIFICATION_KEYS.includes(key as EmailNotificationKey));
  }
  function channelUnavailable(channel: NotificationChannel) {
    return dirty || modern && (channel === 'email' ? !settings?.capabilities?.email : !settings?.telegram?.token_configured);
  }
  async function selectGroup(group: string, channel: NotificationChannel, checked: boolean) {
    if (!settings || busy || channelUnavailable(channel)) return;
    const events = groupKeys(group);
    if (events.every(key => selected(key, channel) === checked)) return;
    const stamp = generation, id = getUser()?.id;
    savingKey = `${group}:${channel}`; saveError = ''; saved = false;
    try {
      const response = modern
        ? await notifications.updateDelivery({ events: Object.fromEntries(events.map(key => [key, { [channel]: checked }])), locale: activeLocale() })
        : await notifications.updateSettings(Object.fromEntries(events.map(key => [key, checked])));
      if (owns(stamp, id)) { apply(response); saved = true; }
    } catch (err: any) {
      if (owns(stamp, id)) saveError = err?.message || t('notification_settings.save_failed');
    } finally { if (owns(stamp, id)) savingKey = null; }
  }
  async function toggle(key: NotificationEvent, channel: NotificationChannel, event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const previous = selected(key, channel);
    if (!settings || busy || dirty) { input.checked = previous; return; }
    const stamp = generation, id = getUser()?.id;
    savingKey = `${key}:${channel}`; saveError = ''; saved = false;
    try {
      const response = modern
        ? await notifications.updateDelivery({ events: { [key]: { [channel]: input.checked } }, locale: activeLocale() })
        : await notifications.updateSettings({ [key]: input.checked });
      if (owns(stamp, id)) { apply(response); saved = true; }
    } catch (err: any) {
      if (owns(stamp, id)) { input.checked = previous; saveError = err?.message || t('notification_settings.save_failed'); }
    } finally { if (owns(stamp, id)) savingKey = null; }
  }
  async function save() {
    if (!settings || busy) return;
    const patch: DeliverySettingsPatch = { locale: activeLocale() };
    if (mode === 'remove') patch.telegram = { action: 'remove' };
    else if (dirty) {
      const chatId = Number(chat), topicId = topic.trim() ? Number(topic) : null;
      if (!chat.trim() || !Number.isSafeInteger(chatId) || !chatId || Math.abs(chatId) > 2 ** 52 - 1
        || topicId !== null && (!Number.isSafeInteger(topicId) || topicId <= 0 || topicId > 2147483647)
        || (mode === 'replace' || !settings.telegram?.token_configured) && !token.trim()) {
        saveError = t('notification_settings.invalid_destination'); return;
      }
      patch.telegram = { action: 'set', chat_id: chatId, message_thread_id: topicId,
        ...(mode === 'replace' || !settings.telegram?.token_configured ? { token: token.trim() } : {}) };
    }
    const stamp = generation, id = getUser()?.id;
    savingKey = 'configuration'; saveError = ''; saved = false;
    try {
      const response = await notifications.updateDelivery(patch);
      if (owns(stamp, id)) { apply(response); saved = true; }
    } catch (err: any) {
      if (owns(stamp, id)) saveError = err?.message || t('notification_settings.save_failed');
    } finally { if (owns(stamp, id)) savingKey = null; }
  }
  const isWaiting = (result: NotificationTestResult | undefined) => result && Object.values(result.channels).some(c => c.status === 'queued' || c.status === 'inflight');
  function pause() { return new Promise<void>(resolve => { const timer = setTimeout(() => { timers.delete(timer); resolve(); }, 1000); timers.set(timer, resolve); }); }
  async function test(key: NotificationEvent) {
    if (!settings || testDirty || busy || pending[key]) return;
    const stamp = generation, id = getUser()?.id;
    const revision = settings.revision, testStamp = testGeneration;
    const ownsTest = () => owns(stamp, id) && testGeneration === testStamp && settings?.revision === revision;
    pending[key] = true; testErrors[key] = '';
    try {
      let result = isWaiting(tests[key]) ? await notifications.testResult(tests[key]!.event_id) : await notifications.testDelivery(key);
      for (let attempt = 0; ownsTest(); attempt++) {
        tests[key] = result;
        if (!isWaiting(result) || attempt >= 30) break;
        await pause();
        if (!ownsTest()) return;
        result = await notifications.testResult(result.event_id);
      }
      if (ownsTest() && result.channels.telegram.status === 'sent') {
        // Confirm only the same saved revision. A late send must not make a
        // changed destination look verified, or overwrite an edited form.
        try {
          const fresh = await notifications.settings();
          if (ownsTest() && settings && fresh.revision === revision && settings.telegram && fresh.telegram) {
            settings = { ...settings, telegram: { ...settings.telegram, confirmed: fresh.telegram.confirmed } };
          }
        } catch { /* The delivery result remains valid if the status refresh fails. */ }
      }
    } catch (err: any) {
      if (ownsTest()) testErrors[key] = err?.message || t('notification_settings.test_failed');
    } finally { if (ownsTest()) pending[key] = false; }
  }
  function reason(code: string | null) {
    if (!code) return '';
    // Finite server codes only; unknown details use a localized safe fallback.
    if (['telegram_timeout', 'telegram_network'].includes(code)) code = 'telegram_transport';
    if (['telegram_invalid_response', 'telegram_redirect_refused', 'telegram_response_too_large'].includes(code)) code = 'telegram_protocol';
    if (['telegram_invalid_chat', 'telegram_invalid_topic'].includes(code)) code = 'telegram_invalid_destination';
    if (code === 'telegram_invalid_token') code = 'telegram_unauthorized';
    const known = ['smtp_unavailable','email_unavailable','telegram_not_configured','channel_disabled','telegram_unauthorized','telegram_forbidden','telegram_invalid_destination','telegram_rate_limited','telegram_unavailable','telegram_protocol','telegram_transport','smtp_delivery_failed','destination_changed','encryption_unavailable'];
    return t(`notification_settings.reason.${known.includes(code) ? code : 'other'}`, undefined, t('notification_settings.reason.other'));
  }
</script>

<svelte:head><title>{t('notification_settings.title')} · Plombir Git</title></svelte:head>
<div class="page-container-narrow notification-settings-page">
  <header class="page-header"><h1>{t('notification_settings.title')}</h1><p>{t('notification_settings.description')}</p></header>
  {#if loading}
    <p class="muted">{t('common.loading')}</p>
  {:else if loadError}
    <div class="message error-box" role="alert">{loadError}</div>
    <button type="button" class="btn btn-secondary" onclick={() => load()}>{t('notification_settings.retry')}</button>
  {:else if settings}
    {#if saveError}<div class="message error-box save-error" role="alert">{saveError}</div>{/if}
    {#if saved}<div class="message success-box" role="status">{t('notification_settings.saved')}</div>{/if}
    {#if modern}
      <section class="section destination" aria-labelledby="destination-heading">
        <h2 id="destination-heading">{t('notification_settings.delivery')}</h2>
        <p class="muted">{t('notification_settings.saved_language', { language: settings.locale === 'ru' ? 'Русский' : settings.locale === 'zh-CN' ? '简体中文' : 'English' })}</p>
        {#if !settings.capabilities?.email}<p class="message unavailable">{t('notification_settings.reason.smtp_unavailable')}</p>{/if}
        <form onsubmit={(event) => { event.preventDefault(); void save(); }}>
          <fieldset disabled={busy}>
            <legend>{t('notification_settings.telegram')}</legend>
            <p class="muted">{settings.telegram?.token_configured ? t('notification_settings.token_saved') : t('notification_settings.token_missing')}</p>
            {#if !settings.telegram?.token_configured}<p class="hint">{t('notification_settings.initial_selection')}</p>{/if}
            {#if settings.telegram?.token_configured}<p class="muted">{settings.telegram.confirmed ? t('notification_settings.confirmed') : t('notification_settings.unconfirmed')}</p>{/if}
            <label for="telegram-action">{t('notification_settings.token_action')}</label>
            <select id="telegram-action" bind:value={mode}>
              <option value="keep">{t('notification_settings.keep')}</option>
              <option value="replace">{settings.telegram?.token_configured ? t('notification_settings.replace') : t('notification_settings.add')}</option>
              {#if settings.telegram?.token_configured}<option value="remove">{t('notification_settings.remove')}</option>{/if}
            </select>
            {#if mode === 'replace'}
              <label for="telegram-token">{t('notification_settings.bot_token')}</label>
              <input id="telegram-token" type="password" autocomplete="new-password" spellcheck="false" bind:value={token} maxlength="149" />
              <p class="hint">{t('notification_settings.botfather')}</p>
            {/if}
            {#if mode !== 'remove'}
              <div class="destination-fields">
                <div><label for="telegram-chat">{t('notification_settings.chat')}</label><input id="telegram-chat" inputmode="text" autocomplete="off" bind:value={chat} /></div>
                <div><label for="telegram-topic">{t('notification_settings.topic')}</label><input id="telegram-topic" inputmode="numeric" autocomplete="off" bind:value={topic} /></div>
              </div>
              <p class="hint">{t('notification_settings.destination_hint')}</p>
            {/if}
          </fieldset>
          <p class="warning">{t('notification_settings.protection')}</p>
          <p class="hint">{t('notification_settings.old_warning')}</p>
          <div class="form-actions"><button type="submit" class="btn btn-primary" disabled={busy} aria-busy={savingKey === 'configuration'}>{t('notification_settings.save_language')}</button>
            {#if dirty}<button type="button" class="btn btn-secondary" disabled={busy} onclick={() => { if (settings) apply(settings); saveError = ''; }}>{t('notification_settings.discard')}</button>{/if}
          </div>
        </form>
      </section>
      {#if testDirty}<p class="message unavailable" role="status">{t('notification_settings.save_before_test')}</p>{/if}
    {/if}
    {#each groups as group}
      <section class="section" aria-labelledby={`group-${group}`}>
        <h2 id={`group-${group}`}>{modern ? t(`notification_settings.${group}`, undefined, formatTranslationFallback(group)) : t('notification_settings.email_title')}</h2>
        <div class="bulk-actions">
          {#each (modern ? ['email', 'telegram'] : ['email']) as channel}
            <div class="bulk-channel" role="group" aria-labelledby={`group-${group} bulk-${group}-${channel}`} data-bulk={`${group}-${channel}`}>
              <span id={`bulk-${group}-${channel}`} class="bulk-label">{channel === 'email' ? t('notification_settings.email_title') : t('notification_settings.telegram')}</span>
              {#each [true, false] as checked}
                <button type="button" class="btn btn-secondary" aria-disabled={busy || groupKeys(group).every(key => selected(key, channel as NotificationChannel) === checked)} disabled={channelUnavailable(channel as NotificationChannel)} onclick={() => selectGroup(group, channel as NotificationChannel, checked)}>{checked ? t('notification_settings.select_all') : t('notification_settings.deselect_all')}</button>
              {/each}
            </div>
          {/each}
        </div>
        <ul class="category-list">
          {#each groupKeys(group) as key (key)}
            <li class="category" data-category={key}>
              <div class="category-text"><span class="category-name" id={`event-${key}`}>{t(modern ? `notification_settings.events.${key}` : `notification_settings.category.${key}.name`, undefined, formatTranslationFallback(key))}</span>
                {#if EMAIL_NOTIFICATION_KEYS.includes(key as EmailNotificationKey)}<span class="category-hint">{t(`notification_settings.category.${key}.hint`, undefined, formatTranslationFallback(key))}</span>{/if}
              </div>
              <div class="channel-actions">
                {#each (modern ? ['email', 'telegram'] : ['email']) as channel}
                  <!-- Keep the opener focusable during async sudo; toggle guards busy changes. -->
                  <label class="channel"><input type="checkbox" aria-labelledby={`event-${key} channel-${key}-${channel}`} checked={selected(key, channel as NotificationChannel)} aria-disabled={busy} disabled={channelUnavailable(channel as NotificationChannel)} onchange={(event) => toggle(key, channel as NotificationChannel, event)} /><span id={`channel-${key}-${channel}`}>{channel === 'email' ? t('notification_settings.email_title') : t('notification_settings.telegram')}</span></label>
                {/each}
                {#if modern}<button type="button" class="btn btn-secondary test-button" disabled={testDirty || busy || pending[key]} aria-busy={pending[key] ?? false} aria-label={`${t('notification_settings.test')} — ${t(`notification_settings.events.${key}`, undefined, formatTranslationFallback(key))}`} onclick={() => test(key)}>{pending[key] ? t('notification_settings.testing') : isWaiting(tests[key]) ? t('notification_settings.check_result') : t('notification_settings.test')}</button>{/if}
              </div>
              {#if tests[key]}<div class="test-results" role="status" aria-live="polite">{#each ['email', 'telegram'] as channel}{@const result = tests[key]!.channels[channel as NotificationChannel]}<p class:sent={result.status === 'sent'}>{channel === 'email' ? t('notification_settings.email_title') : t('notification_settings.telegram')}: {t(`notification_settings.status.${result.status}`, undefined, formatTranslationFallback(result.status))}{result.reason ? ` — ${reason(result.reason)}` : ''}</p>{/each}</div>{/if}
              {#if testErrors[key]}<p class="error-box test-error" role="alert">{testErrors[key]}</p>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/each}
    <p class="muted footnote">{t('notification_settings.in_app_note')}</p>
  {/if}
</div>

<style>
  .notification-settings-page { max-width: 860px; }
  .page-header { margin-bottom: 24px; }
  .page-header p, .muted, .hint, .category-hint { color: var(--text-secondary); }
  .page-header p { margin-top: 6px; }
  h2 { font-size: 18px; margin: 0 0 12px; }
  .section { margin-bottom: 28px; }
  .destination { border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; }
  fieldset { border: 0; padding: 0; min-width: 0; margin: 16px 0; }
  legend { font-weight: 600; margin-bottom: 8px; }
  form label { display: block; margin: 12px 0 6px; }
  form input, form select { width: 100%; box-sizing: border-box; min-width: 0; }
  .destination-fields { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .hint, .category-hint, .footnote, .test-results { font-size: 13px; }
  .hint { margin: 8px 0; }
  .warning { margin-top: 16px; }
  .form-actions, .channel-actions { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; }
  button { white-space: normal; overflow-wrap: anywhere; }
  .form-actions { margin-top: 16px; }
  .message { margin-bottom: 20px; padding: 14px 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error-box { color: var(--red); }
  .success-box, .sent { color: var(--green); }
  .unavailable { color: var(--text-secondary); }
  .category-list { list-style: none; margin: 0; padding: 0; border: 1px solid var(--border); border-radius: var(--radius); }
  .category { padding: 16px; border-bottom: 1px solid var(--border); }
  .category:last-child { border-bottom: none; }
  .category-text { display: flex; flex-direction: column; gap: 3px; margin-bottom: 12px; overflow-wrap: anywhere; }
  .category-name { font-weight: 600; }
  .channel { display: flex; align-items: center; gap: 6px; cursor: pointer; min-height: 32px; }
  .channel input { margin: 0; }
  .channel input[aria-disabled="true"] { opacity: 0.6; cursor: wait; }
  .bulk-actions { display: flex; flex-wrap: wrap; gap: 12px 24px; margin-bottom: 12px; }
  .bulk-channel { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; min-width: 0; }
  .bulk-label { font-weight: 600; }
  .bulk-channel button[aria-disabled="true"] { opacity: 0.6; cursor: default; }
  .test-button { margin-left: auto; }
  .test-results { margin-top: 10px; overflow-wrap: anywhere; }
  .test-results p { margin: 4px 0; }
  @media (max-width: 480px) { .destination { padding: 14px; } .destination-fields { grid-template-columns: 1fr; gap: 0; } .test-button { margin-left: 0; } .bulk-label { flex-basis: 100%; } }
</style>
