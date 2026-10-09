<script lang="ts">
  import '$lib/app.css';
  import Navbar from '$lib/components/Navbar.svelte';
  import InstanceBanner from '$lib/components/InstanceBanner.svelte';
  import SessionStatusBanner from '$lib/components/SessionStatusBanner.svelte';
  import SudoPrompt from '$lib/components/SudoPrompt.svelte';
  import Layout from '$lib/components/Layout.svelte';
  import SourceFooter from '$lib/components/SourceFooter.svelte';
import { fetchUser, isAuthReady } from '$lib/stores/auth.svelte';
import { registerKeyboardShortcuts } from '$lib/stores/instance.svelte';
import { locale, t } from '$lib/i18n';
import { onMount } from 'svelte';
import type { Snippet } from 'svelte';
import { setBanner, setRegistrationOpen, setSourceLink } from '$lib/stores/instance.svelte';
import { instance } from '$lib/api/client.svelte';
import { withBackendBase } from '$lib/api/_base';

  interface Props {
    children: Snippet;
  }

  let { children }: Props = $props();

  // Initialize i18n and fetch user on first load
  locale.init();
  fetchUser();

  // Register global keyboard shortcuts
  onMount(() => {
    const unregister = registerKeyboardShortcuts();
    checkBackendReadiness();
    return unregister;
  });

  async function checkBackendReadiness() {
    try {
      const res = await fetch(withBackendBase('/health'), { cache: 'no-store' });
      if (!res.ok) {
        setBanner(t('errors.backend_health_failed', { status: res.status }), 'error');
        return;
      }
      const body = await res.json().catch(() => null);
      if (!body || !['healthy', 'ok'].includes(String(body.status || ''))) {
        setBanner(t('errors.backend_unhealthy'), 'warning');
        return;
      }
    } catch {
      setBanner(t('errors.backend_unreachable'), 'error');
      return;
    }
    await loadInstanceBanner();
  }

  // Only after the health check has passed, and only on its success path: the
  // banners above are about this browser failing to reach the backend, and an
  // operator's announcement must not overwrite that diagnosis.
  //
  // Without this call the operator's banner had no reader at all — it lived
  // behind `/admin/settings`, which answers 403 to everyone else and is not
  // fetched on any other page (card_801b8bcdb880).
  async function loadInstanceBanner() {
    try {
      const info = await instance.get();
      // The source offer (AGPL §13) rides on the same answer: one request, and
      // the link exists only once the server has said where its source is.
      setSourceLink({ url: info.source_url, commit: info.source_commit });
      setRegistrationOpen(info.registration_open ?? null);
      if (info.banner_message) {
        setBanner(info.banner_message, info.banner_type ?? 'info');
      }
    } catch {
      // The instance has nothing to announce, or could not say so. Either way
      // this is not the browser's problem to report — the health check above is
      // what speaks about reachability.
    }
  }
</script>

<div class="app">
  <InstanceBanner />
  <SessionStatusBanner />
  <Navbar />
  <Layout>
    <main>
      {#if isAuthReady()}
        {@render children()}
      {:else}
        <div class="loading">{t('common.loading')}</div>
      {/if}
    </main>
  </Layout>
  <SourceFooter />
  <SudoPrompt />
</div>

<style>
  .app {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }

  main {
    flex: 1;
  }
</style>
