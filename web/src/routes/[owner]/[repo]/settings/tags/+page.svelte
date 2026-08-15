<script lang="ts">
  import { page } from '$app/stores';
  import { tagProtections, buildTagProtectionPayload, type TagProtection, type TagProtectionPayload } from '$lib/api/client.svelte';
  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<TagProtection[]>([]); let error = $state(''); let editingId = $state<number | null>(null);
  let form = $state({ pattern: '', allowed_user_ids: '' });
  $effect(() => { owner; repo; load(); });
  async function load() { try { items = await tagProtections.list(owner, repo); error = ''; } catch (e: any) { error = e.message; } }
  function resetForm() { editingId = null; form = { pattern: '', allowed_user_ids: '' }; }
  function edit(item: TagProtection) { editingId = item.id; form = { pattern: item.pattern, allowed_user_ids: item.allowed_user_ids.join(', ') }; }
  async function save(event: SubmitEvent) {
    event.preventDefault();
    try {
      if (editingId === null) await tagProtections.create(owner, repo, buildTagProtectionPayload(form, true) as TagProtectionPayload & { pattern: string });
      else await tagProtections.update(owner, repo, editingId, buildTagProtectionPayload(form, false));
      resetForm(); await load();
    } catch (e: any) { error = e.message; }
  }
  async function remove(item: TagProtection) { if (!confirm(`Delete protection for ${item.pattern}?`)) return; try { await tagProtections.delete(owner, repo, item.id); if (editingId === item.id) resetForm(); await load(); } catch (e: any) { error = e.message; } }
</script>
<svelte:head><title>Tag protection · {owner}/{repo}</title></svelte:head>
<div class="settings-page">
  <header><h1>Tag protection</h1><p>Block tag creation and updates matching a pattern over HTTP and SSH. <code>*</code> is the only wildcard.</p></header>
  {#if error}<div class="message" role="alert">{error}</div>{/if}
  <section><h2>{editingId === null ? 'Protect a pattern' : 'Edit protection'}</h2><form onsubmit={save}>
    <label for="tag-pattern">Pattern <span>(use <code>*</code> as the wildcard; <code>?</code>, character classes, and <code>+</code> are not supported)</span></label>
    <input id="tag-pattern" bind:value={form.pattern} maxlength="255" placeholder="v*" readonly={editingId !== null} required />
    <label for="tag-allowed-ids">Allowed user IDs <span>(comma-separated; empty means the pattern is closed to everyone, including the owner)</span></label>
    <input id="tag-allowed-ids" bind:value={form.allowed_user_ids} placeholder="12, 34" />
    <div class="actions"><button class="btn btn-primary">{editingId === null ? 'Add protection' : 'Save changes'}</button>{#if editingId !== null}<button type="button" class="btn" onclick={resetForm}>Cancel</button>{/if}</div>
  </form></section>
  <section><h2>Protected patterns</h2>{#if items.length === 0}<p>No protected tag patterns.</p>{:else}<div class="list">{#each items as item (item.id)}<article><div><code>{item.pattern}</code><p>{item.allowed_user_ids.length ? `allowed: ${item.allowed_user_ids.join(', ')}` : 'nobody may push this pattern'}</p></div><div class="actions"><button class="btn" onclick={() => edit(item)}>Edit</button><button class="btn btn-danger" onclick={() => remove(item)}>Delete</button></div></article>{/each}</div>{/if}</section>
</div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p,article p,label span{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.actions{display:flex;align-items:center;gap:8px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}article p{margin:4px 0 0;font-size:13px}</style>
