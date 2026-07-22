<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import AttachmentPanel from '$lib/components/AttachmentPanel.svelte';
  import { pulls, reviews } from '$lib/api/client.svelte';
  import type { DiffLine, MergeQueueEntry, PrDiff } from '$lib/api/pulls';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let number = $derived(parseInt($page.params.number!));
  let pr = $state<any>(null);
  let diffData = $state<PrDiff | null>(null);
  let reviewList = $state<any[]>([]);
  let reviewComments = $state<any[]>([]);
  let timeline = $state<any[]>([]);
  let requestedReviewers = $state<Array<{ id: number; reviewer_id: number; username: string; requested_by_id: number; created_at: string }>>([]);
  let mergeQueue = $state<MergeQueueEntry[]>([]);
  let loading = $state(true);
  let error = $state('');
  let activeTab = $state('conversation');
  let mergeStrategy = $state('merge');
  let merging = $state(false);
  let managingAutoMerge = $state(false);
  let managingMergeQueue = $state(false);
  let autoMergeReason = $state('');
  let reviewBody = $state('');
  let reviewVerdict = $state('comment');
  let reviewerUsername = $state('');
  let managingReviewer = $state(false);
  let updatingDraft = $state(false);
  let resolvingCommentId = $state<number | null>(null);
  let commentTarget = $state<{ key: string; path: string; line: number; side: 'LEFT' | 'RIGHT' } | null>(null);
  let inlineCommentBody = $state('');
  let submittingInlineComment = $state(false);
  let suggestingChange = $state(false);
  let suggestedContent = $state('');
  let suggestionLineCount = $state(1);
  let applyingSuggestionId = $state<number | null>(null);
  let selectedSuggestionIds = $state<number[]>([]);
  let applyingSuggestions = $state(false);
  let rootComments = $derived(reviewComments.filter((comment) => !comment.reply_to_id));
  let applicableSuggestions = $derived(rootComments.filter((comment) =>
    comment.suggestion !== null && comment.suggestion !== undefined &&
    !comment.suggestion_applied_at && comment.commit_id === pr?.head_sha
  ));
  let queuedEntry = $derived(mergeQueue.find((entry) => entry.pr_number === number));

  $effect(() => {
    loadPR();
  });

  async function loadPR() {
    try {
      loading = true;
      const [prData, diffResult, reviewResult, commentsResult, timelineResult, reviewersResult, queueResult] = await Promise.all([
        pulls.get(owner, repo, number),
        pulls.diff(owner, repo, number).catch(() => null),
        reviews.list(owner, repo, number).catch(() => []),
        reviews.comments(owner, repo, number).catch(() => []),
        reviews.timeline(owner, repo, number).catch(() => []),
        reviews.requestedReviewers(owner, repo, number).catch(() => []),
        pulls.mergeQueue(owner, repo).catch(() => []),
      ]);
      pr = prData;
      diffData = diffResult;
      reviewList = reviewResult || [];
      reviewComments = commentsResult || [];
      timeline = timelineResult || [];
      selectedSuggestionIds = selectedSuggestionIds.filter((id) =>
        reviewComments.some((comment) => comment.id === id && !comment.suggestion_applied_at && comment.commit_id === prData.head_sha)
      );
      requestedReviewers = reviewersResult || [];
      mergeQueue = queueResult || [];
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function toggleDraft() {
    try {
      updatingDraft = true;
      error = '';
      pr = await pulls.update(owner, repo, number, { draft: !pr.is_draft });
    } catch (e: any) {
      error = e.message;
    } finally {
      updatingDraft = false;
    }
  }

  async function requestReviewer() {
    if (!reviewerUsername.trim()) return;
    try {
      managingReviewer = true;
      error = '';
      await reviews.requestReviewer(owner, repo, number, reviewerUsername.trim());
      reviewerUsername = '';
      requestedReviewers = await reviews.requestedReviewers(owner, repo, number);
    } catch (e: any) {
      error = e.message;
    } finally {
      managingReviewer = false;
    }
  }

  async function removeReviewer(username: string) {
    try {
      managingReviewer = true;
      error = '';
      await reviews.removeRequestedReviewer(owner, repo, number, username);
      requestedReviewers = requestedReviewers.filter((reviewer) => reviewer.username !== username);
    } catch (e: any) {
      error = e.message;
    } finally {
      managingReviewer = false;
    }
  }

  async function setThreadResolved(comment: any, resolved: boolean) {
    try {
      resolvingCommentId = comment.id;
      error = '';
      await reviews.setThreadResolved(owner, repo, number, comment.id, resolved);
      reviewComments = await reviews.comments(owner, repo, number);
    } catch (e: any) {
      error = e.message;
    } finally {
      resolvingCommentId = null;
    }
  }

  function repliesFor(rootId: number) {
    return reviewComments.filter((comment) => comment.reply_to_id === rootId);
  }

  function commentLocation(path: string, line: DiffLine, index: number) {
    if (line.kind === 'deletion' && line.old_line != null) {
      return { key: `${path}:${index}`, path, line: line.old_line, side: 'LEFT' as const };
    }
    if ((line.kind === 'addition' || line.kind === 'context') && line.new_line != null) {
      return { key: `${path}:${index}`, path, line: line.new_line, side: 'RIGHT' as const };
    }
    return null;
  }

  function commentsForLine(path: string, line: number, side: 'LEFT' | 'RIGHT') {
    return rootComments.filter((comment) =>
      comment.path === path && comment.line === line && (!comment.side || comment.side === side)
    );
  }

  function startInlineComment(target: { key: string; path: string; line: number; side: 'LEFT' | 'RIGHT' }, content: string) {
    commentTarget = target;
    inlineCommentBody = '';
    suggestingChange = false;
    suggestedContent = content;
    suggestionLineCount = 1;
  }

  async function submitInlineComment() {
    if (!commentTarget || !inlineCommentBody.trim()) return;
    try {
      submittingInlineComment = true;
      error = '';
      const rangeLength = Math.min(100, Math.max(1, Number(suggestionLineCount) || 1));
      await reviews.addComment(owner, repo, number, {
        path: commentTarget.path,
        line: suggestingChange ? commentTarget.line + rangeLength - 1 : commentTarget.line,
        start_line: suggestingChange ? commentTarget.line : undefined,
        side: commentTarget.side,
        start_side: suggestingChange ? commentTarget.side : undefined,
        body: inlineCommentBody.trim(),
        suggestion: suggestingChange ? suggestedContent : undefined,
      });
      reviewComments = await reviews.comments(owner, repo, number);
      commentTarget = null;
      inlineCommentBody = '';
      suggestingChange = false;
      suggestedContent = '';
      suggestionLineCount = 1;
    } catch (e: any) {
      error = e.message;
    } finally {
      submittingInlineComment = false;
    }
  }

  async function applySuggestion(comment: any) {
    try {
      applyingSuggestionId = comment.id;
      error = '';
      await reviews.applySuggestion(owner, repo, number, comment.id);
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      applyingSuggestionId = null;
    }
  }

  function toggleSuggestionSelection(commentId: number) {
    selectedSuggestionIds = selectedSuggestionIds.includes(commentId)
      ? selectedSuggestionIds.filter((id) => id !== commentId)
      : [...selectedSuggestionIds, commentId];
  }

  async function applySelectedSuggestions() {
    if (selectedSuggestionIds.length === 0) return;
    try {
      applyingSuggestions = true;
      error = '';
      await reviews.applySuggestions(owner, repo, number, selectedSuggestionIds);
      selectedSuggestionIds = [];
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      applyingSuggestions = false;
    }
  }

  async function handleMerge() {
    try {
      merging = true;
      await pulls.merge(owner, repo, number, mergeStrategy);
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      merging = false;
    }
  }

  async function enableAutoMerge() {
    try {
      managingAutoMerge = true;
      error = '';
      const outcome = await pulls.enableAutoMerge(owner, repo, number, mergeStrategy);
      autoMergeReason = outcome.reason || '';
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      managingAutoMerge = false;
    }
  }

  async function disableAutoMerge() {
    try {
      managingAutoMerge = true;
      error = '';
      pr = await pulls.disableAutoMerge(owner, repo, number);
      autoMergeReason = '';
    } catch (e: any) {
      error = e.message;
    } finally {
      managingAutoMerge = false;
    }
  }

  async function enqueueMerge() {
    try {
      managingMergeQueue = true;
      error = '';
      await pulls.enqueueMerge(owner, repo, number, mergeStrategy);
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      managingMergeQueue = false;
    }
  }

  async function cancelQueuedMerge() {
    try {
      managingMergeQueue = true;
      error = '';
      await pulls.cancelQueuedMerge(owner, repo, number);
      await loadPR();
    } catch (e: any) {
      error = e.message;
    } finally {
      managingMergeQueue = false;
    }
  }

  async function handleSubmitReview() {
    try {
      await reviews.submit(owner, repo, number, reviewBody, reviewVerdict);
      reviewBody = '';
      reviewVerdict = 'comment';
      await loadPR();
    } catch (e: any) {
      error = e.message;
    }
  }

</script>

<svelte:head>
  <title>PR #{number} · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="pulls" starsCount={0} />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if pr}
    <div class="pr-detail">
      <!-- Header -->
      <div class="pr-header">
        <h1>{pr.title}</h1>
        <div class="pr-meta">
          <span class="state-badge" class:open={pr.state === 'open'} class:closed={pr.state === 'closed'} class:merged={pr.state === 'merged'}>
            {t(`pulls.state.${pr.state}`)}
          </span>
          {#if pr.is_draft}<span class="draft-badge">{t('pulls.draft')}</span>{/if}
          <span class="text-secondary">
            opened {formatDate(pr.created_at)} by <strong>{pr.author || t('common.unknown')}</strong>
          </span>
          <span class="branch-pair">
            <span class="branch-label">{pr.head_branch}</span>
            →
            <span class="branch-label">{pr.base_branch}</span>
          </span>
          {#if pr.state === 'open'}
            <button class="btn-link" onclick={toggleDraft} disabled={updatingDraft}>
              {pr.is_draft ? t('pulls.mark_ready') : t('pulls.convert_draft')}
            </button>
          {/if}
        </div>
      </div>

      {#if pr.body}
        <div class="pr-body">
          <div class="comment-header">
            <strong>{pr.author || t('common.unknown')}</strong> commented
          </div>
          <div class="comment-body">{pr.body}</div>
        </div>
      {/if}

      <AttachmentPanel {owner} {repo} target="pulls" targetId={number} />

      <!-- Tabs -->
      <div class="pr-tabs">
        <button class="tab" class:active={activeTab === 'conversation'} onclick={() => activeTab = 'conversation'}>
          {t('pulls.tabs.conversation')}
        </button>
        <button class="tab" class:active={activeTab === 'diff'} onclick={() => activeTab = 'diff'}>
          {t('pulls.tabs.changes')}
        </button>
        <button class="tab" class:active={activeTab === 'review'} onclick={() => activeTab = 'review'}>
          {t('pulls.tabs.reviews')} ({reviewList.length})
        </button>
      </div>

      <!-- Conversation tab -->
      {#if activeTab === 'conversation'}
        <div class="conversation">
          <section class="reviewers-box">
            <h3>{t('pulls.reviewers.title')}</h3>
            {#if requestedReviewers.length === 0}
              <p class="text-secondary">{t('pulls.reviewers.empty')}</p>
            {:else}
              <div class="reviewer-list">
                {#each requestedReviewers as reviewer (reviewer.id)}
                  <span class="reviewer-chip">
                    @{reviewer.username}
                    <button
                      aria-label={t('pulls.reviewers.remove', { username: reviewer.username })}
                      disabled={managingReviewer}
                      onclick={() => removeReviewer(reviewer.username)}
                    >×</button>
                  </span>
                {/each}
              </div>
            {/if}
            <div class="reviewer-form">
              <input bind:value={reviewerUsername} placeholder={t('pulls.reviewers.placeholder')} />
              <button class="btn-secondary" onclick={requestReviewer} disabled={managingReviewer || !reviewerUsername.trim()}>
                {t('pulls.reviewers.request')}
              </button>
            </div>
          </section>

          <!-- Merge box -->
          {#if pr.state === 'open'}
            <div class="merge-box">
              {#if pr.is_draft}
                <div class="draft-notice">{t('pulls.merge.draft_blocked')}</div>
              {:else if queuedEntry}
                <div class="auto-merge-pending">
                  <div>
                    <strong>{t('pulls.merge.queue_position', { position: queuedEntry.position })}</strong>
                    <span>{t('pulls.merge.queue_waiting', { strategy: queuedEntry.strategy })}</span>
                  </div>
                  <button class="btn-secondary" onclick={cancelQueuedMerge} disabled={managingMergeQueue || queuedEntry.status === 'running'}>
                    {t('pulls.merge.leave_queue')}
                  </button>
                </div>
              {:else if pr.auto_merge_enabled}
                <div class="auto-merge-pending">
                  <div>
                    <strong>{t('pulls.merge.auto_enabled')}</strong>
                    <span>{t('pulls.merge.auto_waiting', { strategy: pr.auto_merge_strategy })}</span>
                    {#if autoMergeReason}<small>{autoMergeReason}</small>{/if}
                  </div>
                  <button class="btn-secondary" onclick={disableAutoMerge} disabled={managingAutoMerge}>
                    {t('pulls.merge.disable_auto')}
                  </button>
                </div>
              {:else}
                <div class="merge-row">
                  <select bind:value={mergeStrategy} class="merge-select">
                    <option value="merge">{t('pulls.merge.strategy.merge')}</option>
                    <option value="squash">{t('pulls.merge.strategy.squash')}</option>
                    <option value="rebase">{t('pulls.merge.strategy.rebase')}</option>
                  </select>
                  <button class="btn-merge" onclick={handleMerge} disabled={merging}>
                    {merging ? t('pulls.merge.merging') : t('pulls.merge.button')}
                  </button>
                  <button class="btn-secondary" onclick={enableAutoMerge} disabled={managingAutoMerge}>
                    {managingAutoMerge ? t('pulls.merge.enabling_auto') : t('pulls.merge.enable_auto')}
                  </button>
                  <button class="btn-secondary" onclick={enqueueMerge} disabled={managingMergeQueue}>
                    {managingMergeQueue ? t('pulls.merge.joining_queue') : t('pulls.merge.join_queue')}
                  </button>
                </div>
              {/if}
            </div>
            {#if mergeQueue.length > 0}
              <div class="queue-summary">
                <strong>{t('pulls.merge.queue_title')}</strong>
                {#each mergeQueue.slice(0, 5) as entry (entry.id)}
                  <span>#{entry.position} · PR #{entry.pr_number} · {entry.title}</span>
                {/each}
              </div>
            {/if}
          {/if}

          {#if timeline.length > 0}
            <section class="timeline">
              <h3>{t('pulls.timeline.title')}</h3>
              {#each timeline as event (event.id)}
                <article class="timeline-event">
                  <span class="timeline-dot"></span>
                  <div>
                    <div class="timeline-summary">
                      <strong>{event.actor?.username || t('pulls.timeline.system')}</strong>
                      <span>{t(`pulls.timeline.${event.kind}`, event.metadata || {})}</span>
                      <time>{formatDate(event.created_at)}</time>
                    </div>
                    {#if event.metadata?.path}
                      <code>{event.metadata.path}{event.metadata.line ? `:${event.metadata.start_line && event.metadata.start_line !== event.metadata.line ? `${event.metadata.start_line}-${event.metadata.line}` : event.metadata.line}` : ''}</code>
                    {/if}
                    {#if event.body}<div class="timeline-body">{event.body}</div>{/if}
                  </div>
                </article>
              {/each}
            </section>
          {/if}

          {#if applicableSuggestions.length > 1}
            <div class="suggestion-batch-bar">
              <span>{t('pulls.suggestion.batch_selected', { count: selectedSuggestionIds.length })}</span>
              <button
                class="btn-primary"
                disabled={applyingSuggestions || selectedSuggestionIds.length === 0}
                onclick={applySelectedSuggestions}
              >
                {applyingSuggestions ? t('pulls.suggestion.applying_selected') : t('pulls.suggestion.apply_selected')}
              </button>
            </div>
          {/if}

          {#if rootComments.length > 0}
            <section class="review-threads">
              <h3>{t('pulls.threads.title')}</h3>
              {#each rootComments as comment (comment.id)}
                <article class="thread" class:resolved={Boolean(comment.resolved_at)}>
                  <header>
                    <code>{comment.path}{comment.line ? `:${comment.start_line && comment.start_line !== comment.line ? `${comment.start_line}-${comment.line}` : comment.line}` : ''}</code>
                    <span>{comment.resolved_at ? t('pulls.threads.resolved') : t('pulls.threads.open')}</span>
                  </header>
                  <div class="thread-comment">{comment.body}</div>
                  <AttachmentPanel {owner} {repo} target="pulls/comments" targetId={comment.id} />
                  {#if comment.suggestion !== null && comment.suggestion !== undefined}
                    <div class="suggestion-block">
                      {#if !comment.suggestion_applied_at && comment.commit_id === pr.head_sha}
                        <label class="suggestion-select">
                          <input
                            type="checkbox"
                            checked={selectedSuggestionIds.includes(comment.id)}
                            onchange={() => toggleSuggestionSelection(comment.id)}
                          />
                          {t('pulls.suggestion.select')}
                        </label>
                      {/if}
                      {#if comment.suggestion === ''}
                        <em>{t('pulls.suggestion.delete_range')}</em>
                      {:else}
                        <code>{comment.suggestion}</code>
                      {/if}
                      {#if comment.suggestion_applied_at}
                        <span>{t('pulls.suggestion.applied')}</span>
                      {:else}
                        <button class="btn-secondary" disabled={applyingSuggestionId === comment.id} onclick={() => applySuggestion(comment)}>
                          {t('pulls.suggestion.apply')}
                        </button>
                      {/if}
                    </div>
                  {/if}
                  {#each repliesFor(comment.id) as reply (reply.id)}
                    <div class="thread-comment reply">{reply.body}</div>
                    <AttachmentPanel {owner} {repo} target="pulls/comments" targetId={reply.id} />
                  {/each}
                  <footer>
                    <button
                      class="btn-secondary"
                      disabled={resolvingCommentId === comment.id}
                      onclick={() => setThreadResolved(comment, !comment.resolved_at)}
                    >
                      {comment.resolved_at ? t('pulls.threads.reopen') : t('pulls.threads.resolve')}
                    </button>
                  </footer>
                </article>
              {/each}
            </section>
          {/if}
        </div>
      {/if}

      <!-- Diff tab -->
      {#if activeTab === 'diff'}
        <div class="diff-view">
          {#if diffData && diffData.files_changed.length > 0}
            <div class="diff-summary">
              <strong>{diffData.stats.files_changed} files</strong>
              <span class="addition-text">+{diffData.stats.total_additions}</span>
              <span class="deletion-text">−{diffData.stats.total_deletions}</span>
            </div>
            {#each diffData.files_changed as file (file.path)}
              <section class="diff-file">
                <header class="diff-file-header">
                  <code>{file.path}</code>
                  <span><span class="addition-text">+{file.additions}</span> <span class="deletion-text">−{file.deletions}</span></span>
                </header>
                <div class="diff-lines">
                  {#each file.lines as line, index (`${file.path}:${index}`)}
                    {@const target = commentLocation(file.path, line, index)}
                    {@const lineComments = target ? commentsForLine(file.path, target.line, target.side) : []}
                    <div class="diff-line" class:addition={line.kind === 'addition'} class:deletion={line.kind === 'deletion'} class:meta={line.kind === 'meta'}>
                      <span class="comment-gutter">
                        {#if target}
                          <button title={t('pulls.diff.add_comment')} aria-label={t('pulls.diff.add_comment')} onclick={() => startInlineComment(target, line.content)}>+</button>
                        {/if}
                      </span>
                      <span class="line-number">{line.old_line ?? ''}</span>
                      <span class="line-number">{line.new_line ?? ''}</span>
                      <code>{line.content || ' '}</code>
                    </div>
                    {#each lineComments as comment (comment.id)}
                      <div class="inline-thread" class:resolved={Boolean(comment.resolved_at)}>
                        <div>{comment.body}</div>
                        {#if comment.suggestion !== null && comment.suggestion !== undefined}
                          <div class="suggestion-block">
                            {#if comment.suggestion === ''}
                              <em>{t('pulls.suggestion.delete_range')}</em>
                            {:else}
                              <code>{comment.suggestion}</code>
                            {/if}
                            {#if comment.suggestion_applied_at}
                              <span>{t('pulls.suggestion.applied')}</span>
                            {:else}
                              <button class="btn-secondary" disabled={applyingSuggestionId === comment.id} onclick={() => applySuggestion(comment)}>
                                {t('pulls.suggestion.apply')}
                              </button>
                            {/if}
                          </div>
                        {/if}
                        {#each repliesFor(comment.id) as reply (reply.id)}
                          <div class="inline-reply">{reply.body}</div>
                        {/each}
                        <button class="btn-link" disabled={resolvingCommentId === comment.id} onclick={() => setThreadResolved(comment, !comment.resolved_at)}>
                          {comment.resolved_at ? t('pulls.threads.reopen') : t('pulls.threads.resolve')}
                        </button>
                      </div>
                    {/each}
                    {#if target && commentTarget?.key === target.key}
                      <div class="inline-comment-form">
                        <textarea bind:value={inlineCommentBody} rows="3" placeholder={t('pulls.diff.comment_placeholder')}></textarea>
                        {#if target.side === 'RIGHT'}
                          <label class="suggestion-toggle"><input type="checkbox" bind:checked={suggestingChange} /> {t('pulls.suggestion.propose')}</label>
                          {#if suggestingChange}
                            <label class="range-control">
                              {t('pulls.suggestion.line_count')}
                              <input type="number" min="1" max="100" bind:value={suggestionLineCount} />
                            </label>
                            <textarea bind:value={suggestedContent} rows="4" placeholder={t('pulls.suggestion.placeholder')}></textarea>
                          {/if}
                        {/if}
                        <div>
                          <button class="btn-primary" disabled={submittingInlineComment || !inlineCommentBody.trim()} onclick={submitInlineComment}>{t('pulls.diff.submit_comment')}</button>
                          <button class="btn-secondary" disabled={submittingInlineComment} onclick={() => commentTarget = null}>{t('common.cancel')}</button>
                        </div>
                      </div>
                    {/if}
                  {/each}
                </div>
              </section>
            {/each}
          {:else}
            <p class="text-secondary">{t('repo.browser.no_diff')}</p>
          {/if}
        </div>
      {/if}

      <!-- Review tab -->
      {#if activeTab === 'review'}
        <div class="review-form">
          <h3>{t('pulls.review.title')}</h3>
          <div class="verdict-select">
            <label class="radio-label">
              <input type="radio" name="verdict" value="comment" bind:group={reviewVerdict} />
              {t('pulls.review.verdict_comment')}
            </label>
            <label class="radio-label">
              <input type="radio" name="verdict" value="approve" bind:group={reviewVerdict} />
              {t('pulls.review.verdict_approve')}
            </label>
            <label class="radio-label">
              <input type="radio" name="verdict" value="request_changes" bind:group={reviewVerdict} />
              {t('pulls.review.verdict_changes')}
            </label>
          </div>
          <textarea bind:value={reviewBody} rows="4" placeholder={t('pulls.review.placeholder')}></textarea>
          <button class="btn-primary" onclick={handleSubmitReview} disabled={!reviewBody.trim()}>
            {t('pulls.review.submit')}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .pr-detail { max-width: 1200px; }

  .pr-header { margin-bottom: 20px; }
  h1 { font-size: 24px; }
  .pr-meta { display: flex; align-items: center; gap: 8px; margin-top: 8px; font-size: 13px; }

  .state-badge {
    padding: 2px 10px;
    border-radius: 12px;
    font-size: 12px;
    font-weight: 600;
  }
  .state-badge.open { background: rgba(63, 185, 80, 0.15); color: var(--green); }
  .state-badge.closed { background: rgba(248, 81, 73, 0.15); color: var(--red); }
  .state-badge.merged { background: rgba(188, 140, 255, 0.15); color: var(--purple); }
  .draft-badge { padding: 2px 8px; border: 1px solid var(--border); border-radius: 12px; color: var(--text-secondary); font-size: 12px; font-weight: 600; }
  .btn-link { padding: 0; border: none; background: none; color: var(--accent); cursor: pointer; }

  .branch-pair {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .branch-label {
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--accent);
  }

  .pr-body {
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
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .comment-body {
    padding: 16px;
    font-size: 14px;
    line-height: 1.6;
    white-space: pre-wrap;
  }

  .pr-tabs {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 16px;
  }

  .tab {
    padding: 8px 16px;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    font-size: 14px;
    cursor: pointer;
  }
  .tab.active { color: var(--text-primary); font-weight: 600; border-bottom-color: var(--orange); }

  .merge-box {
    background: var(--bg-secondary);
    border: 1px solid var(--green-dim);
    border-radius: var(--radius);
    padding: 16px;
    margin-bottom: 16px;
  }

  .reviewers-box, .review-threads { margin-bottom: 16px; padding: 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .reviewer-list { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; }
  .reviewer-chip { display: inline-flex; align-items: center; gap: 5px; padding: 3px 8px; border-radius: 12px; background: var(--bg-tertiary); font-size: 13px; }
  .reviewer-chip button { padding: 0; border: none; background: none; color: var(--text-secondary); cursor: pointer; }
  .reviewer-form { display: flex; gap: 8px; margin-top: 12px; }
  .reviewer-form input { flex: 1; min-width: 0; }
  .draft-notice { color: var(--text-secondary); }
  .auto-merge-pending { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .auto-merge-pending > div { display: flex; flex-direction: column; gap: 4px; }
  .auto-merge-pending small { color: var(--text-secondary); }
  .queue-summary { display: flex; flex-direction: column; gap: 4px; margin: -8px 0 16px; padding: 10px 16px; border: 1px solid var(--border); border-radius: var(--radius); color: var(--text-secondary); font-size: 12px; }
  .thread { margin-top: 12px; overflow: hidden; border: 1px solid var(--border); border-radius: var(--radius); }
  .thread.resolved { opacity: 0.72; }
  .thread header, .thread footer { display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; background: var(--bg-tertiary); font-size: 12px; }
  .thread-comment { padding: 12px; white-space: pre-wrap; }
  .thread-comment.reply { margin-left: 24px; border-top: 1px solid var(--border); }

  .merge-row {
    display: flex;
    gap: 8px;
  }

  .merge-select {
    padding: 6px 10px;
    font-size: 13px;
  }

  .btn-merge {
    padding: 6px 16px;
    background: var(--green-dim);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-merge:hover { background: var(--green); }
  .btn-merge:disabled { opacity: 0.5; }

  .diff-view {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .diff-summary { display: flex; gap: 10px; align-items: center; }
  .addition-text { color: var(--green); }
  .deletion-text { color: var(--red); }
  .diff-file { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .diff-file-header { display: flex; justify-content: space-between; padding: 10px 12px; background: var(--bg-tertiary); border-bottom: 1px solid var(--border); font-size: 13px; }
  .diff-lines { overflow-x: auto; }
  .diff-line {
    display: grid;
    grid-template-columns: 28px 48px 48px minmax(max-content, 1fr);
    min-height: 22px;
    font-size: 12px;
    line-height: 22px;
    background: var(--bg-primary);
  }
  .diff-line.addition { background: rgba(63, 185, 80, 0.12); }
  .diff-line.deletion { background: rgba(248, 81, 73, 0.12); }
  .diff-line.meta { background: rgba(88, 166, 255, 0.09); color: var(--text-secondary); }
  .diff-line > code { padding: 0 10px; white-space: pre; border-left: 1px solid var(--border); }
  .line-number { padding: 0 6px; color: var(--text-secondary); text-align: right; user-select: none; border-left: 1px solid var(--border); }
  .comment-gutter { display: flex; align-items: center; justify-content: center; }
  .comment-gutter button { width: 20px; height: 20px; padding: 0; border: 0; border-radius: 4px; background: transparent; color: transparent; cursor: pointer; }
  .diff-line:hover .comment-gutter button { color: #fff; background: var(--accent); }
  .inline-comment-form, .inline-thread { margin: 8px 12px 8px 124px; padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-secondary); }
  .inline-comment-form textarea { margin-bottom: 8px; }
  .inline-comment-form > div { display: flex; gap: 8px; }
  .inline-thread.resolved { opacity: .72; }
  .inline-reply { margin: 8px 0 8px 16px; padding-top: 8px; border-top: 1px solid var(--border); }
  .suggestion-toggle { display: flex; align-items: center; gap: 6px; margin-bottom: 8px; font-size: 13px; }
  .suggestion-batch-bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 16px; padding: 12px 16px; border: 1px solid var(--accent); border-radius: var(--radius); background: rgba(88, 166, 255, 0.08); }
  .suggestion-select { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-secondary); }
  .timeline { margin-bottom: 16px; padding: 16px; border: 1px solid var(--border); border-radius: var(--radius); }
  .timeline-event { display: grid; grid-template-columns: 14px 1fr; gap: 10px; padding: 10px 0; border-top: 1px solid var(--border); }
  .timeline-event:first-of-type { border-top: none; }
  .timeline-dot { width: 9px; height: 9px; margin-top: 6px; border-radius: 50%; background: var(--accent); }
  .timeline-summary { display: flex; flex-wrap: wrap; gap: 5px; align-items: baseline; }
  .timeline-summary time { margin-left: auto; color: var(--text-secondary); font-size: 12px; }
  .timeline-event code { display: inline-block; margin-top: 5px; }
  .timeline-body { margin-top: 7px; white-space: pre-wrap; color: var(--text-secondary); }
  .range-control { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; font-size: 13px; }
  .range-control input { width: 72px; }
  .suggestion-block { display: flex; flex-direction: column; gap: 8px; margin: 8px 12px; padding: 10px; border: 1px solid var(--green-dim); border-radius: var(--radius); background: rgba(63, 185, 80, 0.08); }
  .suggestion-block code { white-space: pre-wrap; }
  .suggestion-block button { align-self: flex-start; }

  .review-form {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 24px;
  }

  h3 { font-size: 16px; margin-bottom: 12px; }

  .verdict-select {
    display: flex;
    gap: 16px;
    margin-bottom: 12px;
  }

  .radio-label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 14px;
    cursor: pointer;
  }

  textarea {
    width: 100%;
    font-family: var(--font-mono);
    font-size: 13px;
    resize: vertical;
    margin-bottom: 12px;
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
</style>
