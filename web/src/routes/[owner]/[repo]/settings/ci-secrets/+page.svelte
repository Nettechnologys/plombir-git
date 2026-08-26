<script lang="ts">
  import { page } from '$app/stores';
  import { ciSecrets, type CiSecret } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';

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
    if (!confirm(`Delete ${item.name}?`)) return;
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
<svelte:head><title>CI secrets · {owner}/{repo}</title></svelte:head>
<div class="settings-page"><header><h1>CI secrets</h1><p>Encrypted repository secrets are injected into jobs and masked from stored logs.</p></header>{#if error}<div class="message" role="alert">{error}</div>{/if}<section><h2>Add or replace a secret</h2><form onsubmit={save}><label for="secret-name">Name</label><input id="secret-name" bind:value={name} pattern="[A-Z_][A-Z0-9_]*" maxlength="100" placeholder="DEPLOY_TOKEN" disabled={saving} required /><label for="secret-value">Value</label><input id="secret-value" type="password" bind:value={value} minlength="4" maxlength="65536" disabled={saving} required /><button class="btn btn-primary" disabled={saving} aria-busy={saving}>Save secret</button></form></section><section><h2>Configured secrets</h2>{#if items.length === 0}<p>No secrets configured.</p>{:else}<div class="list">{#each items as item (item.name)}<article><div><strong>{item.name}</strong><small>Updated {new Date(item.updated_at).toLocaleString()}</small></div><button class="btn btn-danger" disabled={isBusy(rowKey(item.name))} aria-busy={isBusy(rowKey(item.name))} onclick={() => remove(item)}>Delete</button></article>{/each}</div>{/if}</section></div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,small{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}small{display:block;margin-top:5px}</style>
