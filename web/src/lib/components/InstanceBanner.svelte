<script lang="ts">
  import { getBanner, clearBanner } from '$lib/stores/instance.svelte';

  let banner = $derived(getBanner());
</script>

{#if banner.message}
  <div class="banner" class:info={banner.type === 'info'} class:warning={banner.type === 'warning'} class:error={banner.type === 'error'}>
    <span class="banner-icon">
      {#if banner.type === 'warning'}⚠️{:else if banner.type === 'error'}🚫{:else}ℹ️{/if}
    </span>
    <span class="banner-text">{banner.message}</span>
    <button class="banner-close" onclick={clearBanner}>✕</button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    font-size: 13px;
    font-weight: 500;
    border-bottom: 1px solid;
    z-index: 50;
  }
  .info { background: rgba(46, 164, 255, 0.15); color: #6db7ff; border-bottom-color: rgba(88, 166, 255, 0.4); }
  .warning { background: rgba(255, 211, 88, 0.12); color: #f2cc60; border-bottom-color: rgba(210, 153, 34, 0.5); }
  .error { background: rgba(248, 81, 73, 0.15); color: var(--red, #f85149); border-bottom-color: rgba(248, 81, 73, 0.45); }
  .banner-text { flex: 1; }
  .banner-close {
    background: none; border: none; cursor: pointer;
    font-size: 14px; opacity: 0.6; padding: 2px 6px; border-radius: 3px;
    color: inherit;
  }
  .banner-close:hover { opacity: 1; background: rgba(0,0,0,0.05); }
</style>
