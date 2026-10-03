<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import AttachmentPanel from '$lib/components/AttachmentPanel.svelte';
  import BotBadge from '$lib/components/BotBadge.svelte';
  import { pulls, reviews } from '$lib/api/client.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { optionalSection, sectionOr } from '$lib/optionalSection';
  import type { DiffLine, MergeQueueEntry, PrDiff } from '$lib/api/pulls';
  import { createT, formatDate, formatTranslationFallback } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let number = $derived(parseInt($page.params.number!));
  let pr = $state<any>(null);
  let diffData = $state<PrDiff | null>(null);
  // Which optional sections of this page did not load. A read that failed is
  // not an empty answer: `pulls.diff` refusing with a 5xx used to land in
  // `diffData = null`, and the Diff tab drew that as `repo.browser.no_diff` —
  // "this pull request changes nothing", which is a claim about the branch,
  // made out of a git layer that never answered (card_c84bb28a36e1). The five
  // list slots below it read the same way: "no reviews yet" and "the review
  // list could not be read" are different facts about a pull request, and only
  // one of them is something the reader can act on.
  let unavailableSections = $state<string[]>([]);
  let diffUnavailable = $derived(unavailableSections.includes('diff'));
  let unavailableLabels = $derived(
    unavailableSections
      .map((section) => t(`pulls.unavailable.section.${section}`, undefined, formatTranslationFallback(section)))
      .join(', '),
  );
  let reviewList = $state<any[]>([]);
  let reviewComments = $state<any[]>([]);
  let timeline = $state<any[]>([]);
  let requestedReviewers = $state<Array<{ id: number; reviewer_id: number; username: string; requested_by_id: number; created_at: string }>>([]);
  let mergeQueue = $state<MergeQueueEntry[]>([]);
  let loading = $state(true);
  let mutationBusy = $state(false);
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
  let dismissTargetId = $state<number | null>(null);
  let dismissMessage = $state('');
  let dismissingReviewId = $state<number | null>(null);
  // Reviews keyed by id so a timeline entry can find the row its verdict lives
  // on. The timeline says *what happened*; whether that verdict still counts is
  // a property of the review row (`dismissed_at`), which is what the merge gate
  // reads (card_dc0f5d58e5f4).
  let reviewById = $derived(new Map<number, any>(reviewList.map((review) => [review.id, review])));
  let approvingCi = $state(false);
  const pullRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;
  // A pipeline runs under the *base* repository's id and is handed that
  // repository's CI secrets, so `trigger_pull_request_ci` refuses a fork head
  // until a maintainer has vouched for this exact commit. Both halves matter:
  // `head_repo_id` is what makes it a fork, and the SHA comparison is what
  // makes the approval expire on the next push to the fork — an approval that
  // outlived the diff it was given for would be worth nothing
  // (card_3c0751fbf09d).
  let ciHeldForFork = $derived(
    pr?.state === 'open' &&
    pr?.head_repo_id !== null && pr?.head_repo_id !== undefined &&
    !!pr?.head_sha &&
    pr?.ci_approved_sha !== pr?.head_sha
  );

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedNumber = number;
    routeGeneration += 1;
    pr = null;
    diffData = null;
    unavailableSections = [];
    reviewList = [];
    reviewComments = [];
    timeline = [];
    requestedReviewers = [];
    mergeQueue = [];
    selectedSuggestionIds = [];
    activeTab = 'conversation';
    mergeStrategy = 'merge';
    autoMergeReason = '';
    reviewBody = '';
    reviewVerdict = 'comment';
    reviewerUsername = '';
    commentTarget = null;
    inlineCommentBody = '';
    suggestingChange = false;
    suggestedContent = '';
    suggestionLineCount = 1;
    dismissTargetId = null;
    dismissMessage = '';
    mutationBusy = false;
    updatingDraft = false;
    approvingCi = false;
    managingReviewer = false;
    resolvingCommentId = null;
    submittingInlineComment = false;
    applyingSuggestionId = null;
    applyingSuggestions = false;
    merging = false;
    managingAutoMerge = false;
    managingMergeQueue = false;
    dismissingReviewId = null;
    error = '';
    loading = true;
    void loadPR(expectedOwner, expectedRepo, expectedNumber, routeGeneration);
  });

  type PullRoute = Readonly<{
    owner: string;
    repo: string;
    number: number;
    generation: number;
  }>;

  function currentRoute(): PullRoute {
    return { owner, repo, number, generation: routeGeneration };
  }

  function isCurrentRoute(route: PullRoute) {
    return (
      routeGeneration === route.generation &&
      owner === route.owner &&
      repo === route.repo &&
      number === route.number
    );
  }

  async function runMutation(
    work: (route: PullRoute) => Promise<void>,
    setOperationBusy?: (busy: boolean) => void,
  ) {
    if (mutationBusy) return;
    const route = currentRoute();
    pullRequests.begin(route.owner, route.repo, route.number);
    mutationBusy = true;
    setOperationBusy?.(true);
    error = '';
    try {
      await work(route);
    } catch (e: any) {
      if (isCurrentRoute(route)) error = e.message;
    } finally {
      if (isCurrentRoute(route)) {
        setOperationBusy?.(false);
        mutationBusy = false;
      }
    }
  }

  function reloadPR() {
    void loadPR(owner, repo, number, routeGeneration);
  }

  async function loadPR(
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
    const claim = pullRequests.begin(expectedOwner, expectedRepo, expectedNumber);
    try {
      loading = true;
      error = '';
      const [prData, diffResult, reviewResult, commentsResult, timelineResult, reviewersResult, queueResult] = await Promise.all([
        pulls.get(expectedOwner, expectedRepo, expectedNumber),
        optionalSection(pulls.diff(expectedOwner, expectedRepo, expectedNumber), 'the diff of this pull request'),
        optionalSection(reviews.list(expectedOwner, expectedRepo, expectedNumber), 'the reviews of this pull request'),
        optionalSection(reviews.comments(expectedOwner, expectedRepo, expectedNumber), 'the review comments of this pull request'),
        optionalSection(reviews.timeline(expectedOwner, expectedRepo, expectedNumber), 'the timeline of this pull request'),
        optionalSection(reviews.requestedReviewers(expectedOwner, expectedRepo, expectedNumber), 'the requested reviewers of this pull request'),
        optionalSection(pulls.mergeQueue(expectedOwner, expectedRepo), 'the merge queue of this repository'),
      ]);
      if (!pullRequests.owns(claim, owner, repo, number) || !isCurrentRoute(route)) return;
      const missing: string[] = [];
      pr = prData;
      diffData = sectionOr(diffResult, 'diff', null, missing);
      reviewList = sectionOr(reviewResult, 'reviews', [], missing) || [];
      reviewComments = sectionOr(commentsResult, 'comments', [], missing) || [];
      timeline = sectionOr(timelineResult, 'timeline', [], missing) || [];
      selectedSuggestionIds = selectedSuggestionIds.filter((id) =>
        reviewComments.some((comment) => comment.id === id && !comment.suggestion_applied_at && comment.commit_id === prData.head_sha)
      );
      requestedReviewers = sectionOr(reviewersResult, 'reviewers', [], missing) || [];
      mergeQueue = sectionOr(queueResult, 'merge_queue', [], missing) || [];
      unavailableSections = missing;
    } catch (e: any) {
      if (pullRequests.owns(claim, owner, repo, number) && isCurrentRoute(route)) {
        error = e.message;
      }
    } finally {
      if (pullRequests.owns(claim, owner, repo, number) && isCurrentRoute(route)) {
        loading = false;
      }
    }
  }

  async function toggleDraft() {
    const nextDraft = !pr.is_draft;
    await runMutation(
      async (route) => {
        const next = await pulls.update(route.owner, route.repo, route.number, { draft: nextDraft });
        if (isCurrentRoute(route)) pr = next;
      },
      (busy) => updatingDraft = busy,
    );
  }

  async function approveForkCi() {
    await runMutation(
      async (route) => {
        await pulls.approveCi(route.owner, route.repo, route.number);
        if (!isCurrentRoute(route)) return;
        // The server starts the run this unblocks, so re-read the PR: the banner
        // has to disappear on the same interaction that made it stale.
        await loadPR(route.owner, route.repo, route.number, route.generation);
      },
      (busy) => approvingCi = busy,
    );
  }

  async function requestReviewer() {
    if (!reviewerUsername.trim()) return;
    const username = reviewerUsername.trim();
    await runMutation(
      async (route) => {
        await reviews.requestReviewer(route.owner, route.repo, route.number, username);
        if (!isCurrentRoute(route)) return;
        const nextReviewers = await reviews.requestedReviewers(route.owner, route.repo, route.number);
        if (!isCurrentRoute(route)) return;
        reviewerUsername = '';
        requestedReviewers = nextReviewers;
      },
      (busy) => managingReviewer = busy,
    );
  }

  async function removeReviewer(username: string) {
    await runMutation(
      async (route) => {
        await reviews.removeRequestedReviewer(route.owner, route.repo, route.number, username);
        if (isCurrentRoute(route)) {
          requestedReviewers = requestedReviewers.filter((reviewer) => reviewer.username !== username);
        }
      },
      (busy) => managingReviewer = busy,
    );
  }

  async function setThreadResolved(comment: any, resolved: boolean) {
    await runMutation(
      async (route) => {
        await reviews.setThreadResolved(route.owner, route.repo, route.number, comment.id, resolved);
        if (!isCurrentRoute(route)) return;
        const nextComments = await reviews.comments(route.owner, route.repo, route.number);
        if (isCurrentRoute(route)) reviewComments = nextComments;
      },
      (busy) => resolvingCommentId = busy ? comment.id : null,
    );
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
    const target = commentTarget;
    const body = inlineCommentBody.trim();
    const isSuggestion = suggestingChange;
    const suggestion = suggestedContent;
    const rangeLength = Math.min(100, Math.max(1, Number(suggestionLineCount) || 1));
    await runMutation(
      async (route) => {
        await reviews.addComment(route.owner, route.repo, route.number, {
          path: target.path,
          line: isSuggestion ? target.line + rangeLength - 1 : target.line,
          start_line: isSuggestion ? target.line : undefined,
          side: target.side,
          start_side: isSuggestion ? target.side : undefined,
          body,
          suggestion: isSuggestion ? suggestion : undefined,
        });
        if (!isCurrentRoute(route)) return;
        const nextComments = await reviews.comments(route.owner, route.repo, route.number);
        if (!isCurrentRoute(route)) return;
        reviewComments = nextComments;
        commentTarget = null;
        inlineCommentBody = '';
        suggestingChange = false;
        suggestedContent = '';
        suggestionLineCount = 1;
      },
      (busy) => submittingInlineComment = busy,
    );
  }

  async function applySuggestion(comment: any) {
    await runMutation(
      async (route) => {
        await reviews.applySuggestion(route.owner, route.repo, route.number, comment.id);
        if (isCurrentRoute(route)) {
          await loadPR(route.owner, route.repo, route.number, route.generation);
        }
      },
      (busy) => applyingSuggestionId = busy ? comment.id : null,
    );
  }

  function toggleSuggestionSelection(commentId: number) {
    selectedSuggestionIds = selectedSuggestionIds.includes(commentId)
      ? selectedSuggestionIds.filter((id) => id !== commentId)
      : [...selectedSuggestionIds, commentId];
  }

  async function applySelectedSuggestions() {
    if (selectedSuggestionIds.length === 0) return;
    const suggestionIds = [...selectedSuggestionIds];
    await runMutation(
      async (route) => {
        await reviews.applySuggestions(route.owner, route.repo, route.number, suggestionIds);
        if (!isCurrentRoute(route)) return;
        selectedSuggestionIds = [];
        await loadPR(route.owner, route.repo, route.number, route.generation);
      },
      (busy) => applyingSuggestions = busy,
    );
  }

  async function handleMerge() {
    const strategy = mergeStrategy;
    await runMutation(
      async (route) => {
        await pulls.merge(route.owner, route.repo, route.number, strategy);
        if (isCurrentRoute(route)) {
          await loadPR(route.owner, route.repo, route.number, route.generation);
        }
      },
      (busy) => merging = busy,
    );
  }

  async function enableAutoMerge() {
    const strategy = mergeStrategy;
    await runMutation(
      async (route) => {
        const outcome = await pulls.enableAutoMerge(route.owner, route.repo, route.number, strategy);
        if (!isCurrentRoute(route)) return;
        autoMergeReason = outcome.reason || '';
        await loadPR(route.owner, route.repo, route.number, route.generation);
      },
      (busy) => managingAutoMerge = busy,
    );
  }

  async function disableAutoMerge() {
    await runMutation(
      async (route) => {
        const next = await pulls.disableAutoMerge(route.owner, route.repo, route.number);
        if (!isCurrentRoute(route)) return;
        pr = next;
        autoMergeReason = '';
      },
      (busy) => managingAutoMerge = busy,
    );
  }

  async function enqueueMerge() {
    const strategy = mergeStrategy;
    await runMutation(
      async (route) => {
        await pulls.enqueueMerge(route.owner, route.repo, route.number, strategy);
        if (isCurrentRoute(route)) {
          await loadPR(route.owner, route.repo, route.number, route.generation);
        }
      },
      (busy) => managingMergeQueue = busy,
    );
  }

  async function cancelQueuedMerge() {
    await runMutation(
      async (route) => {
        await pulls.cancelQueuedMerge(route.owner, route.repo, route.number);
        if (isCurrentRoute(route)) {
          await loadPR(route.owner, route.repo, route.number, route.generation);
        }
      },
      (busy) => managingMergeQueue = busy,
    );
  }

  function startDismissal(reviewId: number) {
    dismissTargetId = reviewId;
    dismissMessage = '';
  }

  function cancelDismissal() {
    dismissTargetId = null;
    dismissMessage = '';
  }

  /**
   * Withdraw a review that is still counting toward branch protection.
   *
   * Reloads the whole pull request afterwards rather than patching the one row:
   * the dismissal also writes a `review_dismiss` timeline event and changes
   * what the merge box is allowed to do, and those three views must not
   * disagree about whether the approval still stands.
   */
  async function dismissReview(reviewId: number) {
    const message = dismissMessage.trim();
    await runMutation(
      async (route) => {
        await reviews.dismiss(route.owner, route.repo, route.number, reviewId, message);
        if (!isCurrentRoute(route)) return;
        cancelDismissal();
        await loadPR(route.owner, route.repo, route.number, route.generation);
      },
      (busy) => dismissingReviewId = busy ? reviewId : null,
    );
  }

  async function handleSubmitReview() {
    const body = reviewBody;
    const verdict = reviewVerdict;
    await runMutation(async (route) => {
      await reviews.submit(route.owner, route.repo, route.number, body, verdict);
      if (!isCurrentRoute(route)) return;
      reviewBody = '';
      reviewVerdict = 'comment';
      await loadPR(route.owner, route.repo, route.number, route.generation);
    });
  }

</script>

<svelte:head>
  <title>PR #{number} · {owner}/{repo} · Plombir Git</title>
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
      <!--
        The parts of the page that did not answer, named. Without this the only
        trace of a failed side request was the shape of an empty section, which
        reads as a fact about the pull request instead of as a missing answer
        (card_c84bb28a36e1).
      -->
      {#if unavailableSections.length > 0}
        <div class="partial-banner" role="status">
          <span>{t('pulls.unavailable.notice')} {unavailableLabels}</span>
          <button class="btn-link" onclick={reloadPR} disabled={loading}>{t('common.retry')}</button>
        </div>
      {/if}

      <!-- Header -->
      <div class="pr-header">
        <h1>{pr.title}</h1>
        <div class="pr-meta">
          <span class="state-badge" class:open={pr.state === 'open'} class:closed={pr.state === 'closed'} class:merged={pr.state === 'merged'}>
            {t(`pulls.state.${pr.state}`, undefined, formatTranslationFallback(pr.state))}
          </span>
          {#if pr.is_draft}<span class="draft-badge">{t('pulls.draft')}</span>{/if}
          <span class="text-secondary">
            opened {formatDate(pr.created_at)} by <strong>{pr.author || t('common.unknown')}</strong><BotBadge owner={pr.author_bot_owner} />
          </span>
          <span class="branch-pair">
            <span class="branch-label">{pr.head_branch}</span>
            →
            <span class="branch-label">{pr.base_branch}</span>
          </span>
          {#if pr.state === 'open'}
            <button class="btn-link" onclick={toggleDraft} disabled={mutationBusy || updatingDraft}>
              {pr.is_draft ? t('pulls.mark_ready') : t('pulls.convert_draft')}
            </button>
          {/if}
        </div>
      </div>

      {#if pr.body}
        <div class="pr-body">
          <div class="comment-header">
            <strong>{pr.author || t('common.unknown')}</strong><BotBadge owner={pr.author_bot_owner} /> commented
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
                      disabled={mutationBusy || managingReviewer}
                      onclick={() => removeReviewer(reviewer.username)}
                    >×</button>
                  </span>
                {/each}
              </div>
            {/if}
            <div class="reviewer-form">
              <input bind:value={reviewerUsername} placeholder={t('pulls.reviewers.placeholder')} disabled={mutationBusy} />
              <button class="btn-secondary" onclick={requestReviewer} disabled={mutationBusy || managingReviewer || !reviewerUsername.trim()}>
                {t('pulls.reviewers.request')}
              </button>
            </div>
          </section>

          <!-- Fork CI approval -->
          {#if ciHeldForFork}
            <div class="ci-held">
              <div>
                <strong>{t('pulls.fork_ci.held')}</strong>
                <span>{t('pulls.fork_ci.explanation')}</span>
              </div>
              <button class="btn-secondary ci-approve" onclick={approveForkCi} disabled={mutationBusy || approvingCi}>
                {approvingCi ? t('pulls.fork_ci.approving') : t('pulls.fork_ci.approve')}
              </button>
            </div>
          {/if}

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
                  <button class="btn-secondary" onclick={cancelQueuedMerge} disabled={mutationBusy || managingMergeQueue || queuedEntry.status === 'running'}>
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
                  <button class="btn-secondary" onclick={disableAutoMerge} disabled={mutationBusy || managingAutoMerge}>
                    {t('pulls.merge.disable_auto')}
                  </button>
                </div>
              {:else}
                <div class="merge-row">
                  <select bind:value={mergeStrategy} class="merge-select" disabled={mutationBusy}>
                    <option value="merge">{t('pulls.merge.strategy.merge')}</option>
                    <option value="squash">{t('pulls.merge.strategy.squash')}</option>
                    <option value="rebase">{t('pulls.merge.strategy.rebase')}</option>
                  </select>
                  <button class="btn-merge" onclick={handleMerge} disabled={mutationBusy || merging}>
                    {merging ? t('pulls.merge.merging') : t('pulls.merge.button')}
                  </button>
                  <button class="btn-secondary" onclick={enableAutoMerge} disabled={mutationBusy || managingAutoMerge}>
                    {managingAutoMerge ? t('pulls.merge.enabling_auto') : t('pulls.merge.enable_auto')}
                  </button>
                  <button class="btn-secondary" onclick={enqueueMerge} disabled={mutationBusy || managingMergeQueue}>
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
                <!--
                  The review row behind a verdict entry, when this entry is one.
                  Only `approve` / `request_changes` are folded into
                  `count_current_approvals`, so only those two can be standing
                  or withdrawn; a `comment` review has no verdict to take back.
                -->
                {@const verdict = event.kind === 'review_approve' || event.kind === 'review_request_changes'
                  ? reviewById.get(event.metadata?.review_id)
                  : undefined}
                <article class="timeline-event">
                  <span class="timeline-dot"></span>
                  <div>
                    <div class="timeline-summary">
                      <strong>{event.actor?.username || t('pulls.timeline.system')}</strong>
                      <span>{t(`pulls.timeline.${event.kind}`, event.metadata || {}, formatTranslationFallback(event.kind))}</span>
                      {#if verdict?.dismissed_at}
                        <span class="withdrawn-badge">{t('pulls.review.withdrawn')}</span>
                      {/if}
                      <time>{formatDate(event.created_at)}</time>
                    </div>
                    {#if event.metadata?.path}
                      <code>{event.metadata.path}{event.metadata.line ? `:${event.metadata.start_line && event.metadata.start_line !== event.metadata.line ? `${event.metadata.start_line}-${event.metadata.line}` : event.metadata.line}` : ''}</code>
                    {/if}
                    {#if event.body}<div class="timeline-body">{event.body}</div>{/if}
                    {#if verdict && !verdict.dismissed_at && pr.state === 'open'}
                      {#if dismissTargetId === verdict.id}
                        <div class="dismiss-form">
                          <input bind:value={dismissMessage} placeholder={t('pulls.review.dismiss_placeholder')} disabled={mutationBusy} />
                          <button
                            class="btn-secondary"
                            disabled={mutationBusy || dismissingReviewId === verdict.id || !dismissMessage.trim()}
                            onclick={() => dismissReview(verdict.id)}
                          >
                            {dismissingReviewId === verdict.id ? t('pulls.review.dismissing') : t('pulls.review.dismiss_confirm')}
                          </button>
                          <button class="btn-link" onclick={cancelDismissal}>{t('pulls.review.dismiss_cancel')}</button>
                        </div>
                      {:else}
                        <button class="btn-link dismiss-review" onclick={() => startDismissal(verdict.id)}>
                          {t('pulls.review.dismiss')}
                        </button>
                      {/if}
                    {/if}
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
                disabled={mutationBusy || applyingSuggestions || selectedSuggestionIds.length === 0}
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
                            disabled={mutationBusy}
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
                        <button class="btn-secondary" disabled={mutationBusy || applyingSuggestionId === comment.id} onclick={() => applySuggestion(comment)}>
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
                      disabled={mutationBusy || resolvingCommentId === comment.id}
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
                          <button title={t('pulls.diff.add_comment')} aria-label={t('pulls.diff.add_comment')} onclick={() => startInlineComment(target, line.content)} disabled={mutationBusy}>+</button>
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
                              <button class="btn-secondary" disabled={mutationBusy || applyingSuggestionId === comment.id} onclick={() => applySuggestion(comment)}>
                                {t('pulls.suggestion.apply')}
                              </button>
                            {/if}
                          </div>
                        {/if}
                        {#each repliesFor(comment.id) as reply (reply.id)}
                          <div class="inline-reply">{reply.body}</div>
                        {/each}
                        <button class="btn-link" disabled={mutationBusy || resolvingCommentId === comment.id} onclick={() => setThreadResolved(comment, !comment.resolved_at)}>
                          {comment.resolved_at ? t('pulls.threads.reopen') : t('pulls.threads.resolve')}
                        </button>
                      </div>
                    {/each}
                    {#if target && commentTarget?.key === target.key}
                      <div class="inline-comment-form">
                        <textarea bind:value={inlineCommentBody} rows="3" placeholder={t('pulls.diff.comment_placeholder')} disabled={mutationBusy}></textarea>
                        {#if target.side === 'RIGHT'}
                          <label class="suggestion-toggle"><input type="checkbox" bind:checked={suggestingChange} disabled={mutationBusy} /> {t('pulls.suggestion.propose')}</label>
                          {#if suggestingChange}
                            <label class="range-control">
                              {t('pulls.suggestion.line_count')}
                              <input type="number" min="1" max="100" bind:value={suggestionLineCount} disabled={mutationBusy} />
                            </label>
                            <textarea bind:value={suggestedContent} rows="4" placeholder={t('pulls.suggestion.placeholder')} disabled={mutationBusy}></textarea>
                          {/if}
                        {/if}
                        <div>
                          <button class="btn-primary" disabled={mutationBusy || submittingInlineComment || !inlineCommentBody.trim()} onclick={submitInlineComment}>{t('pulls.diff.submit_comment')}</button>
                          <button class="btn-secondary" disabled={submittingInlineComment} onclick={() => commentTarget = null}>{t('common.cancel')}</button>
                        </div>
                      </div>
                    {/if}
                  {/each}
                </div>
              </section>
            {/each}
          {:else if diffUnavailable}
            <div class="diff-unavailable">
              <p>{t('pulls.diff.unavailable')}</p>
              <button class="btn-secondary" onclick={reloadPR} disabled={loading}>{t('common.retry')}</button>
            </div>
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
              <input type="radio" name="verdict" value="comment" bind:group={reviewVerdict} disabled={mutationBusy} />
              {t('pulls.review.verdict_comment')}
            </label>
            <label class="radio-label">
              <input type="radio" name="verdict" value="approve" bind:group={reviewVerdict} disabled={mutationBusy} />
              {t('pulls.review.verdict_approve')}
            </label>
            <label class="radio-label">
              <input type="radio" name="verdict" value="request_changes" bind:group={reviewVerdict} disabled={mutationBusy} />
              {t('pulls.review.verdict_changes')}
            </label>
          </div>
          <textarea bind:value={reviewBody} rows="4" placeholder={t('pulls.review.placeholder')} disabled={mutationBusy}></textarea>
          <button class="btn-primary" onclick={handleSubmitReview} disabled={mutationBusy || !reviewBody.trim()}>
            {t('pulls.review.submit')}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .pr-detail { max-width: 1200px; }

  .partial-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 16px;
    padding: 8px 12px;
    border: 1px solid var(--yellow, var(--border));
    border-radius: var(--radius);
    background: var(--bg-secondary);
    font-size: 13px;
  }

  .diff-unavailable {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

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
  .ci-held { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin: 16px 0; padding: 12px 16px; border: 1px solid var(--warning, var(--border)); border-radius: var(--radius); background: var(--bg-tertiary); }
  .ci-held > div { display: flex; flex-direction: column; gap: 4px; }
  .ci-held span { color: var(--text-secondary); font-size: 12px; }
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
  .withdrawn-badge { padding: 1px 7px; border: 1px solid var(--border); border-radius: 12px; color: var(--text-secondary); font-size: 11px; font-weight: 600; }
  .dismiss-review { margin-top: 6px; font-size: 13px; }
  .dismiss-form { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-top: 8px; }
  .dismiss-form input { flex: 1; min-width: 180px; }
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
