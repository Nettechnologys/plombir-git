<script lang="ts">
  import { goto } from '$app/navigation';
  import { signingKeys, type SigningKey, type SigningKeyKind } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { formatDate, t } from '$lib/i18n';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const confirmer = createConfirmer();
  const listRequests = new LatestRequestFence<'signing-keys'>();
  let keys = $state<SigningKey[]>([]);
  let loading = $state(true);
  let adding = $state(false);
  let deleting = $state<Set<number>>(new Set());
  let error = $state('');
  let success = $state('');
  let title = $state('');
  let kind = $state<SigningKeyKind>('ssh');
  let publicKey = $state('');

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadKeys();
  });

  async function loadKeys() {
    const claim = listRequests.begin('signing-keys');
    loading = true;
    try {
      const next = await signingKeys.list();
      if (listRequests.owns(claim, 'signing-keys')) keys = next;
    } catch (cause: unknown) {
      if (listRequests.owns(claim, 'signing-keys')) error = message(cause);
    } finally {
      if (listRequests.owns(claim, 'signing-keys')) loading = false;
    }
  }

  function message(cause: unknown): string {
    return cause instanceof Error ? cause.message : String(cause);
  }

  async function addKey(event: SubmitEvent) {
    event.preventDefault();
    if (adding) return;
    if (!title.trim() || !publicKey.trim()) {
      error = t('signing_keys.required');
      return;
    }
    adding = true;
    error = '';
    success = '';
    try {
      await signingKeys.create(title.trim(), kind, publicKey.trim());
      title = '';
      publicKey = '';
      success = t('signing_keys.added');
      await loadKeys();
    } catch (cause: unknown) {
      error = message(cause);
    } finally {
      adding = false;
    }
  }

  async function deleteKey(key: SigningKey) {
    if (!(await confirmer.ask({
      title: t('signing_keys.delete'),
      message: t('signing_keys.confirm_delete', { title: key.title }),
      confirmLabel: t('signing_keys.delete'),
    }))) return;
    if (deleting.has(key.id)) return;
    deleting = new Set(deleting).add(key.id);
    error = '';
    success = '';
    try {
      await signingKeys.delete(key.id);
      success = t('signing_keys.deleted');
      await loadKeys();
    } catch (cause: unknown) {
      error = message(cause);
    } finally {
      const next = new Set(deleting);
      next.delete(key.id);
      deleting = next;
    }
  }
</script>

<svelte:head><title>{t('signing_keys.title')} · Plombir Git</title></svelte:head>

<div class="page-container signing-keys-page">
  <header class="page-header">
    <h1>{t('signing_keys.title')}</h1>
    <p>{t('signing_keys.description')}</p>
  </header>

  {#if error}<div class="message error-box" role="alert">{error}</div>{/if}
  {#if success}<div class="message success-box">{success}</div>{/if}

  <section class="section">
    <h2>{t('signing_keys.add_title')}</h2>
    <form class="create-form" onsubmit={addKey}>
      <label for="signing-title">{t('signing_keys.name')}</label>
      <input id="signing-title" bind:value={title} maxlength="100" disabled={adding} />
      <label for="signing-kind">{t('signing_keys.kind')}</label>
      <select id="signing-kind" bind:value={kind} disabled={adding}>
        <option value="ssh">{t('signing_keys.ssh')}</option>
        <option value="gpg">{t('signing_keys.gpg')}</option>
      </select>
      <label for="signing-public-key">{t('signing_keys.public_key')}</label>
      <textarea id="signing-public-key" bind:value={publicKey} rows={kind === 'gpg' ? 10 : 4} disabled={adding} spellcheck="false"></textarea>
      <p class="hint">{t('signing_keys.email_hint')}</p>
      <div><button class="btn btn-primary" type="submit" disabled={adding}>{adding ? t('signing_keys.adding') : t('signing_keys.add')}</button></div>
    </form>
  </section>

  <section class="section">
    <h2>{t('signing_keys.existing')}</h2>
    {#if loading}
      <p>{t('signing_keys.loading')}</p>
    {:else if keys.length === 0}
      <div class="empty-state">{t('signing_keys.empty')}</div>
    {:else}
      <div class="key-list">
        {#each keys as key (key.id)}
          <article class="key-card">
            <div class="key-main">
              <h3>{key.title} <small>{key.kind.toUpperCase()}</small></h3>
              <code>{key.fingerprint}</code>
              <p>{formatDate(key.created_at)}</p>
            </div>
            <button class="btn btn-danger" type="button" disabled={deleting.has(key.id)} onclick={() => deleteKey(key)}>
              {t('signing_keys.delete')}
            </button>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<ConfirmModal {confirmer} />

<style>
  .signing-keys-page { max-width: 880px; }
  .page-header, .section { margin-bottom: 24px; }
  .page-header p, .hint, .key-main p { color: var(--text-secondary); }
  .section { padding-bottom: 28px; border-bottom: 1px solid var(--border); }
  .create-form { display: grid; gap: 8px; }
  label { margin-top: 8px; color: var(--text-secondary); font-size: 13px; font-weight: 600; }
  input, textarea, select { width: 100%; padding: 8px 10px; }
  textarea { resize: vertical; font-family: var(--font-mono, monospace); }
  .message, .empty-state { margin-bottom: 20px; padding: 14px 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error-box { color: var(--red); background: color-mix(in srgb, var(--red) 10%, transparent); }
  .success-box { color: var(--green); background: color-mix(in srgb, var(--green) 10%, transparent); }
  .key-list { display: grid; gap: 12px; }
  .key-card { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .key-main { min-width: 0; }
  code { display: block; margin: 8px 0; overflow-wrap: anywhere; color: var(--text-secondary); }
  @media (max-width: 640px) { .key-card { align-items: flex-start; flex-direction: column; } }
</style>
