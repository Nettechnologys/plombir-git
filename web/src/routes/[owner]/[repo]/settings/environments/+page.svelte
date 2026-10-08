<script lang="ts">
  import { page } from '$app/stores';
  import { allowedUserLabel, ciEnvironments, parseStringList, type CiEnvironment } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';

  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<CiEnvironment[]>([]); let name = $state('production'); let isProtected = $state(true);
  let required = $state(1); let approvers = $state(''); let error = $state(''); let editingId = $state<number | null>(null); let saving = $state(false);
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
    resetForm();
    void load(expectedOwner, expectedRepo);
  });

  function rowKey(id: number | null, environmentName = name.trim()) {
    return id === null ? `new:${environmentName}` : `id:${id}`;
  }
  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }
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
      const next = await ciEnvironments.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) { items = next; error = ''; }
    } catch (e: any) {
      if (listRequests.owns(claim, owner, repo)) error = e.message;
    }
  }
  function payload() { return { name: name.trim(), protected: isProtected, required_approvals: required, allowed_approvers: parseStringList(approvers) }; }
  function resetForm() { editingId = null; name = ''; isProtected = true; required = 1; approvers = ''; }
  function edit(item: CiEnvironment) { editingId = item.id; name = item.name; isProtected = item.protected; required = item.required_approvals; approvers = (item.allowed_approvers ?? []).map(allowedUserLabel).join(', '); }
  async function save(event: SubmitEvent) {
    event.preventDefault();
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedEditingId = editingId;
    const key = rowKey(expectedEditingId);
    if (!claimMutation(key)) return;
    try {
      saving = true;
      error = '';
      if (expectedEditingId === null) await ciEnvironments.create(expectedOwner, expectedRepo, payload());
      else await ciEnvironments.update(expectedOwner, expectedRepo, expectedEditingId, payload());
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      resetForm();
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
  async function remove(item: CiEnvironment) {
    if (!confirm(`Delete environment ${item.name}?`)) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(item.id);
    if (!claimMutation(key)) return;
    try {
      await ciEnvironments.delete(expectedOwner, expectedRepo, item.id);
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) await load(expectedOwner, expectedRepo);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>
<svelte:head><title>Environments · {owner}/{repo}</title></svelte:head>
<div class="settings-page">
  <header><h1>Deployment environments</h1><p>Require designated reviewers before jobs can deploy to protected environments.</p></header>
  {#if error}<div class="message" role="alert">{error}</div>{/if}
  <section><h2>{editingId === null ? 'Create environment' : 'Edit environment'}</h2><form onsubmit={save}>
    <label for="environment-name">Name</label><input id="environment-name" bind:value={name} maxlength="255" disabled={saving} required />
    <label class="check"><input type="checkbox" bind:checked={isProtected} disabled={saving} /> Require approval</label>
    <label for="required-approvals">Required approvals</label><input id="required-approvals" type="number" min="1" max="10" bind:value={required} disabled={saving} required />
    <label for="approvers">Allowed approvers <span>(comma-separated usernames; empty means repository admins)</span></label><input id="approvers" bind:value={approvers} placeholder="alice, bob" disabled={saving} />
    <div class="actions"><button class="btn btn-primary" disabled={saving} aria-busy={saving}>{editingId === null ? 'Create environment' : 'Save changes'}</button>{#if editingId !== null}<button type="button" class="btn" disabled={saving} onclick={resetForm}>Cancel</button>{/if}</div>
  </form></section>
  <section><h2>Configured environments</h2>{#if items.length === 0}<p>No environments configured.</p>{:else}<div class="list">{#each items as item (item.id)}<article><div><strong>{item.name}</strong><p>{item.protected ? `${item.required_approvals} approval(s) required` : 'Unprotected'}{(item.allowed_approvers ?? []).length ? ` · reviewers: ${(item.allowed_approvers ?? []).map(allowedUserLabel).join(', ')}` : ''}</p></div><div class="actions"><button class="btn" disabled={isBusy(rowKey(item.id))} onclick={() => edit(item)}>Edit</button><button class="btn btn-danger" disabled={isBusy(rowKey(item.id))} aria-busy={isBusy(rowKey(item.id))} onclick={() => remove(item)}>Delete</button></div></article>{/each}</div>{/if}</section>
</div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,article p,label span{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.check,.actions{display:flex;align-items:center;gap:8px}.check input{width:auto}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}article p{margin:4px 0 0;font-size:13px}</style>
