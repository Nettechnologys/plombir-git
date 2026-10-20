<script lang="ts">
  import { page } from '$app/stores';
  import ScopedVariables from '$lib/components/ScopedVariables.svelte';
  import { createT } from '$lib/i18n';
  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
</script>

<svelte:head><title>{t('settings.ci_variables.title')} · {owner}/{repo}</title></svelte:head>
<ScopedVariables scope={{ kind: 'repo', owner, repo }} />
