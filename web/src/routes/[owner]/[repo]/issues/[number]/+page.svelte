<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import AttachmentPanel from '$lib/components/AttachmentPanel.svelte';
  import BotBadge from '$lib/components/BotBadge.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import ThreadSubscription from '$lib/components/ThreadSubscription.svelte';
  import {
    buildIssueLinksPayload,
    collaborators,
    issues,
    milestones,
    repos,
    type Issue,
    type IssueComment,
    type IssueLinksFormState,
    type Milestone,
    type RepoPermission,
  } from '$lib/api/client.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { getUser } from '$lib/stores/auth.svelte';
  import { canModifyComment, isEdited } from '$lib/commentState';
  import { isRepoAdmin } from '$lib/repoPermission';
  import { createT, formatDate, formatTranslationFallback } from '$lib/i18n';
  import { renderMarkdown as renderMarkdownSafe } from '$lib/utils/markdown';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let number = $derived(parseInt($page.params.number!));
  let issue = $state<Issue | null>(null);
  let commentList = $state<IssueComment[]>([]);
  let milestoneList = $state<Milestone[]>([]);
  let assigneeOptions = $state<Array<{ id: number; label: string }>>([]);
  let loading = $state(true);
  let mutationBusy = $state(false);
  let error = $state('');
  let newComment = $state('');
  let linkForm = $state<IssueLinksFormState>({ assigneeId: '', milestoneId: '' });
  const issueRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;

  // Comment edit/delete and issue deletion (card_60961272e1ba): offered to a
  // comment's author and to repository administrators — the two the server
  // lets through — and deleting the issue to administrators only.
  let viewerPermission = $state<RepoPermission | null>(null);
  let viewer = $derived(getUser());
  let isAdmin = $derived(isRepoAdmin(viewerPermission));
  let editingCommentId = $state<number | null>(null);
  let editDraft = $state('');
  let editBusy = $state(false);
  let editError = $state('');
  let pendingCommentDelete = $state<IssueComment | null>(null);
  let commentDeleteBusy = $state(false);
  let commentDeleteError = $state('');
  let issueDeleteOpen = $state(false);
  let issueDeleteBusy = $state(false);
  let issueDeleteError = $state('');

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedNumber = number;
    routeGeneration += 1;
    issue = null;
    commentList = [];
    milestoneList = [];
    assigneeOptions = [];
    newComment = '';
    linkForm = { assigneeId: '', milestoneId: '' };
    mutationBusy = false;
    viewerPermission = null;
    editingCommentId = null;
    editDraft = '';
    editBusy = false;
    editError = '';
    pendingCommentDelete = null;
    commentDeleteBusy = false;
    commentDeleteError = '';
    issueDeleteOpen = false;
    issueDeleteBusy = false;
    issueDeleteError = '';
    error = '';
    loading = true;
    void loadIssue(expectedOwner, expectedRepo, expectedNumber, routeGeneration);
  });

  type IssueRoute = Readonly<{
    owner: string;
    repo: string;
    number: number;
    generation: number;
  }>;

  function currentRoute(): IssueRoute {
    return { owner, repo, number, generation: routeGeneration };
  }

  function isCurrentRoute(route: IssueRoute) {
    return (
      routeGeneration === route.generation &&
      owner === route.owner &&
      repo === route.repo &&
      number === route.number
    );
  }

  function beginMutation(): IssueRoute | null {
    if (mutationBusy) return null;
    const route = currentRoute();
    issueRequests.begin(route.owner, route.repo, route.number);
    mutationBusy = true;
    error = '';
    return route;
  }

  async function loadIssue(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedNumber = number,
    expectedRoute = routeGeneration,
  ) {
    const route = {
      owner: expectedOwner,
      repo: expectedRepo,
      number: expectedNumber,
      generation: expectedRoute,
    };
    if (!isCurrentRoute(route)) return;
    const claim = issueRequests.begin(expectedOwner, expectedRepo, expectedNumber);
    try {
      loading = true;
      error = '';
      const [issueData, commentsData, milestoneData, collaboratorData, repoData] = await Promise.all([
        issues.get(expectedOwner, expectedRepo, expectedNumber),
        issues.comments(expectedOwner, expectedRepo, expectedNumber),
        milestones.list(expectedOwner, expectedRepo),
        collaborators.list(expectedOwner, expectedRepo),
        repos.get(expectedOwner, expectedRepo),
      ]);
      if (!issueRequests.owns(claim, owner, repo, number) || !isCurrentRoute(route)) return;
      issue = issueData;
      commentList = commentsData || [];
      viewerPermission = repoData?.viewer_permission ?? null;
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
      candidates.set(repoData.owner_id, expectedOwner);
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
        // Assigned to someone who is neither the owner nor a collaborator any
        // more: the issue itself carries the name, so the picker shows it
        // rather than the number it used to fall back to.
        candidates.set(issueData.assignee_id, issueData.assignee ?? unnamed(issueData.assignee_id));
      }
      assigneeOptions = Array.from(candidates, ([id, label]) => ({ id, label }));
    } catch (e: any) {
      if (issueRequests.owns(claim, owner, repo, number) && isCurrentRoute(route)) {
        error = e.message;
      }
    } finally {
      if (issueRequests.owns(claim, owner, repo, number) && isCurrentRoute(route)) {
        loading = false;
      }
    }
  }

  async function handleComment(e: Event) {
    e.preventDefault();
    const route = beginMutation();
    if (!route) return;
    const body = newComment;
    try {
      await issues.addComment(route.owner, route.repo, route.number, body);
      if (!isCurrentRoute(route)) return;
      newComment = '';
      await loadIssue(route.owner, route.repo, route.number, route.generation);
    } catch (e: any) {
      if (isCurrentRoute(route)) error = e.message;
    } finally {
      if (isCurrentRoute(route)) mutationBusy = false;
    }
  }

  async function toggleState() {
    if (!issue) return;
    const route = beginMutation();
    if (!route) return;
    const newState = issue.state === 'open' ? 'closed' : 'open';
    try {
      await issues.update(route.owner, route.repo, route.number, { state: newState });
      if (!isCurrentRoute(route)) return;
      await loadIssue(route.owner, route.repo, route.number, route.generation);
    } catch (e: any) {
      if (isCurrentRoute(route)) error = e.message;
    } finally {
      if (isCurrentRoute(route)) mutationBusy = false;
    }
  }

  async function saveLinks() {
    const route = beginMutation();
    if (!route) return;
    const payload = buildIssueLinksPayload(linkForm);
    try {
      const nextIssue = await issues.update(route.owner, route.repo, route.number, payload);
      if (!isCurrentRoute(route)) return;
      issue = nextIssue;
      linkForm = {
        assigneeId: nextIssue.assignee_id === null ? '' : String(nextIssue.assignee_id),
        milestoneId: nextIssue.milestone_id === null ? '' : String(nextIssue.milestone_id),
      };
    } catch (e: any) {
      if (isCurrentRoute(route)) error = e.message;
    } finally {
      if (isCurrentRoute(route)) mutationBusy = false;
    }
  }

  function startEdit(comment: IssueComment) {
    editingCommentId = comment.id;
    editDraft = comment.body;
    editError = '';
  }

  function cancelEdit() {
    if (editBusy) return;
    editingCommentId = null;
    editDraft = '';
    editError = '';
  }

  async function saveEdit(comment: IssueComment) {
    const body = editDraft;
    if (!body.trim() || editBusy) return;
    const route = currentRoute();
    editBusy = true;
    editError = '';
    try {
      const updated = await issues.editComment(route.owner, route.repo, comment.id, body);
      if (!isCurrentRoute(route)) return;
      // The answer is the bare row: keep the author name the listing gave.
      commentList = commentList.map((candidate) =>
        candidate.id === comment.id ? { ...candidate, ...updated } : candidate,
      );
      editingCommentId = null;
      editDraft = '';
    } catch (e: any) {
      if (isCurrentRoute(route)) editError = e?.message || t('comments.edit_failed');
    } finally {
      if (isCurrentRoute(route)) editBusy = false;
    }
  }

  function askDeleteComment(comment: IssueComment) {
    pendingCommentDelete = comment;
    commentDeleteError = '';
  }

  function closeDeleteComment() {
    if (commentDeleteBusy) return;
    pendingCommentDelete = null;
    commentDeleteError = '';
  }

  async function confirmDeleteComment() {
    const comment = pendingCommentDelete;
    if (!comment || commentDeleteBusy) return;
    const route = currentRoute();
    commentDeleteBusy = true;
    commentDeleteError = '';
    try {
      await issues.deleteComment(route.owner, route.repo, comment.id);
      if (!isCurrentRoute(route)) return;
      commentList = commentList.filter((candidate) => candidate.id !== comment.id);
      if (editingCommentId === comment.id) editingCommentId = null;
      pendingCommentDelete = null;
    } catch (e: any) {
      if (isCurrentRoute(route)) commentDeleteError = e?.message || t('comments.delete_failed');
    } finally {
      if (isCurrentRoute(route)) commentDeleteBusy = false;
    }
  }

  function closeDeleteIssue() {
    if (issueDeleteBusy) return;
    issueDeleteOpen = false;
    issueDeleteError = '';
  }

  async function confirmDeleteIssue() {
    if (issueDeleteBusy) return;
    const route = currentRoute();
    issueDeleteBusy = true;
    issueDeleteError = '';
    try {
      await issues.delete(route.owner, route.repo, route.number);
      if (!isCurrentRoute(route)) return;
      issueDeleteOpen = false;
      await goto(`/${route.owner}/${route.repo}/issues`);
    } catch (e: any) {
      if (isCurrentRoute(route)) issueDeleteError = e?.message || t('issues.delete.failed');
    } finally {
      if (isCurrentRoute(route)) issueDeleteBusy = false;
    }
  }

  function renderMarkdown(content: string | null | undefined): string {
    if (!content) return '';
    return renderMarkdownSafe(content);
  }
</script>

<svelte:head>
  <title>{issue?.title || `${t('issues.title')} #${number}`} · {owner}/{repo} · Plombir Git</title>
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
            {t('issues.opened_by', { date: formatDate(issue.created_at), author: issue.author || t('common.unknown') })}<BotBadge owner={issue.author_bot_owner} />
          </span>
          {#if issue.labels?.length}
            {#each issue.labels as label}
              <span class="label-badge">{label}</span>
            {/each}
          {/if}
        </div>
      </div>

      <ThreadSubscription {owner} {repo} kind="issues" {number} />

      <section class="issue-links">
        <h2>{t('issues.links')}</h2>
        <div class="issue-links-grid">
          <label>
            {t('issues.assignee')}
            <select bind:value={linkForm.assigneeId} disabled={mutationBusy}>
              <option value="">{t('issues.unassigned')}</option>
              {#each assigneeOptions as assignee (assignee.id)}
                <option value={String(assignee.id)}>{assignee.label}</option>
              {/each}
            </select>
          </label>
          <label>
            {t('issues.milestone')}
            <select bind:value={linkForm.milestoneId} disabled={mutationBusy}>
              <option value="">{t('issues.no_milestone')}</option>
              {#each milestoneList as milestone (milestone.id)}
                <option value={String(milestone.id)}>{milestone.title} · {t(`milestones.${milestone.state}`, undefined, formatTranslationFallback(milestone.state))}</option>
              {/each}
            </select>
          </label>
          <button class="btn-primary save-links" type="button" onclick={saveLinks} disabled={mutationBusy}>
            {mutationBusy ? t('common.loading') : t('issues.save_links')}
          </button>
        </div>
      </section>

      {#if issue.body}
        <div class="issue-body">
          <div class="comment-header">
            {t('issues.commented', { author: issue.author || t('common.unknown'), date: formatDate(issue.created_at) })}<BotBadge owner={issue.author_bot_owner} />
          </div>
          <div class="comment-body markdown-body">{@html renderMarkdown(issue.body)}</div>
        </div>
      {/if}

      <AttachmentPanel {owner} {repo} target="issues" targetId={number} />

      <!-- Comments -->
      {#each commentList as comment (comment.id)}
        <div class="comment" data-comment={comment.id}>
          <div class="comment-header">
            <span>
              {t('issues.commented', { author: comment.author || t('common.unknown'), date: formatDate(comment.created_at) })}<BotBadge owner={comment.author_bot_owner} />
              {#if isEdited(comment)}
                <span class="edited-marker" title={formatDate(comment.updated_at)}>{t('comments.edited')}</span>
              {/if}
            </span>
            {#if canModifyComment(comment, viewer, viewerPermission) && editingCommentId !== comment.id}
              <span class="comment-actions">
                <button type="button" class="btn-link edit-comment" onclick={() => startEdit(comment)} disabled={editBusy}>{t('common.edit')}</button>
                <button type="button" class="btn-link danger delete-comment" onclick={() => askDeleteComment(comment)}>{t('common.delete')}</button>
              </span>
            {/if}
          </div>
          {#if editingCommentId === comment.id}
            <div class="comment-edit">
              <textarea class="comment-edit-input" bind:value={editDraft} rows="4" disabled={editBusy} aria-label={t('comments.edit_label')}></textarea>
              {#if editError}
                <div class="error-banner comment-edit-error" role="alert">{editError}</div>
              {/if}
              <div class="form-actions">
                <button type="button" class="btn-primary save-comment-edit" onclick={() => saveEdit(comment)} disabled={editBusy || !editDraft.trim()}>
                  {editBusy ? t('common.saving') : t('common.save')}
                </button>
                <button type="button" class="btn-close cancel-comment-edit" onclick={cancelEdit} disabled={editBusy}>{t('common.cancel')}</button>
              </div>
            </div>
          {:else}
            <div class="comment-body markdown-body">{@html renderMarkdown(comment.body)}</div>
          {/if}
          <AttachmentPanel {owner} {repo} target="issues/comments" targetId={comment.id} />
        </div>
      {/each}

      <!-- Add comment -->
      <form onsubmit={handleComment} class="comment-form">
        <textarea bind:value={newComment} rows="4" placeholder={t('issues.comment_placeholder')} disabled={mutationBusy}></textarea>
        <div class="form-actions">
          <button type="submit" class="btn-primary" disabled={mutationBusy || !newComment.trim()}>{t('issues.comment')}</button>
          <button type="button" class="btn-close" onclick={toggleState} disabled={mutationBusy}>
            {issue.state === 'open' ? t('issues.close_issue') : t('issues.reopen_issue')}
          </button>
        </div>
      </form>

      {#if isAdmin}
        <section class="issue-danger">
          <h2>{t('issues.delete.title')}</h2>
          <p class="text-secondary">{t('issues.delete.desc')}</p>
          <button type="button" class="btn-danger delete-issue" onclick={() => { issueDeleteOpen = true; issueDeleteError = ''; }}>
            {t('issues.delete.button')}
          </button>
        </section>
      {/if}
    </div>
  {/if}
</div>

{#if pendingCommentDelete}
  <Modal onclose={closeDeleteComment} labelledby="delete-comment-title">
    <h2 id="delete-comment-title">{t('comments.delete_title')}</h2>
    <p>{t('comments.delete_confirm')}</p>
    {#if commentDeleteError}
      <div class="error-banner comment-delete-error" role="alert">{commentDeleteError}</div>
    {/if}
    <div class="form-actions">
      <button type="button" class="btn-danger confirm-delete-comment" onclick={confirmDeleteComment} disabled={commentDeleteBusy}>
        {commentDeleteBusy ? t('common.deleting') : t('common.delete')}
      </button>
      <button type="button" class="btn-close" onclick={closeDeleteComment} disabled={commentDeleteBusy} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

{#if issueDeleteOpen && issue}
  <Modal onclose={closeDeleteIssue} labelledby="delete-issue-title">
    <h2 id="delete-issue-title">{t('issues.delete.confirm_title', { number: issue.number })}</h2>
    <p>{t('issues.delete.confirm_body')}</p>
    {#if issueDeleteError}
      <div class="error-banner issue-delete-error" role="alert">{issueDeleteError}</div>
    {/if}
    <div class="form-actions">
      <button type="button" class="btn-danger confirm-delete-issue" onclick={confirmDeleteIssue} disabled={issueDeleteBusy}>
        {issueDeleteBusy ? t('common.deleting') : t('issues.delete.button')}
      </button>
      <button type="button" class="btn-close" onclick={closeDeleteIssue} disabled={issueDeleteBusy} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

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

  .comment-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }
  .edited-marker { margin-left: 6px; color: var(--text-muted); font-size: 12px; }
  .comment-actions { display: inline-flex; gap: 8px; }
  .btn-link {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font-size: 12px;
    cursor: pointer;
  }
  .btn-link.danger { color: var(--red); }
  .btn-link:disabled { opacity: 0.5; cursor: not-allowed; }
  .comment-edit { padding: 12px 16px; }
  .comment-edit textarea {
    width: 100%;
    box-sizing: border-box;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
    color: var(--text-primary);
    font-family: inherit;
  }
  .issue-danger {
    margin-top: 32px;
    padding: 16px;
    border: 1px solid var(--red);
    border-radius: var(--radius);
  }
  .issue-danger h2 { font-size: 16px; margin: 0 0 8px; color: var(--red); }
  .issue-danger p { margin: 0 0 12px; font-size: 13px; }
  .btn-danger {
    padding: 6px 16px;
    background: none;
    color: var(--red);
    border: 1px solid var(--red);
    border-radius: var(--radius);
    font-size: 14px;
    cursor: pointer;
  }
  .btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }

  @media (max-width: 700px) {
    .issue-links-grid { grid-template-columns: 1fr; }
  }
</style>
