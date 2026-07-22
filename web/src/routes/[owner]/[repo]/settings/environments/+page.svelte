<script lang="ts">
  import { page } from '$app/stores';
  import { ciEnvironments, type CiEnvironment } from '$lib/api/client.svelte';
  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<CiEnvironment[]>([]); let name = $state('production'); let isProtected = $state(true);
  let required = $state(1); let approverIds = $state(''); let error = $state(''); let editingId = $state<number | null>(null);
  $effect(() => { owner; repo; load(); });
  async function load() { try { items = await ciEnvironments.list(owner, repo); error = ''; } catch (e: any) { error = e.message; } }
  function payload() { return { name: name.trim(), protected: isProtected, required_approvals: required, allowed_approver_ids: approverIds.split(',').map(v => Number(v.trim())).filter(Number.isSafeInteger) }; }
  function resetForm() { editingId = null; name = ''; isProtected = true; required = 1; approverIds = ''; }
  function edit(item: CiEnvironment) { editingId = item.id; name = item.name; isProtected = item.protected; required = item.required_approvals; approverIds = item.allowed_approver_ids.join(', '); }
  async function save(event: SubmitEvent) { event.preventDefault(); try { if (editingId === null) await ciEnvironments.create(owner, repo, payload()); else await ciEnvironments.update(owner, repo, editingId, payload()); resetForm(); await load(); } catch (e: any) { error = e.message; } }
  async function remove(item: CiEnvironment) { if (!confirm(`Delete environment ${item.name}?`)) return; try { await ciEnvironments.delete(owner, repo, item.id); await load(); } catch (e: any) { error = e.message; } }
</script>
<svelte:head><title>Environments · {owner}/{repo}</title></svelte:head>
<div class="settings-page">
  <header><h1>Deployment environments</h1><p>Require designated reviewers before jobs can deploy to protected environments.</p></header>
  {#if error}<div class="message" role="alert">{error}</div>{/if}
  <section><h2>{editingId === null ? 'Create environment' : 'Edit environment'}</h2><form onsubmit={save}>
    <label for="environment-name">Name</label><input id="environment-name" bind:value={name} maxlength="255" required />
    <label class="check"><input type="checkbox" bind:checked={isProtected} /> Require approval</label>
    <label for="required-approvals">Required approvals</label><input id="required-approvals" type="number" min="1" max="10" bind:value={required} required />
    <label for="approver-ids">Allowed approver user IDs <span>(comma-separated; empty means repository admins)</span></label><input id="approver-ids" bind:value={approverIds} placeholder="12, 34" />
    <div class="actions"><button class="btn btn-primary">{editingId === null ? 'Create environment' : 'Save changes'}</button>{#if editingId !== null}<button type="button" class="btn" onclick={resetForm}>Cancel</button>{/if}</div>
  </form></section>
  <section><h2>Configured environments</h2>{#if items.length === 0}<p>No environments configured.</p>{:else}<div class="list">{#each items as item (item.id)}<article><div><strong>{item.name}</strong><p>{item.protected ? `${item.required_approvals} approval(s) required` : 'Unprotected'}{item.allowed_approver_ids.length ? ` · reviewers: ${item.allowed_approver_ids.join(', ')}` : ''}</p></div><div class="actions"><button class="btn" onclick={() => edit(item)}>Edit</button><button class="btn btn-danger" onclick={() => remove(item)}>Delete</button></div></article>{/each}</div>{/if}</section>
</div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,article p,label span{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.check,.actions{display:flex;align-items:center;gap:8px}.check input{width:auto}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}article p{margin:4px 0 0;font-size:13px}</style>
