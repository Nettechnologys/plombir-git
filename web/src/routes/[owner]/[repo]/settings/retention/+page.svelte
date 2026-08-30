<script lang="ts">
  import { page } from '$app/stores';
  import { ciRetention, type CiRetentionPolicy } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  let artifactDays = $state(30);
  let cacheDays = $state(7);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let message = $state('');
  const policyRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    artifactDays = 30;
    cacheDays = 7;
    loading = true;
    busy = false;
    error = '';
    message = '';
    void load(expectedOwner, expectedRepo, routeGeneration);
  });

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function fillPolicy(policy: CiRetentionPolicy) {
    artifactDays = policy.artifact_retention_days;
    cacheDays = policy.cache_retention_days;
  }

  async function load(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = policyRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      const policy = await ciRetention.get(expectedOwner, expectedRepo);
      if (policyRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        fillPolicy(policy);
        error = '';
      }
    } catch (e: any) {
      if (policyRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = e.message;
      }
    } finally {
      if (policyRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        loading = false;
      }
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = policyRequests.begin(expectedOwner, expectedRepo);
    const policy = {
      artifact_retention_days: artifactDays,
      cache_retention_days: cacheDays,
    };
    try {
      busy = true;
      error = '';
      message = '';
      const updated = await ciRetention.update(expectedOwner, expectedRepo, policy);
      if (policyRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        fillPolicy(updated);
        message = 'Retention policy saved. New uploads use the updated lifetime.';
        error = '';
      }
    } catch (e: any) {
      if (policyRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = e.message;
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) busy = false;
    }
  }

  async function cleanup() {
    if (busy) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    try {
      busy = true;
      error = '';
      message = '';
      const result = await ciRetention.cleanup(expectedOwner, expectedRepo);
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        message = `Deleted ${result.artifacts_deleted} artifact(s) and ${result.caches_deleted} cache entry(s).${result.failures ? ` ${result.failures} item(s) could not be safely removed.` : ''}`;
        error = '';
      }
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) busy = false;
    }
  }
</script>

<svelte:head><title>CI retention · {owner}/{repo}</title></svelte:head>
<div class="settings-page">
  <header><h1>CI retention</h1><p>Control how long newly uploaded artifacts and accessed caches remain available. Expired storage is reclaimed hourly.</p></header>
  {#if error}<div class="message error" role="alert">{error}</div>{/if}
  {#if message}<div class="message" role="status">{message}</div>{/if}
  {#if loading}
    <p>Loading…</p>
  {:else}
    <form onsubmit={save}>
      <label for="artifact-days">Artifact retention (days)</label>
      <input id="artifact-days" type="number" min="1" max="3650" bind:value={artifactDays} disabled={busy} required />
      <label for="cache-days">Cache retention after last access (days)</label>
      <input id="cache-days" type="number" min="1" max="3650" bind:value={cacheDays} disabled={busy} required />
      <div class="actions">
        <button class="btn btn-primary" disabled={busy} aria-busy={busy}>Save policy</button>
        <button type="button" class="btn" onclick={cleanup} disabled={busy} aria-busy={busy}>Clean expired storage now</button>
      </div>
    </form>
  {/if}
</div>
<style>.settings-page{max-width:760px}header{margin-bottom:24px}header p{color:var(--text-secondary)}form{display:grid;gap:10px;max-width:520px}input{padding:8px 10px}.actions{display:flex;gap:8px;margin-top:8px}.message{padding:12px;margin-bottom:16px;border:1px solid var(--border);border-radius:var(--radius)}.error{color:var(--red)}</style>
