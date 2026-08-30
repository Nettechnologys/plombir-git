<script lang="ts">
  import { page } from '$app/stores';
  import { deployKeys, type DeployKey } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
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
  let busyRows = $state<Set<string>>(new Set());
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    keys = [];
    loading = true;
    saving = false;
    busyRows = new Set();
    title = '';
    publicKey = '';
    readOnly = true;
    error = '';
    success = '';
    void loadKeys(expectedOwner, expectedRepo);
  });

  function rowKey(id: number | null, keyTitle = title.trim()): string {
    return id === null ? `new:${keyTitle}` : `id:${id}`;
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy(key: string): boolean {
    return busyRows.has(key);
  }

  function claimMutation(key: string): boolean {
    if (isBusy(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }

  function releaseMutation(key: string): void {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  async function loadKeys(expectedOwner: string, expectedRepo: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      error = '';
      const next = await deployKeys.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) {
        keys = next;
        error = '';
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.deploy_keys.load_failed', 'Failed to load deploy keys.');
      }
    } finally {
      if (listRequests.owns(claim, owner, repo)) loading = false;
    }
  }

  async function addKey(event: SubmitEvent) {
    event.preventDefault();
    if (!title.trim() || !publicKey.trim()) {
      error = t('settings.deploy_keys.required', 'Title and public key are required.');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedTitle = title.trim();
    const expectedPublicKey = publicKey.trim();
    const expectedReadOnly = readOnly;
    const key = rowKey(null, expectedTitle);
    if (!claimMutation(key)) return;
    try {
      saving = true;
      error = '';
      success = '';
      await deployKeys.create(
        expectedOwner,
        expectedRepo,
        expectedTitle,
        expectedPublicKey,
        expectedReadOnly,
      );
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      title = '';
      publicKey = '';
      readOnly = true;
      success = t('settings.deploy_keys.created', 'Deploy key added.');
      await loadKeys(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.deploy_keys.create_failed', 'Failed to add deploy key.');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        saving = false;
        releaseMutation(key);
      }
    }
  }

  async function removeKey(key: DeployKey) {
    if (!confirm(t('settings.deploy_keys.delete_confirm', { title: key.title }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const mutationKey = rowKey(key.id);
    if (!claimMutation(mutationKey)) return;
    try {
      deletingId = key.id;
      error = '';
      success = '';
      await deployKeys.delete(expectedOwner, expectedRepo, key.id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = t('settings.deploy_keys.deleted', 'Deploy key removed.');
      await loadKeys(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.deploy_keys.delete_failed', 'Failed to remove deploy key.');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        deletingId = null;
        releaseMutation(mutationKey);
      }
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
      <button class="btn btn-primary" type="submit" disabled={saving} aria-busy={saving}>{saving ? t('common.loading') : t('settings.deploy_keys.add', 'Add deploy key')}</button>
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
            <button
              class="btn btn-danger"
              type="button"
              disabled={isBusy(rowKey(key.id))}
              aria-busy={deletingId === key.id}
              onclick={() => removeKey(key)}
            >{t('common.delete', 'Delete')}</button>
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
