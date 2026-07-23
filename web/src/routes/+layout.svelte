<script lang="ts">
  import '$lib/app.css';
  import Navbar from '$lib/components/Navbar.svelte';
  import InstanceBanner from '$lib/components/InstanceBanner.svelte';
  import Layout from '$lib/components/Layout.svelte';
import { fetchUser, isAuthReady } from '$lib/stores/auth.svelte';
import { registerKeyboardShortcuts } from '$lib/stores/instance.svelte';
import { locale, t } from '$lib/i18n';
import { onMount } from 'svelte';
import type { Snippet } from 'svelte';
import { setBanner } from '$lib/stores/instance.svelte';
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
    }
  }
</script>

<div class="app">
  <InstanceBanner />
  <Navbar />
  <Layout>
    <main>
      {#if isAuthReady()}
        {@render children()}
      {:else}
        <div class="loading">Loading...</div>
      {/if}
    </main>
  </Layout>
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
