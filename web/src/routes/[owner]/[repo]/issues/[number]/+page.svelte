<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import AttachmentPanel from '$lib/components/AttachmentPanel.svelte';
  import {
    buildIssueLinksPayload,
    collaborators,
    issues,
    milestones,
    repos,
    type Issue,
    type IssueLinksFormState,
    type Milestone,
  } from '$lib/api/client.svelte';
  import { getUser } from '$lib/stores/auth.svelte';
  import { createT, formatDate, formatTranslationFallback } from '$lib/i18n';
  import { renderMarkdown as renderMarkdownSafe } from '$lib/utils/markdown';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let number = $derived(parseInt($page.params.number!));
  let issue = $state<Issue | null>(null);
  let commentList = $state<any[]>([]);
  let milestoneList = $state<Milestone[]>([]);
  let assigneeOptions = $state<Array<{ id: number; label: string }>>([]);
  let loading = $state(true);
  let savingLinks = $state(false);
  let error = $state('');
  let newComment = $state('');
  let linkForm = $state<IssueLinksFormState>({ assigneeId: '', milestoneId: '' });

  $effect(() => {
    loadIssue();
  });

  async function loadIssue() {
    try {
      loading = true;
      const [issueData, commentsData, milestoneData, collaboratorData, repoData] = await Promise.all([
        issues.get(owner, repo, number),
        issues.comments(owner, repo, number),
        milestones.list(owner, repo),
        collaborators.list(owner, repo),
        repos.get(owner, repo),
      ]);
      issue = issueData;
      commentList = commentsData || [];
      milestoneList = milestoneData;
      linkForm = {
        assigneeId: issueData.assignee_id === null ? '' : String(issueData.assignee_id),
        milestoneId: issueData.milestone_id === null ? '' : String(issueData.milestone_id),
      };

      // Everyone this issue can be assigned to, by name. The picker used to
      // label every option but the reader's own account `User #N`, so choosing
      // an assignee meant choosing a number (card_73ce6d28518b). The owner is
      // named by the route itself, the collaborators by the listing that now
      // carries `username`, and an id neither of those explains keeps the
      // numeric label as an honest fallback rather than disappearing.
      const unnamed = (userId: number) => t('issues.unnamed_user', { userId });
      const candidates = new Map<number, string>();
      candidates.set(repoData.owner_id, owner);
      for (const collaborator of collaboratorData) {
        if (!candidates.has(collaborator.user_id)) {
          candidates.set(collaborator.user_id, collaborator.username ?? unnamed(collaborator.user_id));
        }
      }
      const currentUser = getUser();
      if (currentUser) {
        candidates.set(currentUser.id, currentUser.username);
      }
      if (issueData.assignee_id !== null && !candidates.has(issueData.assignee_id)) {
        candidates.set(issueData.assignee_id, unnamed(issueData.assignee_id));
      }
      assigneeOptions = Array.from(candidates, ([id, label]) => ({ id, label }));
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function handleComment(e: Event) {
    e.preventDefault();
    try {
      await issues.addComment(owner, repo, number, newComment);
      newComment = '';
      await loadIssue();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function toggleState() {
    if (!issue) return;
    try {
      const newState = issue.state === 'open' ? 'closed' : 'open';
      await issues.update(owner, repo, number, { state: newState });
      await loadIssue();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function saveLinks() {
    savingLinks = true;
    error = '';
    try {
      issue = await issues.update(owner, repo, number, buildIssueLinksPayload(linkForm));
      linkForm = {
        assigneeId: issue.assignee_id === null ? '' : String(issue.assignee_id),
        milestoneId: issue.milestone_id === null ? '' : String(issue.milestone_id),
      };
    } catch (e: any) {
      error = e.message;
    } finally {
      savingLinks = false;
    }
  }

  function renderMarkdown(content: string | null | undefined): string {
    if (!content) return '';
    return renderMarkdownSafe(content);
  }
</script>

<svelte:head>
  <title>{issue?.title || `${t('issues.title')} #${number}`} · {owner}/{repo} · ForgeKeep</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="issues" starsCount={0} />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if issue}
    <div class="issue-detail">
      <div class="issue-header">
        <div class="issue-title-row">
          <h1>{issue.title}</h1>
          <span class="issue-number">#{issue.number}</span>
        </div>
        <div class="issue-meta">
          <span class="state-badge" class:open={issue.state === 'open'} class:closed={issue.state === 'closed'}>
            {t(`issues.state.${issue.state}`, undefined, formatTranslationFallback(issue.state))}
          </span>
          <span class="text-secondary">
            {t('issues.opened_by', { date: formatDate(issue.created_at), author: issue.author || t('common.unknown') })}
          </span>
          {#if issue.labels?.length}
            {#each issue.labels as label}
              <span class="label-badge">{label}</span>
            {/each}
          {/if}
        </div>
      </div>

      <section class="issue-links">
        <h2>{t('issues.links')}</h2>
        <div class="issue-links-grid">
          <label>
            {t('issues.assignee')}
            <select bind:value={linkForm.assigneeId} disabled={savingLinks}>
              <option value="">{t('issues.unassigned')}</option>
              {#each assigneeOptions as assignee (assignee.id)}
                <option value={String(assignee.id)}>{assignee.label}</option>
              {/each}
            </select>
          </label>
          <label>
            {t('issues.milestone')}
            <select bind:value={linkForm.milestoneId} disabled={savingLinks}>
              <option value="">{t('issues.no_milestone')}</option>
              {#each milestoneList as milestone (milestone.id)}
                <option value={String(milestone.id)}>{milestone.title} · {t(`milestones.${milestone.state}`, undefined, formatTranslationFallback(milestone.state))}</option>
              {/each}
            </select>
          </label>
          <button class="btn-primary save-links" type="button" onclick={saveLinks} disabled={savingLinks}>
            {savingLinks ? t('common.loading') : t('issues.save_links')}
          </button>
        </div>
      </section>

      {#if issue.body}
        <div class="issue-body">
          <div class="comment-header">
            {t('issues.commented', { author: issue.author || t('common.unknown'), date: formatDate(issue.created_at) })}
          </div>
          <div class="comment-body markdown-body">{@html renderMarkdown(issue.body)}</div>
        </div>
      {/if}

      <AttachmentPanel {owner} {repo} target="issues" targetId={number} />

      <!-- Comments -->
      {#each commentList as comment}
        <div class="comment">
          <div class="comment-header">
            {t('issues.commented', { author: comment.author || t('common.unknown'), date: formatDate(comment.created_at) })}
          </div>
          <div class="comment-body markdown-body">{@html renderMarkdown(comment.body)}</div>
          <AttachmentPanel {owner} {repo} target="issues/comments" targetId={comment.id} />
        </div>
      {/each}

      <!-- Add comment -->
      <form onsubmit={handleComment} class="comment-form">
        <textarea bind:value={newComment} rows="4" placeholder={t('issues.comment_placeholder')}></textarea>
        <div class="form-actions">
          <button type="submit" class="btn-primary" disabled={!newComment.trim()}>{t('issues.comment')}</button>
          <button type="button" class="btn-close" onclick={toggleState}>
            {issue.state === 'open' ? t('issues.close_issue') : t('issues.reopen_issue')}
          </button>
        </div>
      </form>
    </div>
  {/if}
</div>

<style>
.issue-detail { max-width: 800px; }

  .issue-header { margin-bottom: 24px; }

  .issue-title-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h1 { font-size: 24px; }
  .issue-number { color: var(--text-muted); font-size: 18px; }

  .issue-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    font-size: 13px;
  }

  .state-badge {
    padding: 2px 10px;
    border-radius: 12px;
    font-size: 12px;
    font-weight: 600;
  }
  .state-badge.open { background: rgba(63, 185, 80, 0.15); color: var(--green); }
  .state-badge.closed { background: rgba(248, 81, 73, 0.15); color: var(--red); }

  .label-badge {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--purple);
    color: var(--purple);
    border-radius: 10px;
    font-size: 11px;
  }

  .issue-links {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px 16px;
    margin-bottom: 16px;
  }
  .issue-links h2 { margin: 0 0 10px; font-size: 15px; }
  .issue-links-grid { display: grid; grid-template-columns: 1fr 1fr auto; gap: 10px; align-items: end; }
  .issue-links label { display: flex; flex-direction: column; gap: 5px; font-size: 12px; color: var(--text-secondary); }
  .issue-links select {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
    color: var(--text-primary);
    padding: 6px 8px;
  }
  .save-links { white-space: nowrap; }

  .issue-body, .comment {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
    margin-bottom: 12px;
  }

  .comment-header {
    padding: 8px 16px;
    background: var(--bg-tertiary);
    font-size: 13px;
    color: var(--text-secondary);
  }

  .comment-body {
    padding: 16px;
    font-size: 14px;
    line-height: 1.6;
  }

  .comment-body :global(p) {
    margin: 0 0 12px;
  }

  .comment-body :global(p:last-child) {
    margin-bottom: 0;
  }

  .comment-body :global(ul),
  .comment-body :global(ol) {
    padding-left: 24px;
    margin: 8px 0 12px;
  }

  .comment-body :global(pre) {
    overflow-x: auto;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
  }

  .comment-body :global(code) {
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .comment-form {
    margin-top: 16px;
  }

  textarea {
    width: 100%;
    font-family: var(--font-mono);
    font-size: 13px;
    resize: vertical;
    margin-bottom: 8px;
  }

  .form-actions {
    display: flex;
    gap: 8px;
  }

  .btn-primary {
    padding: 6px 16px;
    background: var(--green-dim);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:hover { background: var(--green); }
  .btn-primary:disabled { opacity: 0.5; }

  .btn-close {
    padding: 6px 16px;
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    cursor: pointer;
  }
  .btn-close:hover { background: var(--bg-hover); }

  @media (max-width: 700px) {
    .issue-links-grid { grid-template-columns: 1fr; }
  }
</style>
