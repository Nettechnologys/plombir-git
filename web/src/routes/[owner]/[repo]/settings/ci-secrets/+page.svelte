<script lang="ts">
  import { page } from '$app/stores';
  import { ciSecrets, type CiSecret } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<CiSecret[]>([]); let name = $state(''); let value = $state(''); let error = $state(''); let saving = $state(false);
  let busyRows = $state<Set<string>>(new Set());
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    items = [];
    error = '';
    saving = false;
    busyRows = new Set();
    void load(expectedOwner, expectedRepo);
  });

  function rowKey(secretName: string) { return `secret:${secretName.toUpperCase()}`; }
  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) { return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo; }
  function isBusy(key: string) { return busyRows.has(key); }
  function claimMutation(key: string) {
    if (isBusy(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }
  function releaseMutation(key: string) {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  async function load(expectedOwner: string, expectedRepo: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      const next = await ciSecrets.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) { items = next; error = ''; }
    } catch (e: any) {
      if (listRequests.owns(claim, owner, repo)) error = e.message;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const secretName = name.trim().toUpperCase();
    const key = rowKey(secretName);
    if (!claimMutation(key)) return;
    try {
      saving = true;
      error = '';
      await ciSecrets.put(expectedOwner, expectedRepo, secretName, value);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      name = '';
      value = '';
      await load(expectedOwner, expectedRepo);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        saving = false;
        releaseMutation(key);
      }
    }
  }

  async function remove(item: CiSecret) {
    if (!(await confirmer.ask({
      title: t('settings.ci_secrets.delete_confirm_title'),
      message: t('settings.ci_secrets.delete_confirm', { name: item.name }),
      confirmLabel: t('common.delete'),
    }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(item.name);
    if (!claimMutation(key)) return;
    try {
      await ciSecrets.delete(expectedOwner, expectedRepo, item.name);
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) await load(expectedOwner, expectedRepo);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>
<svelte:head><title>{t('settings.ci_secrets.title')} · {owner}/{repo}</title></svelte:head>
<div class="settings-page"><header><h1>{t('settings.ci_secrets.title')}</h1><p>{t('settings.ci_secrets.desc')}</p></header>{#if error}<div class="message" role="alert">{error}</div>{/if}<section><h2>{t('settings.ci_secrets.add_title')}</h2><form onsubmit={save}><label for="secret-name">{t('settings.ci_secrets.name')}</label><input id="secret-name" bind:value={name} pattern="[A-Z_][A-Z0-9_]*" maxlength="100" placeholder="DEPLOY_TOKEN" disabled={saving} required /><label for="secret-value">{t('settings.ci_secrets.value')}</label><input id="secret-value" type="password" bind:value={value} minlength="4" maxlength="65536" disabled={saving} required /><button class="btn btn-primary" disabled={saving} aria-busy={saving}>{t('settings.ci_secrets.save')}</button></form></section><section><h2>{t('settings.ci_secrets.list_title')}</h2>{#if items.length === 0}<p>{t('settings.ci_secrets.empty')}</p>{:else}<div class="list">{#each items as item (item.name)}<article><div><strong>{item.name}</strong><small>{t('common.updated', { date: formatDateTime(item.updated_at) })}</small></div><button class="btn btn-danger" disabled={isBusy(rowKey(item.name))} aria-busy={isBusy(rowKey(item.name))} onclick={() => remove(item)}>{t('common.delete')}</button></article>{/each}</div>{/if}</section></div>

<ConfirmModal {confirmer} />

<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,small{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}small{display:block;margin-top:5px}</style>
