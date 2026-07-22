<script lang="ts">
  import { page } from '$app/stores';
  import { ciRetention } from '$lib/api/client.svelte';
  const owner = $derived($page.params.owner!); const repo = $derived($page.params.repo!);
  let artifactDays = $state(30); let cacheDays = $state(7); let loading = $state(true);
  let error = $state(''); let message = $state('');
  $effect(() => { owner; repo; load(); });
  async function load() { try { loading = true; const policy = await ciRetention.get(owner, repo); artifactDays = policy.artifact_retention_days; cacheDays = policy.cache_retention_days; error = ''; } catch (e: any) { error = e.message; } finally { loading = false; } }
  async function save(event: SubmitEvent) { event.preventDefault(); try { await ciRetention.update(owner, repo, { artifact_retention_days: artifactDays, cache_retention_days: cacheDays }); message = 'Retention policy saved. New uploads use the updated lifetime.'; error = ''; } catch (e: any) { error = e.message; } }
  async function cleanup() { try { const result = await ciRetention.cleanup(owner, repo); message = `Deleted ${result.artifacts_deleted} artifact(s) and ${result.caches_deleted} cache entry(s).${result.failures ? ` ${result.failures} item(s) could not be safely removed.` : ''}`; error = ''; } catch (e: any) { error = e.message; } }
</script>
<svelte:head><title>CI retention · {owner}/{repo}</title></svelte:head>
<div class="settings-page"><header><h1>CI retention</h1><p>Control how long newly uploaded artifacts and accessed caches remain available. Expired storage is reclaimed hourly.</p></header>
{#if error}<div class="message error" role="alert">{error}</div>{/if}{#if message}<div class="message" role="status">{message}</div>{/if}
{#if loading}<p>Loading…</p>{:else}<form onsubmit={save}><label for="artifact-days">Artifact retention (days)</label><input id="artifact-days" type="number" min="1" max="3650" bind:value={artifactDays} required /><label for="cache-days">Cache retention after last access (days)</label><input id="cache-days" type="number" min="1" max="3650" bind:value={cacheDays} required /><div class="actions"><button class="btn btn-primary">Save policy</button><button type="button" class="btn" onclick={cleanup}>Clean expired storage now</button></div></form>{/if}</div>
<style>.settings-page{max-width:760px}header{margin-bottom:24px}header p{color:var(--text-secondary)}form{display:grid;gap:10px;max-width:520px}input{padding:8px 10px}.actions{display:flex;gap:8px;margin-top:8px}.message{padding:12px;margin-bottom:16px;border:1px solid var(--border);border-radius:var(--radius)}.error{color:var(--red)}</style>
