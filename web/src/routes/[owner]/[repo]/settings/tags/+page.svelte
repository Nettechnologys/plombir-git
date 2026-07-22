<script lang="ts">
  import { page } from '$app/stores';
  import { tagProtections, type TagProtection } from '$lib/api/client.svelte';
  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let items = $state<TagProtection[]>([]); let pattern = $state(''); let error = $state('');
  $effect(() => { owner; repo; load(); });
  async function load() { try { items = await tagProtections.list(owner, repo); error = ''; } catch (e: any) { error = e.message; } }
  async function create(event: SubmitEvent) { event.preventDefault(); try { await tagProtections.create(owner, repo, pattern.trim()); pattern = ''; await load(); } catch (e: any) { error = e.message; } }
  async function remove(item: TagProtection) { if (!confirm(`Delete protection for ${item.pattern}?`)) return; try { await tagProtections.delete(owner, repo, item.id); await load(); } catch (e: any) { error = e.message; } }
</script>
<svelte:head><title>Tag protection · {owner}/{repo}</title></svelte:head>
<div class="settings-page"><header><h1>Tag protection</h1><p>Block tag creation and updates matching a wildcard pattern over HTTP and SSH.</p></header>{#if error}<div class="message" role="alert">{error}</div>{/if}<section><h2>Protect a pattern</h2><form onsubmit={create}><label for="tag-pattern">Pattern</label><input id="tag-pattern" bind:value={pattern} maxlength="255" placeholder="v*" required /><button class="btn btn-primary">Add protection</button></form></section><section><h2>Protected patterns</h2>{#if items.length === 0}<p>No protected tag patterns.</p>{:else}<div class="list">{#each items as item (item.id)}<article><code>{item.pattern}</code><button class="btn btn-danger" onclick={() => remove(item)}>Delete</button></article>{/each}</div>{/if}</section></div>
<style>.settings-page{max-width:880px}header,section{margin-bottom:28px}header p{color:var(--text-secondary)}form{display:grid;gap:9px}input{padding:8px 10px}.message{color:var(--red);padding:12px;border:1px solid var(--border);border-radius:var(--radius)}.list{display:grid;gap:10px}article{display:flex;align-items:center;justify-content:space-between;padding:14px;border:1px solid var(--border);border-radius:var(--radius)}</style>
