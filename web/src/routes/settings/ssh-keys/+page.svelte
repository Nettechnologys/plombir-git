<script lang="ts">
  import { goto } from '$app/navigation';
  import { sshKeys, type SshKey } from '$lib/api/client.svelte';
  import { t } from '$lib/i18n';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';

  let keys = $state<SshKey[]>([]);
  let loading = $state(true);
  let adding = $state(false);
  let deletingId = $state<number | null>(null);
  let error = $state('');
  let success = $state('');
  let title = $state('');
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
    try {
      loading = true;
      error = '';
      keys = await sshKeys.list();
    } catch (err: any) {
      error = err.message || t('ssh_keys.load_failed');
    } finally {
      loading = false;
    }
  }

  async function addKey(event: SubmitEvent) {
    event.preventDefault();
    if (!title.trim()) {
      error = t('ssh_keys.title_required');
      return;
    }
    if (!publicKey.trim()) {
      error = t('ssh_keys.key_required');
      return;
    }

    try {
      adding = true;
      error = '';
      success = '';
      await sshKeys.create(title.trim(), publicKey.trim());
      title = '';
      publicKey = '';
      success = t('ssh_keys.added');
      await loadKeys();
    } catch (err: any) {
      error = err.message || t('ssh_keys.add_failed');
    } finally {
      adding = false;
    }
  }

  async function deleteKey(key: SshKey) {
    if (!confirm(t('ssh_keys.delete_confirm', { title: key.title }))) return;

    try {
      deletingId = key.id;
      error = '';
      success = '';
      await sshKeys.delete(key.id);
      success = t('ssh_keys.deleted');
      await loadKeys();
    } catch (err: any) {
      error = err.message || t('ssh_keys.delete_failed');
    } finally {
      deletingId = null;
    }
  }

  function formatDate(value?: string | null) {
    if (!value) return t('ssh_keys.never');
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString();
  }
</script>

<svelte:head>
  <title>{t('ssh_keys.title')} · IronForge</title>
</svelte:head>

<div class="page-container ssh-keys-page">
  <header class="page-header">
    <h1>{t('ssh_keys.title')}</h1>
    <p>{t('ssh_keys.description')}</p>
  </header>

  {#if error}<div class="message error-box" role="alert">{error}</div>{/if}
  {#if success}<div class="message success-box">{success}</div>{/if}

  <section class="section">
    <h2>{t('ssh_keys.add_title')}</h2>
    <form class="create-form" onsubmit={addKey}>
      <label for="ssh-key-title">{t('ssh_keys.name')}</label>
      <input
        id="ssh-key-title"
        bind:value={title}
        placeholder={t('ssh_keys.name_placeholder')}
        disabled={adding}
        maxlength="100"
      />

      <label for="ssh-public-key">{t('ssh_keys.public_key')}</label>
      <textarea
        id="ssh-public-key"
        bind:value={publicKey}
        placeholder={t('ssh_keys.public_key_placeholder')}
        disabled={adding}
        rows="4"
        spellcheck="false"
      ></textarea>

      <div>
        <button class="btn btn-primary" type="submit" disabled={adding}>
          {adding ? t('ssh_keys.adding') : t('ssh_keys.add')}
        </button>
      </div>
    </form>
  </section>

  <section class="section">
    <h2>{t('ssh_keys.existing')}</h2>
    {#if loading}
      <p class="muted">{t('ssh_keys.loading')}</p>
    {:else if keys.length === 0}
      <div class="empty-state">{t('ssh_keys.empty')}</div>
    {:else}
      <div class="key-list">
        {#each keys as key (key.id)}
          <article class="key-card">
            <div class="key-main">
              <h3>{key.title}</h3>
              <code>{key.fingerprint}</code>
              <dl>
                <div>
                  <dt>{t('ssh_keys.created')}</dt>
                  <dd>{formatDate(key.created_at)}</dd>
                </div>
                <div>
                  <dt>{t('ssh_keys.last_used')}</dt>
                  <dd>{formatDate(key.last_used_at)}</dd>
                </div>
              </dl>
            </div>
            <button
              class="btn btn-danger"
              type="button"
              disabled={deletingId === key.id}
              onclick={() => deleteKey(key)}
            >
              {deletingId === key.id ? t('ssh_keys.deleting') : t('ssh_keys.delete')}
            </button>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .ssh-keys-page { max-width: 880px; }
  .page-header { margin-bottom: 24px; }
  h1 { margin: 0 0 6px; font-size: 28px; }
  h2 { margin: 0 0 16px; font-size: 18px; }
  h3 { margin: 0; font-size: 16px; }
  p { margin: 0; color: var(--text-secondary); }
  .section { margin-bottom: 32px; padding-bottom: 28px; border-bottom: 1px solid var(--border); }
  .create-form { display: grid; gap: 8px; }
  label { margin-top: 8px; color: var(--text-secondary); font-size: 13px; font-weight: 600; }
  input, textarea { width: 100%; padding: 8px 10px; }
  textarea { resize: vertical; font-family: var(--font-mono, monospace); }
  .message, .empty-state { margin-bottom: 20px; padding: 14px 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error-box { color: var(--red); background: color-mix(in srgb, var(--red) 10%, transparent); }
  .success-box { color: var(--green); background: color-mix(in srgb, var(--green) 10%, transparent); }
  .key-list { display: grid; gap: 12px; }
  .key-card { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .key-main { min-width: 0; }
  code { display: block; margin-top: 8px; overflow-wrap: anywhere; color: var(--text-secondary); }
  dl { display: flex; flex-wrap: wrap; gap: 20px; margin: 12px 0 0; }
  dl div { display: flex; gap: 6px; }
  dt { color: var(--text-secondary); }
  dd { margin: 0; }
  .muted { color: var(--text-secondary); }
  @media (max-width: 640px) {
    .key-card { align-items: flex-start; flex-direction: column; }
  }
</style>
