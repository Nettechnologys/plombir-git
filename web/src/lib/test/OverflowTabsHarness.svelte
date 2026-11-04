<script lang="ts">
  import OverflowTabs from '$lib/components/OverflowTabs.svelte';
  let locale = $state('en');
  let settings = $state(true);
  let count = $state(1);
  let active = $state('settings');
  const labels: Record<string, string[]> = {
    en: ['Code', 'Issues', 'Pull requests', 'Settings', 'More'],
    ru: ['Код', 'Задачи', 'Пул-реквесты', 'Настройки', 'Ещё'],
    'zh-CN': ['代码', '议题', '合并请求', '设置', '更多'],
  };
  const tabs = $derived([
    { id: 'code', label: labels[locale][0], href: '/code', icon: '📁' },
    { id: 'issues', label: labels[locale][1], href: '/issues', icon: '◉', count },
    { id: 'pulls', label: labels[locale][2], href: '/pulls', icon: '⑂' },
    ...(settings ? [{ id: 'settings', label: labels[locale][3], href: '/settings', icon: '⚙' }] : []),
  ]);
</script>

<OverflowTabs {tabs} activeTab={active} label="Repository" moreLabel={labels[locale][4]} moreActiveLabel={`More; current: ${tabs.find(tab => tab.id === active)?.label}`} />
<button onclick={() => locale = 'en'}>English</button>
<button onclick={() => locale = 'ru'}>Russian</button>
<button onclick={() => locale = 'zh-CN'}>Chinese</button>
<button onclick={() => settings = !settings}>Toggle settings permission</button>
<button onclick={() => count = 100000}>Increase count</button>
<button onclick={() => active = 'issues'}>Activate issues</button>
<button>Outside navigation</button>
