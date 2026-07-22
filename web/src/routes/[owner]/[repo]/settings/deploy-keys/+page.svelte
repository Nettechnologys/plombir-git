<script lang="ts">
  import { page } from '$app/stores';
  import { deployKeys, type DeployKey } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let keys = $state<DeployKey[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let deletingId = $state<number | null>(null);
  let title = $state('');
  let publicKey = $state('');
  let readOnly = $state(true);
  let error = $state('');
  let success = $state('');

  $effect(() => {
    owner;
    repo;
    loadKeys();
  });

  async function loadKeys() {
    try {
      loading = true;
      error = '';
      keys = await deployKeys.list(owner, repo);
    } catch (err: any) {
      error = err.message || t('settings.deploy_keys.load_failed', 'Failed to load deploy keys.');
    } finally {
      loading = false;
    }
  }

  async function addKey(event: SubmitEvent) {
    event.preventDefault();
    if (!title.trim() || !publicKey.trim()) {
      error = t('settings.deploy_keys.required', 'Title and public key are required.');
      return;
    }
    try {
      saving = true;
      error = '';
      success = '';
      await deployKeys.create(owner, repo, title.trim(), publicKey.trim(), readOnly);
      title = '';
      publicKey = '';
      readOnly = true;
      success = t('settings.deploy_keys.created', 'Deploy key added.');
      await loadKeys();
    } catch (err: any) {
      error = err.message || t('settings.deploy_keys.create_failed', 'Failed to add deploy key.');
    } finally {
      saving = false;
    }
  }

  async function removeKey(key: DeployKey) {
    if (!confirm(t('settings.deploy_keys.delete_confirm', { title: key.title }))) return;
    try {
      deletingId = key.id;
      error = '';
      success = '';
      await deployKeys.delete(owner, repo, key.id);
      success = t('settings.deploy_keys.deleted', 'Deploy key removed.');
      await loadKeys();
    } catch (err: any) {
      error = err.message || t('settings.deploy_keys.delete_failed', 'Failed to remove deploy key.');
    } finally {
      deletingId = null;
    }
  }
</script>

<svelte:head><title>{t('settings.deploy_keys.title', 'Deploy keys')} · {owner}/{repo}</title></svelte:head>

<div class="deploy-keys-page">
  <header>
    <h1>{t('settings.deploy_keys.title', 'Deploy keys')}</h1>
    <p>{t('settings.deploy_keys.desc', 'Grant an automation key access to this repository only.')}</p>
  </header>

  {#if error}<div class="message error" role="alert">{error}</div>{/if}
  {#if success}<div class="message success">{success}</div>{/if}

  <section>
    <h2>{t('settings.deploy_keys.add', 'Add deploy key')}</h2>
    <form onsubmit={addKey}>
      <label for="deploy-key-title">{t('settings.deploy_keys.name', 'Title')}</label>
      <input id="deploy-key-title" bind:value={title} maxlength="100" disabled={saving} />
      <label for="deploy-public-key">{t('settings.deploy_keys.public_key', 'Public key')}</label>
      <textarea id="deploy-public-key" bind:value={publicKey} rows="4" spellcheck="false" disabled={saving}></textarea>
      <label class="checkbox"><input type="checkbox" bind:checked={readOnly} disabled={saving} /> {t('settings.deploy_keys.read_only', 'Read-only access')}</label>
      <button class="btn btn-primary" type="submit" disabled={saving}>{saving ? t('common.loading') : t('settings.deploy_keys.add', 'Add deploy key')}</button>
    </form>
  </section>

  <section>
    <h2>{t('settings.deploy_keys.current', 'Configured deploy keys')}</h2>
    {#if loading}
      <p>{t('common.loading')}</p>
    {:else if keys.length === 0}
      <div class="empty-state">{t('settings.deploy_keys.empty', 'No deploy keys configured.')}</div>
    {:else}
      <div class="key-list">
        {#each keys as key (key.id)}
          <article>
            <div>
              <h3>{key.title}</h3>
              <code>{key.fingerprint}</code>
              <span class:write={!key.read_only}>{key.read_only ? t('settings.deploy_keys.read_only', 'Read-only access') : t('settings.deploy_keys.read_write', 'Read/write access')}</span>
            </div>
            <button class="btn btn-danger" type="button" disabled={deletingId === key.id} onclick={() => removeKey(key)}>{t('common.delete', 'Delete')}</button>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .deploy-keys-page { max-width: 880px; }
  header, section { margin-bottom: 28px; }
  h1 { margin-bottom: 6px; }
  header p { color: var(--text-secondary); }
  form { display: grid; gap: 9px; }
  input, textarea { width: 100%; padding: 8px 10px; }
  textarea, code { font-family: var(--font-mono, monospace); }
  .checkbox { display: flex; align-items: center; gap: 8px; }
  .checkbox input { width: auto; }
  .message, .empty-state { margin-bottom: 18px; padding: 12px 14px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error { color: var(--red); }
  .success { color: var(--green); }
  .key-list { display: grid; gap: 12px; }
  article { display: flex; justify-content: space-between; gap: 16px; align-items: center; padding: 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  article h3 { margin: 0 0 8px; }
  article code { display: block; overflow-wrap: anywhere; color: var(--text-secondary); }
  article span { display: inline-block; margin-top: 8px; color: var(--text-secondary); }
  article span.write { color: var(--orange, #c56a00); }
</style>
