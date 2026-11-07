<script lang="ts">
  import { page } from '$app/stores';
  import PageError from '$lib/components/PageError.svelte';
</script>

<PageError status={$page.status} />
