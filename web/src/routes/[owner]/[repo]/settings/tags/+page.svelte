<script lang="ts">
  import { page } from '$app/stores';
  import { allowedUserLabel, tagProtections, buildTagProtectionPayload, type TagProtection, type TagProtectionPayload } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';

  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<TagProtection[]>([]); let error = $state(''); let editingId = $state<number | null>(null);
  let form = $state({ pattern: '', allowed_users: '' });
  let saving = $state(false); let busyRows = $state<Set<string>>(new Set());
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
  function rowKey(id: number | null, pattern = form.pattern.trim()) {
    return id === null ? `new:${pattern}` : `id:${id}`;
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
      const next = await tagProtections.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) { items = next; error = ''; }
    } catch (e: any) {
      if (listRequests.owns(claim, owner, repo)) error = e.message;
    }
  }
  function resetForm() { editingId = null; form = { pattern: '', allowed_users: '' }; }
  function edit(item: TagProtection) { editingId = item.id; form = { pattern: item.pattern, allowed_users: (item.allowed_users ?? []).map(allowedUserLabel).join(', ') }; }
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
      if (expectedEditingId === null) await tagProtections.create(expectedOwner, expectedRepo, buildTagProtectionPayload(form, true) as TagProtectionPayload & { pattern: string });
      else await tagProtections.update(expectedOwner, expectedRepo, expectedEditingId, buildTagProtectionPayload(form, false));
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
  async function remove(item: TagProtection) {
    if (!confirm(`Delete protection for ${item.pattern}?`)) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(item.id);
    if (!claimMutation(key)) return;
    try {
      await tagProtections.delete(expectedOwner, expectedRepo, item.id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      if (editingId === item.id) resetForm();
      await load(expectedOwner, expectedRepo);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>
<svelte:head><title>Tag protection · {owner}/{repo}</title></svelte:head>
<div class="settings-page">
  <header><h1>Tag protection</h1><p>Block tag creation and updates matching a pattern over HTTP and SSH. <code>*</code> is the only wildcard.</p></header>
  {#if error}<div class="message" role="alert">{error}</div>{/if}
  <section><h2>{editingId === null ? 'Protect a pattern' : 'Edit protection'}</h2><form onsubmit={save}>
    <label for="tag-pattern">Pattern <span>(use <code>*</code> as the wildcard; <code>?</code>, character classes, and <code>+</code> are not supported)</span></label>
    <input id="tag-pattern" bind:value={form.pattern} maxlength="255" placeholder="v*" readonly={editingId !== null} disabled={saving} required />
    <label for="tag-allowed-users">Users allowed to push this pattern <span>(comma-separated usernames; empty means the pattern is closed to everyone, including the owner)</span></label>
    <input id="tag-allowed-users" bind:value={form.allowed_users} placeholder="alice, bob" disabled={saving} />
    <div class="actions"><button class="btn btn-primary" disabled={saving} aria-busy={saving}>{editingId === null ? 'Add protection' : 'Save changes'}</button>{#if editingId !== null}<button type="button" class="btn" disabled={saving} onclick={resetForm}>Cancel</button>{/if}</div>
  </form></section>
  <section><h2>Protected patterns</h2>{#if items.length === 0}<p>No protected tag patterns.</p>{:else}<div class="list">{#each items as item (item.id)}<article><div><code>{item.pattern}</code><p>{(item.allowed_users ?? []).length ? `allowed: ${(item.allowed_users ?? []).map(allowedUserLabel).join(', ')}` : 'nobody may push this pattern'}</p></div><div class="actions"><button class="btn" disabled={isBusy(rowKey(item.id))} onclick={() => edit(item)}>Edit</button><button class="btn btn-danger" disabled={isBusy(rowKey(item.id))} aria-busy={isBusy(rowKey(item.id))} onclick={() => remove(item)}>Delete</button></div></article>{/each}</div>{/if}</section>
</div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,article p,label span{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.actions{display:flex;align-items:center;gap:8px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}article p{margin:4px 0 0;font-size:13px}</style>
