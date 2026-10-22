<script lang="ts">
  // What an import brought here that a GitHub / GitLab account this person
  // signs in with wrote, per repository, and the confirmed move of it to their
  // account (card_2a006cf365f6). Nothing moves on sign-in by itself: the list
  // is a question, and each repository is taken back only after the person
  // confirms it. Renders nothing when there is nothing to take back.
  import { auth, type ClaimableImportedRepository } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';
  import type { Confirmer } from '$lib/confirm.svelte';

  const t = createT();

  let { confirmer }: { confirmer: Confirmer } = $props();

  let rows = $state<ClaimableImportedRepository[]>([]);
  let busy = $state(false);
  let error = $state('');
  let success = $state('');

  const PLATFORMS: Record<string, string> = { github: 'GitHub', gitlab: 'GitLab' };
  const platform = (service: string) => PLATFORMS[service] ?? service;
  const total = (row: ClaimableImportedRepository) =>
    row.issues + row.comments + row.pull_requests + row.reviews + row.releases;

  async function load() {
    try {
      const listed = await auth.listImportedAuthorship();
      rows = Array.isArray(listed) ? listed : [];
    } catch (err: any) {
      error = err?.message || t('account_security.imported.load_failed');
    }
  }

  async function claim(row: ClaimableImportedRepository) {
    if (busy) return;
    if (
      !(await confirmer.ask({
        title: t('account_security.imported.claim_confirm_title'),
        message: t('account_security.imported.claim_confirm', {
          repository: row.repository,
          login: `@${row.login}`,
        }),
        confirmLabel: t('account_security.imported.claim'),
      }))
    )
      return;
    try {
      busy = true;
      error = '';
      success = '';
      const moved = await auth.claimImportedAuthorship([row.repo_id]);
      const count =
        moved.issues + moved.comments + moved.pull_requests + moved.reviews + moved.releases;
      success = t('account_security.imported.claimed', { count, repository: row.repository });
      rows = rows.filter((candidate) => candidate.repo_id !== row.repo_id);
    } catch (err: any) {
      error = err?.message || t('account_security.imported.claim_failed');
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    load();
  });
</script>

{#if rows.length > 0 || error || success}
  <section class="section imported-authorship">
    <div class="section-heading">
      <div>
        <h2>{t('account_security.imported.title')}</h2>
        <p>{t('account_security.imported.description')}</p>
      </div>
    </div>

    {#if error}<div class="error-box" role="alert">{error}</div>{/if}
    {#if success}<div class="success-box" role="status">{success}</div>{/if}

    {#if rows.length > 0}
      <ul class="passkey-list">
        {#each rows as row (`${row.repo_id}:${row.service}`)}
          <li>
            <div>
              <strong>{row.repository}</strong>
              <span class="muted">
                {platform(row.service)} · {t('account_security.imported.as', { login: `@${row.login}` })}
                · {t('account_security.imported.counts', {
                  issues: row.issues,
                  comments: row.comments,
                  pull_requests: row.pull_requests,
                  reviews: row.reviews,
                  releases: row.releases,
                })}
              </span>
            </div>
            <button
              type="button"
              class="btn btn-secondary"
              onclick={() => claim(row)}
              disabled={busy || total(row) === 0}
            >
              {t('account_security.imported.claim')}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}

<style>
  /* The look of the security page's other sections, which scope theirs. */
  .section {
    margin-bottom: 20px;
    padding: 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
  }

  .section-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
  }

  h2 {
    margin: 0;
    font-size: 18px;
  }

  p {
    margin: 0;
  }

  .section-heading p,
  .muted {
    color: var(--text-secondary);
  }

  .passkey-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  .passkey-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
  }

  .passkey-list li > div {
    display: grid;
    gap: 2px;
  }

  .passkey-list .muted {
    font-size: 12px;
  }

  .error-box,
  .success-box {
    margin-bottom: 16px;
    padding: 12px 14px;
    border-radius: var(--radius);
  }

  .error-box {
    border: 1px solid var(--red-dim);
    background: color-mix(in srgb, var(--red-dim) 14%, transparent);
    color: var(--red);
  }

  .success-box {
    border: 1px solid var(--green-dim);
    background: color-mix(in srgb, var(--green-dim) 14%, transparent);
    color: var(--green);
  }
</style>
