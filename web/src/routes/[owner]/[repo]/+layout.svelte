<script lang="ts">
  import { page } from '$app/stores';
  import { untrack, type Snippet } from 'svelte';
  import { repos, ApiError } from '$lib/api/client.svelte';
  import { getUser, isAuthReady } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';
  import PageError from '$lib/components/PageError.svelte';

  let { children }: { children: Snippet } = $props();
  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  const identity = $derived(JSON.stringify([
    owner, repo, $page.url.pathname, $page.url.search, getUser()?.id ?? null, isAuthReady(),
  ]));
  let result = $state<{ identity: string; status: number } | null>(null);
  let attempt = $state(0);
  const status = $derived(result?.identity === identity ? result.status : null);

  $effect(() => {
    const expected = identity;
    const expectedOwner = owner;
    const expectedRepo = repo;
    attempt;
    result = null;
    if (!isAuthReady()) return;
    let current = true;
    // Sudo replaces the bearer for the same account. Reading it inside the
    // request must not reset forms; route/account changes above do reset them.
    untrack(() => {
      void repos.get(expectedOwner, expectedRepo).then(() => {
        if (current && identity === expected) result = { identity: expected, status: 200 };
      }).catch((error: unknown) => {
        if (!current || identity !== expected) return;
        // Also accept the older server's denial codes while upgrading. Never
        // render its raw message, or a header/clone link derived from the URL.
        const hidden = error instanceof ApiError && [401, 403, 404].includes(error.status);
        result = { identity: expected, status: hidden ? 404 : 503 };
      });
    });
    return () => { current = false; };
  });
</script>

<svelte:head>
  {#if status === null}<title>{t('common.loading')} · Plombir Git</title>{/if}
</svelte:head>

{#if status === 200}
  {@render children()}
{:else if status !== null}
  <PageError {status} />
  {#if status !== 404}
    <div class="retry"><button class="btn btn-outline" type="button" onclick={() => attempt++}>{t('common.retry')}</button></div>
  {/if}
{:else}
  <p class="loading" role="status">{t('common.loading')}</p>
{/if}

<style>
  .loading, .retry { text-align: center; margin: 48px var(--layout-gutter); }
</style>
