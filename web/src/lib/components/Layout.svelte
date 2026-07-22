<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    children: Snippet;
  }

  let { children }: Props = $props();
</script>

<div class="page-layout">
  <div class="page-layout__inner">
    {@render children()}
  </div>
</div>

<style>
  .page-layout {
    width: 100%;
    padding: 0;
  }

  .page-layout__inner {
    max-width: 100%;
    margin: 0 auto;
  }

  .page-layout :global(main) {
    min-height: calc(100vh - 62px);
    padding-top: 0;
    padding-bottom: 0;
  }

  .page-layout :global(main > .loading) {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 160px;
    color: var(--text-secondary);
  }

  @media (max-width: 720px) {
    .page-layout :global(main) {
      min-height: calc(100vh - 170px);
    }
  }
</style>
