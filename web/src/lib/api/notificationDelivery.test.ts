import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Page from '../../routes/settings/notifications/+page.svelte';
import SudoPrompt from '../components/SudoPrompt.svelte';
import { NOTIFICATION_EVENTS, type DeliverySettings, type DeliverySettingsPatch, type NotificationTestResult } from './notifications';
import { locale } from '../i18n';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, notifications, resetTestClient, setSudoPrompt } from '../test/client';
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

beforeEach(async () => {
  vi.clearAllMocks(); resetTestClient(); locale.set('en'); stored = defaults();
  auth.me.mockResolvedValue({ id: 1, username: 'alice', mfa_enabled: true, has_password: true, auth_provider: 'local' });
  await fetchUser();
  notifications.settings.mockImplementation(async () => structuredClone(stored));
  notifications.updateDelivery.mockImplementation(async (patch: DeliverySettingsPatch) => {
    stored.locale = patch.locale ?? stored.locale;
    for (const [key, channels] of Object.entries(patch.events ?? {})) Object.assign(stored.events[key as keyof typeof stored.events], channels);
    stored.revision++;
    return structuredClone(stored);
  });
  notifications.testDelivery.mockResolvedValue(result());
});
afterEach(async () => { await page?.destroy(); await prompt?.destroy(); page = prompt = undefined; await logout(); locale.set('en'); });

describe('saved notification delivery', () => {
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
  it.each(['cancel','wrong factor'])('uses the existing SudoPrompt and preserves saved channels on %s', async (kind) => {
    prompt=await renderComponent(SudoPrompt);
    const ask=setSudoPrompt.mock.calls.find(([handler]) => typeof handler === 'function')![0] as () => Promise<boolean>;
    notifications.updateDelivery.mockImplementation(async () => { if (!await ask()) throw new Error('Authentication cancelled'); return structuredClone(stored); });
    page=await renderComponent(Page);
    await check(row('mention').querySelectorAll('input')[0],false);
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
    expect(row('mention').querySelectorAll('input')[0].checked).toBe(true);
    expect(stored.events.mention.email).toBe(true);
    expect(notifications.updateDelivery).toHaveBeenCalledTimes(1);
  });
});
