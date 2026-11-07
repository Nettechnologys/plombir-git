import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SvelteMap } from 'svelte/reactivity';
import Page from '../../routes/settings/notifications/+page.svelte';
import SudoPrompt from '../components/SudoPrompt.svelte';
import { EMAIL_NOTIFICATION_KEYS, NOTIFICATION_EVENTS, type DeliverySettings, type DeliverySettingsPatch, type NotificationTestResult, type NotificationChannel } from './notifications';
import { locale } from '../i18n';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, notifications, resetTestClient, setSudoPrompt, setToken } from '../test/client';
import { check, change, click, element, input, renderComponent, settle, submit, type RenderedComponent } from '../test/render';

let page: RenderedComponent | undefined;
let prompt: RenderedComponent | undefined;
let stored: DeliverySettings;
const defaults = () => ({
  email: { review_requested: true, mention: true, assigned: true, ci_failed: true, participating: true, ci_triggered: false },
  events: Object.fromEntries(NOTIFICATION_EVENTS.map(key => [key, { email: key !== 'ci_triggered', telegram: true }])),
  locale: 'en', revision: 4,
  telegram: { token_configured: true, chat_id: -1001234, message_thread_id: 42, confirmed: true },
  capabilities: { email: true, email_reason: null },
} as DeliverySettings);
const result = (event = 'fixture', email = 'sent', telegram = 'sent') => ({event_id: event, channels: { email: { status: email, reason: email === 'unavailable' ? 'smtp_unavailable' : null }, telegram: { status: telegram, reason: telegram === 'failed' ? 'telegram_unauthorized' : null } }} as NotificationTestResult);
function deferred<T>() { let resolve!: (v: T) => void; let reject!: (e: unknown) => void; const promise = new Promise<T>((a,b) => {resolve = a; reject = b;}); return {promise,resolve,reject}; }
const row = (key: string) => element(page!.container, `[data-category="${key}"]`);
const groupEvents = (group: string) => NOTIFICATION_EVENTS.filter(key => EMAIL_NOTIFICATION_KEYS.includes(key as typeof EMAIL_NOTIFICATION_KEYS[number]) === (group === 'activity'));
const bulk = (group: string, channel: string, checked: boolean) => element<HTMLButtonElement>(page!.container, `[data-bulk="${group}-${channel}"] button:nth-of-type(${checked ? 1 : 2})`);

beforeEach(async () => {
  vi.clearAllMocks(); resetTestClient(); locale.set('en'); stored = defaults();
  auth.me.mockResolvedValue({ id: 1, username: 'alice', mfa_enabled: true, has_password: true, auth_provider: 'local' });
  await fetchUser();
  notifications.settings.mockImplementation(async () => structuredClone(stored));
  notifications.updateDelivery.mockImplementation(async (patch: DeliverySettingsPatch) => {
    stored.locale = patch.locale ?? stored.locale;
    for (const [key, channels] of Object.entries(patch.events ?? {})) Object.assign(stored.events[key as keyof typeof stored.events], channels);
    if (patch.telegram?.action === 'set') stored.telegram = {
      token_configured: !!patch.telegram.token || stored.telegram.token_configured,
      chat_id: patch.telegram.chat_id, message_thread_id: patch.telegram.message_thread_id, confirmed: false,
    };
    if (patch.telegram?.action === 'remove') stored.telegram = {
      token_configured: false, chat_id: null, message_thread_id: null, confirmed: false,
    };
    stored.revision++;
    return structuredClone(stored);
  });
  notifications.testDelivery.mockResolvedValue(result());
});
afterEach(async () => { await page?.destroy(); await prompt?.destroy(); page = prompt = undefined; await logout(); locale.set('en'); });

describe('saved notification delivery', () => {
  it('explains initial selection and renders the authoritative choices returned when Telegram is added', async () => {
    stored.telegram = { token_configured: false, chat_id: null, message_thread_id: null, confirmed: false };
    stored.events.mention.email = stored.email.mention = false;
    for (const channels of Object.values(stored.events)) channels.telegram = false;
    notifications.updateDelivery.mockImplementationOnce(async () => {
      for (const channels of Object.values(stored.events)) channels.telegram = channels.email;
      stored.telegram = { token_configured: true, chat_id: -1001234, message_thread_id: null, confirmed: false };
      stored.revision++;
      return structuredClone(stored);
    });
    page = await renderComponent(Page);
    expect(page.container.textContent).toContain('same events selected for email');
    expect(bulk('activity', 'telegram', true).disabled).toBe(true);
    await change(element(page.container, '#telegram-action'), 'replace');
    await input(element(page.container, '#telegram-token'), '123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi');
    await input(element(page.container, '#telegram-chat'), '-1001234');
    await submit(element(page.container, 'form'));
    expect(notifications.updateDelivery).toHaveBeenCalledExactlyOnceWith({ locale: 'en', telegram: { action: 'set', token: '123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi', chat_id: -1001234, message_thread_id: null } });
    for (const key of NOTIFICATION_EVENTS) {
      expect(row(key).querySelectorAll('input')[1].checked).toBe(stored.events[key].email);
    }
    expect(page.container.querySelector('#telegram-token')).toBeNull();
    expect(bulk('activity', 'telegram', true).disabled).toBe(false);
  });
  it.each(['activity', 'security'])('selects and clears just the chosen channel in the %s block in one request', async group => {
    page = await renderComponent(Page);
    locale.set('ru'); await settle();
    for (const channel of ['email', 'telegram'] as NotificationChannel[]) {
      for (const checked of [false, true]) {
        const before = structuredClone(stored.events);
        notifications.updateDelivery.mockClear();
        await click(bulk(group, channel, checked));
        expect(notifications.updateDelivery).toHaveBeenCalledExactlyOnceWith({
          events: Object.fromEntries(groupEvents(group).map(key => [key, { [channel]: checked }])), locale: 'ru',
        });
        for (const key of NOTIFICATION_EVENTS) {
          for (const name of ['email', 'telegram'] as NotificationChannel[]) {
            const expected = groupEvents(group).includes(key) && name === channel ? checked : before[key][name];
            expect(stored.events[key][name]).toBe(expected);
            expect(row(key).querySelectorAll('input')[name === 'email' ? 0 : 1].checked).toBe(expected);
          }
        }
        await click(bulk(group, channel, checked));
        expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
      }
    }
  });
  it('does not submit bulk actions for unavailable channels or an unsaved destination', async () => {
    stored.capabilities.email = false;
    stored.telegram.token_configured = false;
    page = await renderComponent(Page);
    for (const group of ['activity', 'security']) for (const channel of ['email', 'telegram']) {
      expect(bulk(group, channel, false).disabled).toBe(true);
      await click(bulk(group, channel, false));
    }
    expect(notifications.updateDelivery).not.toHaveBeenCalled();
    await page.destroy(); stored = defaults(); page = await renderComponent(Page);
    await input(element(page.container, '#telegram-chat'), '-1009999');
    await click(bulk('security', 'email', false));
    expect(notifications.updateDelivery).not.toHaveBeenCalled();
  });
  it('guards concurrent bulk requests and keeps selections and focus after a failed save', async () => {
    const pendingSave = deferred<DeliverySettings>();
    notifications.updateDelivery.mockReturnValueOnce(pendingSave.promise);
    page = await renderComponent(Page);
    const opener = bulk('security', 'telegram', false); opener.focus();
    await click(opener);
    await click(bulk('activity', 'email', false));
    await click(opener);
    expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
    expect(opener.disabled).toBe(false);
    expect(opener.getAttribute('aria-disabled')).toBe('true');
    pendingSave.reject(new Error('Fixture save failure')); await settle();
    expect(document.activeElement).toBe(opener);
    for (const key of groupEvents('security')) expect(row(key).querySelectorAll('input')[1].checked).toBe(true);
    expect(page.container.textContent).toContain('Fixture save failure');
    expect(opener.getAttribute('aria-disabled')).toBe('false');
  });
  it.each(['cancel', 'success'])('uses one sudo prompt for bulk deselection and preserves opener focus on %s', async outcome => {
    prompt = await renderComponent(SudoPrompt);
    const ask = setSudoPrompt.mock.calls.find(([handler]) => typeof handler === 'function')![0] as () => Promise<boolean>;
    notifications.updateDelivery.mockImplementation(async (patch: DeliverySettingsPatch) => {
      if (!await ask()) throw new Error('Authentication cancelled');
      for (const [key, channels] of Object.entries(patch.events ?? {})) Object.assign(stored.events[key as keyof typeof stored.events], channels);
      stored.revision++;
      return structuredClone(stored);
    });
    page = await renderComponent(Page);
    const opener = bulk('security', 'telegram', false); opener.focus(); await click(opener);
    const dialog = element(document.body, '[role=dialog]');
    expect(dialog.querySelector('#sudo-code')).not.toBeNull();
    if (outcome === 'success') {
      auth.sudo.mockResolvedValue({ token: 'fixture-session' });
      await input(element(dialog, '#sudo-password'), 'fixture-password');
      await input(element(dialog, '#sudo-code'), '123456');
      await submit(element(dialog, 'form'));
    } else await click(element(dialog, '.actions button'));
    expect(document.body.querySelector('[role=dialog]')).toBeNull();
    expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(opener);
    for (const key of groupEvents('security')) expect(row(key).querySelectorAll('input')[1].checked).toBe(outcome === 'cancel');
    expect(row('mention').querySelectorAll('input')[1].checked).toBe(true);
  });
  it('renders every event with two independent channels and a test, with no saved token input', async () => {
    page = await renderComponent(Page);
    expect(page.container.querySelectorAll('[data-category]')).toHaveLength(36);
    for (const key of NOTIFICATION_EVENTS) {
      expect(row(key).querySelectorAll('input[type="checkbox"]')).toHaveLength(2);
      expect(row(key).querySelector('.test-button')).not.toBeNull();
    }
    expect(page.container.querySelector('#telegram-token')).toBeNull();
    expect(element<HTMLInputElement>(page.container, '#telegram-chat').value).toBe('-1001234');
    expect(page.container.textContent).toContain('previously confirmed destinations');
  });
  it('saves just the changed channel with the current locale and shows the response', async () => {
    page = await renderComponent(Page); locale.set('ru'); await settle();
    await check(row('new_device').querySelectorAll('input')[1], false);
    expect(notifications.updateDelivery).toHaveBeenCalledWith({events:{new_device:{telegram:false}},locale:'ru'});
    expect(row('new_device').querySelectorAll('input')[0].checked).toBe(true);
    expect(row('new_device').querySelectorAll('input')[1].checked).toBe(false);
    expect(page.container.textContent).toContain('Сохранённый язык доставки: Русский');
  });
  it('blocks tests for unsaved credentials or language and never sends a secret in a test request', async () => {
    page = await renderComponent(Page);
    await change(element(page.container,'#telegram-action'),'replace');
    await input(element(page.container,'#telegram-token'),'123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi');
    expect(element<HTMLButtonElement>(row('new_device'),'.test-button').disabled).toBe(true);
    await click(element(row('new_device'),'.test-button'));
    expect(notifications.testDelivery).not.toHaveBeenCalled();
    await submit(element(page.container,'form'));
    expect(notifications.updateDelivery).toHaveBeenCalledWith({locale:'en',telegram:{action:'set',token:'123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi',chat_id:-1001234,message_thread_id:42}});
    expect(page.container.querySelector('#telegram-token')).toBeNull();
    locale.set('zh-CN'); await settle();
    expect(element<HTMLButtonElement>(row('new_device'),'.test-button').disabled).toBe(true);
    await submit(element(page.container,'form'));
    expect(notifications.updateDelivery).toHaveBeenLastCalledWith({locale:'zh-CN'});
    await click(element(row('new_device'),'.test-button'));
    expect(notifications.testDelivery).toHaveBeenCalledExactlyOnceWith('new_device');
  });
  it('keeps test pending and results independent across rows and channels', async () => {
    const first=deferred<NotificationTestResult>(); const second=deferred<NotificationTestResult>();
    notifications.testDelivery.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    page=await renderComponent(Page);
    await click(element(row('new_device'),'.test-button'));
    await click(element(row('token_created'),'.test-button'));
    expect(element<HTMLButtonElement>(row('new_device'),'.test-button').disabled).toBe(true);
    second.resolve(result('second','unavailable','sent')); await settle();
    expect(row('token_created').textContent).toContain('Telegram: Sent');
    expect(row('token_created').textContent).toContain('SMTP is not configured');
    expect(row('new_device').querySelector('.test-results')).toBeNull();
    first.resolve(result('first','sent','failed')); await settle();
    expect(row('new_device').textContent).toContain('Email: Sent');
    expect(row('new_device').textContent).toContain('The bot token was rejected');
  });
  it('polls the saved test id and displays its terminal result without sending twice', async () => {
    vi.useFakeTimers();
    try {
      notifications.testDelivery.mockResolvedValue(result('queued-fixture','queued','queued'));
      notifications.testResult.mockResolvedValue(result('queued-fixture','sent','sent'));
      page=await renderComponent(Page); await click(element(row('mention'),'.test-button'));
      expect(element<HTMLButtonElement>(row('mention'),'.test-button').disabled).toBe(true);
      await vi.advanceTimersByTimeAsync(1000); await settle();
      expect(notifications.testResult).toHaveBeenCalledExactlyOnceWith('queued-fixture');
      expect(notifications.testDelivery).toHaveBeenCalledOnce();
      expect(row('mention').textContent).toContain('Telegram: Sent');
      expect(element<HTMLButtonElement>(row('mention'),'.test-button').disabled).toBe(false);
    } finally { vi.useRealTimers(); }
  });
  it('disables unavailable channel inputs but still shows explicit per-channel test results', async () => {
    stored.capabilities.email=false; stored.telegram.token_configured=false;
    notifications.testDelivery.mockResolvedValue(result('off','unavailable','unavailable'));
    page=await renderComponent(Page);
    expect(Array.from(row('mention').querySelectorAll('input')).every(box => box.disabled)).toBe(true);
    await click(element(row('mention'),'.test-button'));
    expect(notifications.testDelivery).toHaveBeenCalledWith('mention');
    expect(row('mention').textContent).toContain('Email: Unavailable');
  });
  it('ignores an old test response after account identity changes', async () => {
    const old=deferred<NotificationTestResult>(); notifications.testDelivery.mockReturnValue(old.promise);
    page=await renderComponent(Page); await click(element(row('mention'),'.test-button'));
    auth.me.mockResolvedValue({id:2,username:'bob'}); stored.telegram.chat_id=777;
    await fetchUser(); await settle();
    old.resolve(result()); await settle();
    expect(page.container.querySelector('.test-results')).toBeNull();
    expect(element<HTMLInputElement>(page.container,'#telegram-chat').value).toBe('777');
  });
  it.each(['sent', 'error'])('clears a completed %s result when another destination is saved and can test it', async outcome => {
    if (outcome === 'error') notifications.testDelivery.mockRejectedValueOnce(new Error('Old destination failed'));
    page = await renderComponent(Page);
    await click(element(row('mention'), '.test-button'));
    expect(row('mention').querySelector(outcome === 'sent' ? '.test-results' : '.test-error')).not.toBeNull();

    await input(element(page.container, '#telegram-chat'), '-1009999');
    await submit(element(page.container, 'form'));
    expect(row('mention').querySelector('.test-results')).toBeNull();
    expect(row('mention').querySelector('.test-error')).toBeNull();
    expect(element<HTMLButtonElement>(page.container, 'button[type=submit]').disabled).toBe(false);
    expect(element<HTMLButtonElement>(row('mention'), '.test-button').disabled).toBe(false);
    expect(notifications.testDelivery).toHaveBeenCalledTimes(1);

    await click(element(row('mention'), '.test-button'));
    expect(notifications.testDelivery).toHaveBeenCalledTimes(2);
    expect(row('mention').textContent).toContain('Telegram: Sent');
  });
  it.each(['sent', 'error'])('ignores a late %s poll from the previous revision without clearing the new pending test', async outcome => {
    vi.useFakeTimers();
    try {
      const oldPoll = deferred<NotificationTestResult>();
      const newTest = deferred<NotificationTestResult>();
      notifications.testDelivery.mockResolvedValueOnce(result('old', 'queued', 'queued')).mockReturnValueOnce(newTest.promise);
      notifications.testResult.mockReturnValueOnce(oldPoll.promise);
      page = await renderComponent(Page);
      await click(element(row('mention'), '.test-button'));
      await vi.advanceTimersByTimeAsync(1000); await settle();
      expect(notifications.testResult).toHaveBeenCalledExactlyOnceWith('old');

      await input(element(page.container, '#telegram-chat'), '-1009999');
      await submit(element(page.container, 'form'));
      expect(row('mention').querySelector('.test-results')).toBeNull();
      expect(element<HTMLButtonElement>(page.container, 'button[type=submit]').disabled).toBe(false);
      await click(element(row('mention'), '.test-button'));
      if (outcome === 'sent') oldPoll.resolve(result('old'));
      else oldPoll.reject(new Error('Old poll failed'));
      await settle();
      await vi.advanceTimersByTimeAsync(35_000); await settle();
      expect(row('mention').querySelector('.test-results')).toBeNull();
      expect(row('mention').querySelector('.test-error')).toBeNull();
      expect(element<HTMLButtonElement>(row('mention'), '.test-button').disabled).toBe(true);
      expect(notifications.testResult).toHaveBeenCalledTimes(1);
      expect(notifications.testDelivery).toHaveBeenCalledTimes(2);

      newTest.resolve(result('new')); await settle();
      expect(row('mention').textContent).toContain('Telegram: Sent');
      expect(element<HTMLButtonElement>(row('mention'), '.test-button').disabled).toBe(false);
    } finally { vi.useRealTimers(); }
  });
  it.each(['locale', 'channel'])('stops a queued poll when the saved %s changes without leaving save busy', async kind => {
    vi.useFakeTimers();
    try {
      notifications.testDelivery.mockResolvedValue(result('old', 'queued', 'queued'));
      page = await renderComponent(Page);
      await click(element(row('mention'), '.test-button'));
      if (kind === 'locale') {
        locale.set('ru'); await settle();
        await submit(element(page.container, 'form'));
      } else {
        await check(row('mention').querySelectorAll('input')[1], false);
      }
      await vi.advanceTimersByTimeAsync(35_000); await settle();
      expect(notifications.testResult).not.toHaveBeenCalled();
      expect(notifications.testDelivery).toHaveBeenCalledTimes(1);
      expect(row('mention').querySelector('.test-results')).toBeNull();
      expect(element<HTMLButtonElement>(row('mention'), '.test-button').disabled).toBe(false);
      expect(element<HTMLButtonElement>(page.container, 'button[type=submit]').disabled).toBe(false);
      expect(row('mention').querySelector('input')?.getAttribute('aria-disabled')).toBe('false');
    } finally { vi.useRealTimers(); }
  });
  it('keeps an existing result after a no-op save with the same revision', async () => {
    notifications.updateDelivery.mockImplementation(async () => structuredClone(stored));
    page = await renderComponent(Page);
    await click(element(row('mention'), '.test-button'));
    await submit(element(page.container, 'form'));
    expect(row('mention').textContent).toContain('Telegram: Sent');
    expect(notifications.testDelivery).toHaveBeenCalledTimes(1);
  });
  it.each(['identity', 'unmount'])('stops queued polling on %s', async lifecycle => {
    vi.useFakeTimers();
    try {
      notifications.testDelivery.mockResolvedValue(result('old', 'queued', 'queued'));
      page = await renderComponent(Page);
      await click(element(row('mention'), '.test-button'));
      if (lifecycle === 'identity') {
        auth.me.mockResolvedValue({ id: 2, username: 'bob' });
        await fetchUser(); await settle();
        expect(row('mention').querySelector('.test-results')).toBeNull();
      } else {
        await page.destroy(); page = undefined;
      }
      await vi.advanceTimersByTimeAsync(35_000); await settle();
      expect(notifications.testResult).not.toHaveBeenCalled();
      expect(notifications.testDelivery).toHaveBeenCalledTimes(1);
    } finally { vi.useRealTimers(); }
  });
  it.each(['success', 'failure'])('keeps the pending toggle focusable and respects a later focus choice on %s', async outcome => {
    const saving = deferred<DeliverySettings>();
    notifications.updateDelivery.mockReturnValueOnce(saving.promise);
    page = await renderComponent(Page);
    const opener = row('mention').querySelectorAll('input')[0];
    opener.focus();
    await check(opener, false);
    expect(document.activeElement).toBe(opener);
    expect(opener.disabled).toBe(false);
    expect(opener.getAttribute('aria-disabled')).toBe('true');
    const other = row('mention').querySelectorAll('input')[1];
    await check(other, false);
    expect(other.checked).toBe(true);
    expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
    other.focus();
    if (outcome === 'success') {
      stored.events.mention.email = false;
      stored.revision++;
      saving.resolve(structuredClone(stored));
    } else saving.reject(new Error('Save failed'));
    await settle();
    expect(document.activeElement).toBe(other);
    expect(opener.getAttribute('aria-disabled')).toBe('false');
    expect(opener.checked).toBe(outcome !== 'success');
  });
  it('returns focus to the initiating channel after successful sudo', async () => {
    // The real request reads a reactive bearer before its first await.
    const credential = new SvelteMap([['token', 'original-session']]);
    notifications.settings.mockImplementation(async () => {
      credential.get('token');
      return structuredClone(stored);
    });
    setToken.mockImplementation(value => { credential.set('token', value); });
    prompt = await renderComponent(SudoPrompt);
    const ask = setSudoPrompt.mock.calls.find(([handler]) => typeof handler === 'function')![0] as () => Promise<boolean>;
    notifications.updateDelivery.mockImplementation(async () => {
      if (!await ask()) throw new Error('Authentication cancelled');
      stored.events.mention.email = false; stored.revision++;
      return structuredClone(stored);
    });
    auth.sudo.mockResolvedValue({ token: 'fixture-session' });
    page = await renderComponent(Page);
    const opener = row('mention').querySelectorAll('input')[0];
    opener.focus(); await check(opener, false);
    const dialog = element(document.body, '[role=dialog]');
    await input(element(dialog, '#sudo-password'), 'fixture-password');
    await input(element(dialog, '#sudo-code'), '123456');
    await submit(element(dialog, 'form'));
    expect(document.body.querySelector('[role=dialog]')).toBeNull();
    expect(document.activeElement).toBe(opener);
    expect(opener.checked).toBe(false);
    expect(opener.getAttribute('aria-disabled')).toBe('false');
    expect(notifications.settings).toHaveBeenCalledTimes(1);
    expect(opener.isConnected).toBe(true);
  });
  it.each(['cancel','wrong factor'])('uses the existing SudoPrompt and preserves saved channels on %s', async (kind) => {
    prompt=await renderComponent(SudoPrompt);
    const ask=setSudoPrompt.mock.calls.find(([handler]) => typeof handler === 'function')![0] as () => Promise<boolean>;
    notifications.updateDelivery.mockImplementation(async () => { if (!await ask()) throw new Error('Authentication cancelled'); return structuredClone(stored); });
    page=await renderComponent(Page);
    const opener = row('mention').querySelectorAll('input')[0];
    opener.focus(); await check(opener,false);
    await settle();
    const dialog=element(document.body,'[role="dialog"]');
    expect(dialog.querySelector('#sudo-code')).not.toBeNull();
    if (kind === 'wrong factor') {
      auth.sudo.mockRejectedValue(new Error('Invalid code'));
      await input(element(dialog,'#sudo-password'),'fixture-password');
      await input(element(dialog,'#sudo-code'),'000000');
      await submit(element(dialog,'form'));
      expect(dialog.textContent).toContain('Invalid code');
      expect(stored.events.mention.email).toBe(true);
    }
    await click(element(dialog,'.actions button')); await settle();
    expect(document.activeElement).toBe(opener);
    expect(row('mention').querySelectorAll('input')[0].checked).toBe(true);
    expect(stored.events.mention.email).toBe(true);
    expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
  });
});
