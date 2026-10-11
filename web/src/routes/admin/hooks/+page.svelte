<script lang="ts">
  import ScopedWebhooks from '$lib/components/ScopedWebhooks.svelte';
  import { createT } from '$lib/i18n';
  const t = createT();
</script>

<svelte:head><title>{t('settings.webhooks.system_title')} · Plombir Git</title></svelte:head>
<ScopedWebhooks scope="system" />
