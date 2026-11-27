<script lang="ts">
  import { releases, repos } from '$lib/api/client.svelte';
  import type { RepositoryDetail } from '$lib/api/repos';
  import { createT, formatRelativeTime, pluralForm } from '$lib/i18n';
  import { languageColor } from '$lib/languageColors';

  /**
   * GitHub's right-hand column: what the repository is, its latest release
   * and its languages. Each part loads on its own and is left out when it
   * has nothing to show or fails to load — the file list never waits for it.
   */
  interface Props {
    owner: string;
    repo: string;
    /** The ref the page shows, `''` for the default branch. */
    ref: string;
    info: RepositoryDetail | null;
  }

  let { owner, repo, ref, info }: Props = $props();
  const t = createT();

  let languages = $state<{ name: string; bytes: number; percent: number }[]>([]);
  let releaseCount = $state(0);
  let latestRelease = $state<{ id: number; tag_name: string; title?: string; created_at: string } | null>(null);

  $effect(() => {
    const key = `${owner}/${repo}@${ref}`;
    languages = [];
    void Promise.resolve()
      .then(() => repos.languages(owner, repo, ref || undefined))
      .then((answer) => {
        if (key !== `${owner}/${repo}@${ref}`) return;
        const list = answer?.languages ?? [];
        const total = list.reduce((sum, language) => sum + language.bytes, 0);
        languages = total
          ? list.map((language) => ({ ...language, percent: (language.bytes / total) * 100 }))
          : [];
      })
      .catch(() => {});
  });

  $effect(() => {
    const key = `${owner}/${repo}`;
    releaseCount = 0;
    latestRelease = null;
    void Promise.resolve()
      .then(() => releases.list(owner, repo, 1, 10))
      .then((page) => {
        if (key !== `${owner}/${repo}`) return;
        releaseCount = page?.pagination?.total ?? 0;
        latestRelease = (page?.data ?? []).find((release: any) => !release.is_draft && !release.is_prerelease) ?? null;
      })
      .catch(() => {});
  });

  /** The languages under 0.5% folded into "Other", as GitHub does. */
  let shown = $derived.by(() => {
    const major = languages.filter((language) => language.percent >= 0.5);
    const rest = languages.filter((language) => language.percent < 0.5);
    const other = rest.reduce((sum, language) => sum + language.percent, 0);
    return other > 0
      ? [...major, { name: t('repo.about.other', 'Other'), bytes: 0, percent: other, other: true }]
      : major;
  });
</script>

<aside class="repo-about">
  <section class="about-section">
    <h3>{t('repo.about.title', 'About')}</h3>
    {#if info?.description}
      <p class="about-description">{info.description}</p>
    {:else}
      <p class="about-description muted">{t('repo.about.no_description', 'No description provided.')}</p>
    {/if}
    <ul class="about-stats">
      <li>
        <span>★ <strong>{info?.stars_count ?? 0}</strong> {pluralForm(info?.stars_count ?? 0, { one: t('repo.about.star_forms.one'), few: t('repo.about.star_forms.few'), many: t('repo.about.star_forms.many'), other: t('repo.about.star_forms.other') })}</span>
      </li>
      <li>
        <a href={`/${owner}/${repo}/network`}>⑂ <strong>{info?.forks_count ?? 0}</strong> {pluralForm(info?.forks_count ?? 0, { one: t('repo.about.fork_forms.one'), few: t('repo.about.fork_forms.few'), many: t('repo.about.fork_forms.many'), other: t('repo.about.fork_forms.other') })}</a>
      </li>
    </ul>
  </section>

  <section class="about-section">
    <h3>
      <a href={`/${owner}/${repo}/releases`}>{t('repo.about.releases', 'Releases')}</a>
      {#if releaseCount}<span class="about-count">{releaseCount}</span>{/if}
    </h3>
    {#if latestRelease}
      <a class="about-release" href={`/${owner}/${repo}/releases?release=${latestRelease.id}`}>
        <span class="about-release-name">
          🏷 {latestRelease.title || latestRelease.tag_name}
          <span class="about-latest">{t('repo.about.latest', 'Latest')}</span>
        </span>
        <span class="muted">{formatRelativeTime(latestRelease.created_at)}</span>
      </a>
    {:else}
      <p class="muted">{t('repo.about.no_releases', 'No releases published')}</p>
    {/if}
  </section>

  {#if shown.length}
    <section class="about-section">
      <h3>{t('repo.about.languages', 'Languages')}</h3>
      <div class="language-bar" aria-hidden="true">
        {#each shown as language (language.name)}
          <span style:width={`${language.percent}%`} style:background={'other' in language ? '#8b949e' : languageColor(language.name)}></span>
        {/each}
      </div>
      <ul class="language-list">
        {#each shown as language (language.name)}
          <li>
            <span class="language-dot" style:background={'other' in language ? '#8b949e' : languageColor(language.name)}></span>
            <span class="language-name">{language.name}</span>
            <span class="muted">{language.percent.toFixed(1)}%</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</aside>

<style>
  .repo-about { display: flex; flex-direction: column; min-width: 0; }
  .about-section { padding: 0 0 16px; margin-bottom: 16px; border-bottom: 1px solid var(--border-light); }
  .about-section:last-child { border-bottom: none; margin-bottom: 0; }
  h3 { display: flex; align-items: center; gap: 6px; font-size: 16px; font-weight: 600; margin: 0 0 12px; }
  h3 a { color: inherit; }
  .about-count {
    padding: 0 6px;
    border-radius: 10px;
    background: var(--bg-tertiary);
    font-size: 12px;
    font-weight: 500;
  }
  .about-description { margin: 0 0 12px; font-size: 15px; line-height: 1.5; overflow-wrap: anywhere; }
  .muted { color: var(--text-muted); font-size: 13px; }
  p.muted { margin: 0; }
  .about-stats { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
  .about-stats span, .about-stats a { color: var(--text-secondary); }
  .about-stats a:hover { color: var(--accent); }
  .about-release { display: flex; flex-direction: column; gap: 2px; color: var(--text-primary); font-size: 14px; }
  .about-release-name { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; font-weight: 600; }
  .about-latest {
    padding: 0 7px;
    border: 1px solid var(--green);
    border-radius: 10px;
    color: var(--green);
    font-size: 12px;
    font-weight: 500;
  }
  .language-bar { display: flex; height: 8px; border-radius: 6px; overflow: hidden; margin-bottom: 12px; background: var(--bg-tertiary); }
  .language-bar span { display: block; min-width: 2px; }
  .language-bar span + span { margin-left: 2px; }
  .language-list { list-style: none; margin: 0; padding: 0; display: flex; flex-wrap: wrap; gap: 6px 16px; font-size: 13px; }
  .language-list li { display: inline-flex; align-items: center; gap: 6px; }
  .language-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
  .language-name { font-weight: 600; }
</style>
