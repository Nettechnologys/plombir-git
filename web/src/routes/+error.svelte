<script lang="ts">
  import { page } from '$app/state';
  import PageError from '$lib/components/PageError.svelte';
</script>

<PageError status={page.status} />
