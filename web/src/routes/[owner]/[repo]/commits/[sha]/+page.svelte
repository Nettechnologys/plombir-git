<script lang="ts">
  import { page } from '$app/stores';
  import { repos, type CommitSignature } from '$lib/api/client.svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { isUnavailable, optionalSection } from '$lib/optionalSection';

  // Svelte 5 runes
  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let sha = $derived(($page.params as any).sha as string);

  let loading = $state(true);
  let error = $state<string | null>(null);
  let commitInfo = $state<{ sha: string; message: string; author: string; date: string } | null>(null);
  let combinedStatus = $state<any | null>(null);
  let statuses = $state<any[]>([]);
  let gpgSignature = $state<CommitSignature | null>(null);
  // "The server told me this commit is unsigned" and "I never got an answer
  // about this commit's signature" are different facts. Swallowing the second
  // into `gpgSignature = null` drew a signed commit on an instance whose git
  // gateway was down exactly like a commit nobody ever signed — the badge
  // simply was not there (card_c84bb28a36e1).
  let signatureUnavailable = $state(false);
  const commitRequests = new LatestRepositoryResourceRequestFence<string>();

  // Fetch data on mount and when params change
  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedSha = sha;
    if (expectedOwner && expectedRepo && expectedSha) {
      void loadData(expectedOwner, expectedRepo, expectedSha);
    }
  });

  async function loadData(expectedOwner: string, expectedRepo: string, expectedSha: string) {
    const claim = commitRequests.begin(expectedOwner, expectedRepo, expectedSha);
    loading = true;
    error = null;
    commitInfo = null;
    combinedStatus = null;
    statuses = [];
    gpgSignature = null;
    signatureUnavailable = false;

    try {
      const [combinedResult, statusesResult, logResult, signatureResult] = await Promise.all([
        repos.getCombinedStatus(expectedOwner, expectedRepo, expectedSha),
        repos.listCommitStatuses(expectedOwner, expectedRepo, expectedSha),
        repos.log(expectedOwner, expectedRepo, expectedSha).catch((logErr: unknown) => {
          console.warn('Could not fetch commit info from log:', logErr);
          return null;
        }),
        optionalSection(
          repos.commitSignature(expectedOwner, expectedRepo, expectedSha),
          "this commit's signature",
        ),
      ]);

      const logCommits = logResult?.commits ?? [];
      let nextCommitInfo = null;
      if (logCommits.length > 0) {
        nextCommitInfo = logCommits.find(
          (commit: any) => commit.sha.startsWith(expectedSha) || expectedSha.startsWith(commit.sha),
        ) || logCommits[0];
      }

      // If we couldn't get commit info, create a minimal version from sha
      if (!nextCommitInfo) {
        nextCommitInfo = {
          sha: expectedSha,
          message: expectedSha,
          author: 'Unknown',
          date: new Date().toISOString()
        };
      }

      if (!commitRequests.owns(claim, owner, repo, sha)) return;
      combinedStatus = combinedResult;
      statuses = statusesResult;
      commitInfo = nextCommitInfo;
      if (isUnavailable(signatureResult)) {
        gpgSignature = null;
        signatureUnavailable = true;
      } else {
        gpgSignature = signatureResult;
        signatureUnavailable = false;
      }
    } catch (err: any) {
      if (commitRequests.owns(claim, owner, repo, sha)) {
        error = err.message || 'Failed to load commit status';
        console.error('Error loading commit status:', err);
      }
    } finally {
      if (commitRequests.owns(claim, owner, repo, sha)) loading = false;
    }
  }

  function retryLoad() {
    void loadData(owner, repo, sha);
  }

  // Helper functions
  function getShortSha(fullSha: string): string {
    return fullSha.substring(0, 8);
  }

  function formatDate(dateStr: string): string {
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMins / 60);
    const diffDays = Math.floor(diffHours / 24);

    if (diffMins < 1) return 'just now';
    if (diffMins < 60) return `${diffMins} minutes ago`;
    if (diffHours < 24) return `${diffHours} hours ago`;
    if (diffDays < 7) return `${diffDays} days ago`;

    return date.toLocaleDateString('en-US', {
      year: 'numeric',
      month: 'short',
      day: 'numeric'
    });
  }

  function getStatusIcon(state: string): string {
    switch (state) {
      case 'success': return '✅';
      case 'failure': return '❌';
      case 'error': return '❌';
      case 'pending': return '⏳';
      default: return '❓';
    }
  }

  function getStatusText(state: string): string {
    switch (state) {
      case 'success': return 'All checks passed';
      case 'failure': return 'Some checks failed';
      case 'error': return 'Some checks errored';
      case 'pending': return 'Checks pending';
      default: return 'Unknown status';
    }
  }

  function getStatusColor(state: string): string {
    switch (state) {
      case 'success': return 'var(--green)';
      case 'failure': return 'var(--red)';
      case 'error': return 'var(--orange)';
      case 'pending': return 'var(--yellow)';
      default: return 'var(--text-muted)';
    }
  }
</script>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="commits" />

  <div class="commit-status-page">
  {#if loading}
    <div class="loading-container">
      <div class="spinner"></div>
      <p>Loading commit status...</p>
    </div>
  {:else if error}
    <div class="error-container">
      <p class="error-message">Error: {error}</p>
      <button onclick={retryLoad}>Retry</button>
    </div>
  {:else if commitInfo}
    <!-- Commit Info Section -->
    <div class="commit-info">
      <div class="commit-header">
        <h1 class="commit-title">{commitInfo.message}</h1>
        <div class="commit-sha">
          <code>{getShortSha(commitInfo.sha)}</code>
        </div>
      </div>
      <div class="commit-meta">
        <span class="commit-author">{commitInfo.author}</span>
        <span class="commit-date">{formatDate(commitInfo.date)}</span>
        <!-- GPG Signature Badge -->
        <!--
          Four states, because the server reports four. `undeterminable` is the
          one this badge used to swallow: it means the check never ran — this
          instance holds no key for the signer — and drawing it as "Bad
          signature" accuses a commit that nothing is wrong with
          (card_61b29791d099).
        -->
        {#if signatureUnavailable}
          <span class="gpg-badge unchecked" title="GPG: the signature check could not be reached">
            <span class="gpg-icon">!</span> Signature status unavailable
          </span>
        {:else if gpgSignature}
          <span class="gpg-badge" class:verified={gpgSignature.verdict === 'valid'} class:unverified={gpgSignature.verdict === 'invalid'} class:unchecked={gpgSignature.verdict === 'undeterminable'} class:no-sig={gpgSignature.verdict === 'unsigned'} title="GPG: {gpgSignature.status}{gpgSignature.signer_name ? ' · ' + gpgSignature.signer_name : ''}">
            {#if gpgSignature.verdict === 'valid'}
              <span class="gpg-icon">✓</span> Signed
              {#if gpgSignature.signer_name}
                <span class="gpg-signer">by {gpgSignature.signer_name}</span>
              {/if}
            {:else if gpgSignature.verdict === 'unsigned'}
              <span class="gpg-icon">○</span> Unsigned
            {:else if gpgSignature.verdict === 'undeterminable'}
              <span class="gpg-icon">?</span> Signature could not be checked
              {#if gpgSignature.signer_name}
                <span class="gpg-signer">by {gpgSignature.signer_name}</span>
              {/if}
            {:else}
              <span class="gpg-icon">✗</span> Bad signature
            {/if}
          </span>
        {/if}
      </div>
    </div>

    <!-- Combined Status Badge -->
    {#if combinedStatus}
      <div class="combined-status" style="border-left-color: {getStatusColor(combinedStatus.state)}">
        <div class="status-icon-large">
          {getStatusIcon(combinedStatus.state)}
        </div>
        <div class="status-content">
          <h2 class="status-title">{getStatusText(combinedStatus.state)}</h2>
          <p class="status-count">{combinedStatus.total_count} checks</p>
        </div>
      </div>
    {/if}

    <!-- Status Checks List -->
    <div class="status-checks">
      <h3>Status Checks</h3>

      {#if statuses.length === 0}
        <div class="empty-state">
          <p>No status checks reported yet.</p>
        </div>
      {:else}
        <div class="status-list">
          {#each statuses as status, index}
            <div class="status-card" class:alt-bg={index % 2 === 1}>
              <div class="status-card-icon">
                {getStatusIcon(status.state)}
              </div>
              <div class="status-card-content">
                <div class="status-card-header">
                  <strong class="status-context">{status.context}</strong>
                  <span class="status-date">{formatDate(status.created_at)}</span>
                </div>
                <p class="status-description">{status.description || 'No description'}</p>
                {#if status.target_url}
                  <a href={status.target_url} target="_blank" rel="noopener noreferrer" class="status-details-link">
                    View Details →
                  </a>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
  </div>
</div>

<style>
  .commit-status-page {
    color: var(--text-primary);
  }

  /* Loading State */
  .loading-container {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 400px;
    gap: 1rem;
    color: var(--text-secondary);
  }

  .spinner {
    width: 40px;
    height: 40px;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  /* Error State */
  .error-container {
    text-align: center;
    padding: 3rem;
    color: var(--text-secondary);
  }

  .error-message {
    color: var(--red);
    margin-bottom: 1rem;
  }

  /* Commit Info Section */
  .commit-info {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .commit-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .commit-title {
    font-size: 1.5rem;
    font-weight: 600;
    margin: 0;
    color: var(--text-primary);
    flex: 1;
  }

  .commit-sha code {
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
    font-size: 0.875rem;
    background: var(--bg-primary);
    padding: 0.25rem 0.5rem;
    border-radius: var(--radius);
    color: var(--accent);
    border: 1px solid var(--border);
  }

  .commit-meta {
    display: flex;
    gap: 1rem;
    font-size: 0.875rem;
    color: var(--text-secondary);
  }

  .commit-author {
    font-weight: 500;
  }

  /* Combined Status Badge */
  .combined-status {
    display: flex;
    align-items: center;
    gap: 1rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-left: 4px solid;
    border-radius: var(--radius);
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .status-icon-large {
    font-size: 2rem;
    line-height: 1;
  }

  .status-content {
    flex: 1;
  }

  .status-title {
    font-size: 1.25rem;
    font-weight: 600;
    margin: 0 0 0.25rem 0;
    color: var(--text-primary);
  }

  .status-count {
    font-size: 0.875rem;
    color: var(--text-secondary);
    margin: 0;
  }

  /* Status Checks List */
  .status-checks {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 1.5rem;
  }

  .status-checks h3 {
    font-size: 1.125rem;
    font-weight: 600;
    margin: 0 0 1rem 0;
    color: var(--text-primary);
  }

  .empty-state {
    text-align: center;
    padding: 2rem;
    color: var(--text-muted);
  }

  .status-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .status-card {
    display: flex;
    align-items: flex-start;
    gap: 1rem;
    padding: 1rem;
    border-radius: var(--radius);
    background: var(--bg-primary);
    transition: background 0.2s;
  }

  .status-card.alt-bg {
    background: var(--bg-hover);
  }

  .status-card:hover {
    background: var(--bg-hover);
  }

  .status-card-icon {
    font-size: 1.25rem;
    line-height: 1;
    flex-shrink: 0;
    margin-top: 0.125rem;
  }

  .status-card-content {
    flex: 1;
    min-width: 0;
  }

  .status-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.25rem;
  }

  .status-context {
    font-size: 0.9375rem;
    color: var(--text-primary);
  }

  .status-date {
    font-size: 0.8125rem;
    color: var(--text-muted);
    white-space: nowrap;
  }

  .status-description {
    font-size: 0.875rem;
    color: var(--text-secondary);
    margin: 0 0 0.5rem 0;
  }

  .status-details-link {
    display: inline-block;
    font-size: 0.875rem;
    color: var(--accent);
    text-decoration: none;
    transition: opacity 0.2s;
  }

  .status-details-link:hover {
    opacity: 0.8;
    text-decoration: underline;
  }

  /* Responsive */
  @media (max-width: 900px) {
    .commit-header {
      flex-direction: column;
      gap: 0.5rem;
    }

    .commit-title {
      font-size: 1.25rem;
    }

    .status-card-header {
      flex-direction: column;
      align-items: flex-start;
      gap: 0.25rem;
    }
  }
  /* ── GPG Signature Badge ── */
  .gpg-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 10px;
    border-radius: 12px;
    font-size: 11px;
    font-weight: 600;
    cursor: default;
    white-space: nowrap;
  }
  .gpg-badge.verified {
    background: rgba(63, 185, 80, 0.12);
    color: var(--green, #3fb950);
    border: 1px solid rgba(63, 185, 80, 0.25);
  }
  .gpg-badge.unverified {
    background: rgba(248, 81, 73, 0.1);
    color: var(--red, #f85149);
    border: 1px solid rgba(248, 81, 73, 0.2);
  }
  /* Neutral on purpose: an unchecked signature is not a warning. */
  .gpg-badge.unchecked {
    background: var(--bg-tertiary);
    color: var(--text-secondary, var(--text-muted));
    border: 1px solid var(--border-light);
  }
  .gpg-badge.no-sig {
    background: var(--bg-tertiary);
    color: var(--text-muted);
    border: 1px solid var(--border-light);
  }
  .gpg-icon { font-size: 12px; }
  .gpg-signer { font-weight: 400; opacity: 0.85; }
</style>
