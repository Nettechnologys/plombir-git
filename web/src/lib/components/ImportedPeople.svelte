<script lang="ts">
  // Who an imported issue or pull request named on its source: assigned there,
  // or asked there for a review (card_a923b9785660). Logins on another
  // platform, not accounts here — nothing links, and the local assignee beside
  // it stays a separate thing. Renders nothing when the source named nobody.
  import type { ImportedPerson } from '$lib/api/issues';
  import { t } from '$lib/i18n';
  import { importedPlatform } from '$lib/importedPlatform';

  let { people = [], role }: { people?: ImportedPerson[] | null; role: 'assignees' | 'reviewers' } =
    $props();

  // One import brings one source, but nothing in the shape promises it.
  const groups = $derived.by(() => {
    const byService = new Map<string, string[]>();
    for (const person of people ?? []) {
      byService.set(person.service, [...(byService.get(person.service) ?? []), person.login]);
    }
    return Array.from(byService, ([service, logins]) => ({
      service,
      platform: importedPlatform(service),
      logins: logins.map((login) => `@${login}`).join(', '),
    }));
  });
</script>

{#each groups as group (group.service)}
  <p class="imported-people">
    <span class="imported-badge">{group.platform}</span>
    <span class="imported-role">
      {role === 'assignees'
        ? t('author.assigned_on_source', 'assigned there:')
        : t('author.review_requested_on_source', 'review requested there:')}
    </span>
    <span class="imported-logins">{group.logins}</span>
  </p>
{/each}

<style>
  .imported-people {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--text-secondary);
  }

  .imported-badge {
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    background: var(--bg-secondary);
  }

  .imported-logins {
    color: var(--text-primary);
  }
</style>
