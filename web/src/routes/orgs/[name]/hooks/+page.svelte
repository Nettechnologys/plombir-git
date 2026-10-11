<script lang="ts">
  import { page } from '$app/stores';
  import ScopedWebhooks from '$lib/components/ScopedWebhooks.svelte';
  import { createT } from '$lib/i18n';
  const t = createT();
  const name = $derived($page.params.name!);
</script>

<svelte:head><title>{t('settings.webhooks.org_title', { name })} · Plombir Git</title></svelte:head>
<ScopedWebhooks scope="org" {name} />
