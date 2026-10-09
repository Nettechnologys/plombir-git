<!--
  Follow or unfollow an issue or a pull request (card_349c2b6a0d7c), and say
  why the viewer is receiving its notifications. Signed-in viewers only: the
  subscription routes need a session. An explicit unfollow is remembered by the
  server — commenting again does not resubscribe.
-->
<script lang="ts">
  import { notifications, type ThreadKind, type ThreadSubscription } from '$lib/api/client.svelte';
  import { createT, formatTranslationFallback } from '$lib/i18n';
  import { isLoggedIn } from '$lib/stores/auth.svelte';

  interface Props {
    owner: string;
    repo: string;
    kind: ThreadKind;
    number: number;
  }

  let { owner, repo, kind, number }: Props = $props();

  const t = createT();

  let subscription = $state<ThreadSubscription | null>(null);
  let busy = $state(false);
  let error = $state('');
  let generation = 0;

  let signedIn = $derived(isLoggedIn() === true);
  let reasonText = $derived(
    subscription?.subscribed
      ? t(
          `subscription.reason.${subscription.reason ?? 'manual'}`,
          undefined,
          t('subscription.reason_other', { reason: formatTranslationFallback(subscription.reason ?? '') }),
        )
      : t('subscription.not_subscribed'),
  );

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedKind = kind;
    const expectedNumber = number;
    const mine = ++generation;
    subscription = null;
    busy = false;
    error = '';
    if (!signedIn || !Number.isFinite(expectedNumber)) return;
    void load(expectedOwner, expectedRepo, expectedKind, expectedNumber, mine);
  });

  async function load(expectedOwner: string, expectedRepo: string, expectedKind: ThreadKind, expectedNumber: number, mine: number) {
    try {
      const next = await notifications.subscription(expectedOwner, expectedRepo, expectedKind, expectedNumber);
      if (mine === generation) subscription = next ?? null;
    } catch (err: any) {
      if (mine === generation) error = err?.message || t('subscription.load_failed');
    }
  }

  async function toggle() {
    if (!subscription || busy) return;
    const mine = generation;
    busy = true;
    error = '';
    try {
      const next = subscription.subscribed
        ? await notifications.unsubscribe(owner, repo, kind, number)
        : await notifications.subscribe(owner, repo, kind, number);
      if (mine === generation) subscription = next;
    } catch (err: any) {
      if (mine === generation) error = err?.message || t('subscription.update_failed');
    } finally {
      if (mine === generation) busy = false;
    }
  }
</script>

{#if signedIn}
  <section class="thread-subscription" aria-label={t('subscription.title')}>
    <h3>{t('subscription.title')}</h3>
    {#if subscription}
      <button type="button" class="btn-subscription" onclick={toggle} disabled={busy} aria-pressed={subscription.subscribed}>
        {subscription.subscribed ? t('subscription.unsubscribe') : t('subscription.subscribe')}
      </button>
      <p class="subscription-reason">{reasonText}</p>
    {:else if !error}
      <p class="subscription-reason">{t('common.loading')}</p>
    {/if}
    {#if error}
      <p class="subscription-error" role="alert">{error}</p>
    {/if}
  </section>
{/if}

<style>
  .thread-subscription {
    margin: 16px 0;
    padding: 12px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  h3 { margin: 0 0 8px; font-size: 14px; }
  .btn-subscription {
    padding: 4px 12px;
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-subscription:disabled { opacity: 0.5; cursor: not-allowed; }
  .subscription-reason { margin: 8px 0 0; font-size: 12px; color: var(--text-secondary); }
  .subscription-error { margin: 8px 0 0; font-size: 12px; color: var(--red); }
</style>
