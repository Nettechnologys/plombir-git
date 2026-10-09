<script lang="ts">
  import { page } from '$app/stores';
  import { ciEnvironments, ciSecrets, type CiEnvironment, type CiSecret } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';

  const t = createT();

  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<CiSecret[]>([]); let environments = $state<CiEnvironment[]>([]); let name = $state(''); let value = $state(''); let environment = $state(''); let error = $state(''); let saving = $state(false);
  let busyRows = $state<Set<string>>(new Set());
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    items = [];
    environments = [];
    error = '';
    saving = false;
    busyRows = new Set();
    void load(expectedOwner, expectedRepo);
  });

  function rowKey(secretName: string, scope: string | null) { return `secret:${scope ?? ''}:${secretName.toUpperCase()}`; }
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
      const [next, nextEnvironments] = await Promise.all([
        ciSecrets.list(expectedOwner, expectedRepo),
        ciEnvironments.list(expectedOwner, expectedRepo).catch(() => [] as CiEnvironment[]),
      ]);
      if (listRequests.owns(claim, owner, repo)) { items = next; environments = nextEnvironments ?? []; error = ''; }
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
    const scope = environment || null;
    const key = rowKey(secretName, scope);
    if (!claimMutation(key)) return;
    try {
      saving = true;
      error = '';
      await ciSecrets.put(expectedOwner, expectedRepo, secretName, value, scope);
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
    if (!confirm(item.environment
      ? t('settings.ci_secrets.delete_confirm_scoped', { name: item.name, environment: item.environment })
      : t('settings.ci_secrets.delete_confirm', { name: item.name }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(item.name, item.environment);
    if (!claimMutation(key)) return;
    try {
      await ciSecrets.delete(expectedOwner, expectedRepo, item.name, item.environment);
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) await load(expectedOwner, expectedRepo);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>
<svelte:head><title>{t('settings.ci_secrets.title')} · {owner}/{repo}</title></svelte:head>
<div class="settings-page"><header><h1>{t('settings.ci_secrets.title')}</h1><p>{t('settings.ci_secrets.desc')}</p></header>{#if error}<div class="message" role="alert">{error}</div>{/if}<section><h2>{t('settings.ci_secrets.add_title')}</h2><form onsubmit={save}><label for="secret-name">{t('settings.ci_secrets.name')}</label><input id="secret-name" bind:value={name} pattern="[A-Z_][A-Z0-9_]*" maxlength="100" placeholder="DEPLOY_TOKEN" disabled={saving} required /><label for="secret-environment">{t('settings.ci_secrets.environment')}</label><select id="secret-environment" bind:value={environment} disabled={saving}><option value="">{t('settings.ci_secrets.environment_repository_wide')}</option>{#each environments as environmentOption (environmentOption.id)}<option value={environmentOption.name}>{environmentOption.name}</option>{/each}</select><label for="secret-value">{t('settings.ci_secrets.value')}</label><input id="secret-value" type="password" bind:value={value} minlength="4" maxlength="65536" disabled={saving} required /><button class="btn btn-primary" disabled={saving} aria-busy={saving}>{t('settings.ci_secrets.save')}</button></form></section><section><h2>{t('settings.ci_secrets.list_title')}</h2>{#if items.length === 0}<p>{t('settings.ci_secrets.empty')}</p>{:else}<div class="list">{#each items as item (`${item.environment ?? ''}:${item.name}`)}<article><div><strong>{item.name}</strong>{#if item.environment}<span class="scope">{t('settings.ci_secrets.environment_scoped', { environment: item.environment })}</span>{/if}<small>{t('common.updated', { date: formatDateTime(item.updated_at) })}</small></div><button class="btn btn-danger" disabled={isBusy(rowKey(item.name, item.environment))} aria-busy={isBusy(rowKey(item.name, item.environment))} onclick={() => remove(item)}>{t('common.delete')}</button></article>{/each}</div>{/if}</section></div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,small{color:var(--text-secondary)}form{display:grid;gap:9px}input,select{padding:8px 10px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}small{display:block;margin-top:5px}.scope{margin-left:8px;padding:1px 6px;border:1px solid var(--border);border-radius:var(--radius);color:var(--text-secondary);font-size:12px}</style>
