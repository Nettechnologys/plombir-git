<script lang="ts">
  import { onMount } from 'svelte';
  import { createT } from '$lib/i18n';

  const t = createT();
  let light = $state(false);
  onMount(() => { light = document.documentElement.dataset.theme === 'light'; });

  function toggle() {
    light = !light;
    const theme = light ? 'light' : 'dark';
    document.documentElement.dataset.theme = theme;
    document.documentElement.style.colorScheme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', light ? '#ffffff' : '#0d1117');
    try { localStorage.setItem('plombir_git_theme', theme); } catch { /* Storage is optional. */ }
  }
</script>

<button
  type="button"
  class="btn btn-sm"
  onclick={toggle}
  aria-label={t('nav.light_theme', 'Light theme')}
  aria-pressed={light}
  title={light ? t('nav.use_dark_theme', 'Use dark theme') : t('nav.use_light_theme', 'Use light theme')}
>{light ? '☀' : '☾'}</button>
