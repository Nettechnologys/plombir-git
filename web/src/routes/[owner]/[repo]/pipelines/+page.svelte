<script lang="ts">
  import { page } from '$app/stores';
  import { onDestroy } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import PipelineBadge from '$lib/components/PipelineBadge.svelte';
  import { artifacts, connectJobLogWebSocket, pipelines, repos } from '$lib/api/client.svelte';
  import type { CiArtifact } from '$lib/api/artifacts';
  import type { WorkflowDispatchInput } from '$lib/api/pipelines';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let pipelineList = $state<any[]>([]);
  let selectedPipeline = $state<any>(null);
  let loading = $state(true);
  let error = $state('');
  let selectedJob = $state<any>(null);
  let showLogPanel = $state(false);
  let logContent = $state('');
  let logStreamStatus = $state<'idle' | 'connected' | 'closed' | 'error'>('idle');
  let logStreamError = $state('');
  let logContentEl = $state<HTMLPreElement | null>(null);
  let logSocket: WebSocket | null = null;
  let approvedJobs = $state<number[]>([]);
  let artifactList = $state<CiArtifact[]>([]);
  let artifactsLoading = $state(false);
  let artifactsError = $state('');
  let downloadingArtifactId = $state<number | null>(null);
  // Guards against a slow request for a pipeline the user has already left:
  // without it the previous pipeline's artifacts land under the new one.
  let artifactRequest = 0;
  let deletingArtifactId = $state<number | null>(null);
  let triggerBranches = $state<Array<{ name: string; is_default: boolean }>>([]);
  let triggerRef = $state('');
  let triggerSchemaRef = $state('');
  let triggerInputDefinitions = $state<WorkflowDispatchInput[]>([]);
  let triggerInputs = $state<Record<string, string>>({});
  let triggerSchemaLoading = $state(false);
  let triggerSchemaRequest = 0;
  let triggering = $state(false);

  // Auto-refresh for running pipelines
  let refreshInterval: ReturnType<typeof setInterval> | null = null;

  function normalizePipelineDetail(detail: any) {
    if (!detail?.pipeline) return detail;
    return {
      ...detail.pipeline,
      ref: detail.pipeline.ref_name,
      stages: (detail.stages || []).map((entry: any) => ({ ...entry.stage, jobs: entry.jobs || [] })),
    };
  }

  $effect(() => {
    triggerRef = '';
    loadPipelines();
    loadTriggerRefs();
    return () => { if (refreshInterval) clearInterval(refreshInterval); };
  });

  $effect(() => {
    // Start auto-refresh when a pipeline is running
    if (selectedPipeline?.status === 'running' || selectedPipeline?.status === 'pending') {
      if (!refreshInterval) {
        refreshInterval = setInterval(() => {
          if (selectedPipeline) {
            const refreshingId = selectedPipeline.id;
            pipelines.get(owner, repo, refreshingId).then(p => {
              selectedPipeline = normalizePipelineDetail(p);
            });
            // A job publishes its artifact when it succeeds, so the list grows
            // while the pipeline is still running — polling only the pipeline
            // would leave the section empty until the user clicked away and back.
            loadArtifacts(refreshingId);
          }
        }, 5000);
      }
    } else {
      if (refreshInterval) { clearInterval(refreshInterval); refreshInterval = null; }
    }
  });

  async function loadPipelines() {
    try {
      loading = true;
      const pipeResult = await pipelines.list(owner, repo);
      pipelineList = pipeResult.data;
      if (pipelineList.length > 0 && !selectedPipeline) {
        selectedPipeline = normalizePipelineDetail(await pipelines.get(owner, repo, pipelineList[0].id));
        await loadArtifacts(pipelineList[0].id);
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadTriggerRefs() {
    try {
      const branches = await repos.branches(owner, repo);
      triggerBranches = branches;
      triggerRef = branches.find((branch) => branch.is_default)?.name ?? branches[0]?.name ?? '';
      await loadDispatchSchema(triggerRef);
    } catch (e: any) {
      error = e.message;
    }
  }

  function initialDispatchValue(input: WorkflowDispatchInput): string {
    if (input.default !== null) return input.default;
    if (input.type === 'boolean') return 'false';
    if (input.type === 'number') return '0';
    if (input.type === 'choice') return input.options[0] ?? '';
    return '';
  }

  async function loadDispatchSchema(ref: string) {
    const requestedRef = ref.trim();
    const requestId = ++triggerSchemaRequest;
    triggerSchemaRef = '';
    triggerInputDefinitions = [];
    triggerInputs = {};
    if (!requestedRef) {
      triggerSchemaLoading = false;
      return;
    }

    triggerSchemaLoading = true;
    error = '';
    try {
      const schema = await pipelines.workflowDispatchSchema(owner, repo, requestedRef);
      if (requestId !== triggerSchemaRequest) return;
      const definitions = schema.inputs;
      triggerInputDefinitions = definitions;
      triggerInputs = Object.fromEntries(
        definitions.map((input) => [input.name, initialDispatchValue(input)]),
      );
      triggerSchemaRef = requestedRef;
    } catch (e: any) {
      if (requestId === triggerSchemaRequest) error = e.message;
    } finally {
      if (requestId === triggerSchemaRequest) triggerSchemaLoading = false;
    }
  }

  function updateTriggerRef(value: string) {
    triggerRef = value;
    if (triggerSchemaRef !== value.trim()) {
      triggerSchemaRequest += 1;
      triggerSchemaRef = '';
      triggerInputDefinitions = [];
      triggerInputs = {};
      triggerSchemaLoading = false;
    }
  }

  function setTriggerInput(name: string, value: string) {
    triggerInputs = { ...triggerInputs, [name]: value };
  }

  async function loadArtifacts(pipelineId: number) {
    const requestId = ++artifactRequest;
    artifactsLoading = true;
    try {
      const list = await artifacts.list(owner, repo, pipelineId);
      if (requestId !== artifactRequest) return;
      artifactList = list;
      artifactsError = '';
    } catch (e: any) {
      if (requestId !== artifactRequest) return;
      // An empty list is a normal state — no job declared `artifacts:` — so it
      // must not be reported as a failure. A failure, on the other hand, must
      // not look like an empty list: the retention page would then be offering
      // to expire files the user was told do not exist.
      artifactList = [];
      artifactsError = e.message;
    } finally {
      if (requestId === artifactRequest) artifactsLoading = false;
    }
  }

  async function downloadArtifact(artifact: CiArtifact) {
    downloadingArtifactId = artifact.id;
    try {
      await artifacts.download(artifact.id, artifact.name);
      artifactsError = '';
    } catch (e: any) {
      artifactsError = t('pipeline.artifact_download_failed', { name: artifact.name, reason: e.message });
    } finally {
      downloadingArtifactId = null;
    }
  }

  async function deleteArtifact(artifact: CiArtifact) {
    // Irreversible and it takes the bytes with it, so the confirmation names
    // the artifact rather than asking a generic "are you sure".
    if (!confirm(t('pipeline.artifact_delete_confirm', { name: artifact.name }))) return;
    deletingArtifactId = artifact.id;
    try {
      await artifacts.remove(artifact.id);
      artifactList = artifactList.filter((entry) => entry.id !== artifact.id);
      artifactsError = '';
    } catch (e: any) {
      artifactsError = t('pipeline.artifact_delete_failed', { name: artifact.name, reason: e.message });
    } finally {
      deletingArtifactId = null;
    }
  }

  function artifactSize(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes < 0) return '—';
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
  }

  async function selectPipeline(id: number) {
    disconnectJobLogSocket();
    selectedJob = null;
    showLogPanel = false;
    try {
      selectedPipeline = normalizePipelineDetail(await pipelines.get(owner, repo, id));
      await loadArtifacts(id);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleTrigger(event?: SubmitEvent) {
    event?.preventDefault();
    const requestedRef = triggerRef.trim();
    if (
      triggering ||
      triggerSchemaLoading ||
      !requestedRef ||
      triggerSchemaRef !== requestedRef
    ) return;

    triggering = true;
    error = '';
    try {
      const created = await pipelines.trigger(owner, repo, requestedRef, triggerInputs);
      await loadPipelines();
      await selectPipeline(created.id);
    } catch (e: any) {
      error = e.message;
    } finally {
      triggering = false;
    }
  }

  async function handleRetry(id: number) {
    try {
      await pipelines.retry(owner, repo, id);
      await loadPipelines();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleCancel(id: number) {
    try {
      await pipelines.cancel(owner, repo, id);
      selectedPipeline.status = 'canceled';
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handlePlay(jobId: number) {
    if (!selectedPipeline) return;
    try {
      await pipelines.play(owner, repo, selectedPipeline.id, jobId);
      selectedPipeline = normalizePipelineDetail(await pipelines.get(owner, repo, selectedPipeline.id));
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleApprove(jobId: number) {
    if (!selectedPipeline) return;
    try {
      const result = await pipelines.approve(owner, repo, selectedPipeline.id, jobId);
      if (!result.released && !approvedJobs.includes(jobId)) approvedJobs = [...approvedJobs, jobId];
      selectedPipeline = normalizePipelineDetail(await pipelines.get(owner, repo, selectedPipeline.id));
    } catch (e: any) {
      error = e.message;
    }
  }

  async function viewJobLog(jobId: number) {
    if (!selectedPipeline) return;
    try {
      disconnectJobLogSocket();
      const job = await pipelines.job(owner, repo, selectedPipeline.id, jobId);
      selectedJob = job;
      logContent = job.log || '';
      showLogPanel = true;
      startJobLogStream(jobId);
    } catch (e: any) {
      disconnectJobLogSocket();
      logContent = 'Failed to load log: ' + e.message;
      logStreamStatus = 'error';
      showLogPanel = true;
    }
  }

  function closeLog() {
    disconnectJobLogSocket();
    showLogPanel = false;
    selectedJob = null;
  }

  function startJobLogStream(jobId: number) {
    logStreamStatus = 'idle';
    logStreamError = '';
    logSocket = connectJobLogWebSocket(
      jobId,
      (chunk) => appendLogChunk(jobId, chunk),
      (status) => {
        if (selectedJob?.id !== jobId) return;
        logStreamStatus = status;
      },
      () => {
        if (selectedJob?.id !== jobId) return;
        logStreamStatus = 'error';
        logStreamError = 'Live log connection failed';
      },
    );
  }

  function appendLogChunk(jobId: number, chunk: string) {
    if (!chunk || selectedJob?.id !== jobId) return;
    logContent += chunk;
    requestAnimationFrame(() => {
      if (logContentEl) {
        logContentEl.scrollTop = logContentEl.scrollHeight;
      }
    });
  }

  function disconnectJobLogSocket() {
    if (logSocket) {
      logSocket.close();
      logSocket = null;
    }
    logStreamStatus = 'idle';
    logStreamError = '';
  }

  function duration(start: string, end?: string) {
    if (!start) return '-';
    const s = new Date(start).getTime();
    const e = end ? new Date(end).getTime() : Date.now();
    const sec = Math.floor((e - s) / 1000);
    if (sec < 60) return sec + 's';
    if (sec < 3600) return Math.floor(sec / 60) + 'm ' + (sec % 60) + 's';
    return Math.floor(sec / 3600) + 'h ' + Math.floor((sec % 3600) / 60) + 'm';
  }

  function statusIcon(status: string): string {
    switch (status) {
      case 'success': return '✓';
      case 'failed': case 'error': return '✗';
      case 'running': return '⟳';
      case 'manual': return '▶';
      case 'waiting_approval': return '⏳';
      case 'canceled': return '−';
      case 'skipped': return '○';
      default: return '●';
    }
  }

  function statusColor(status: string): string {
    switch (status) {
      case 'success': return 'var(--green)';
      case 'failed': case 'error': return 'var(--red)';
      case 'running': return 'var(--accent)';
      case 'canceled': return 'var(--text-muted)';
      case 'skipped': return 'var(--text-muted)';
      default: return 'var(--yellow)';
    }
  }

  function isRunning(s: string) { return s === 'running' || s === 'pending'; }

  function openJobByKey(e: KeyboardEvent, jobId: number) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      viewJobLog(jobId);
    }
  }

  onDestroy(() => {
    if (refreshInterval) clearInterval(refreshInterval);
    disconnectJobLogSocket();
  });

  function selectPipelineByKey(e: KeyboardEvent, id: number) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      selectPipeline(id);
    }
  }

  function closeLogByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      closeLog();
    }
  }
</script>

<svelte:head>
  <title>CI/CD · {owner}/{repo} · ForgeKeep</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="pipelines" />

  <form class="pipeline-trigger" onsubmit={handleTrigger}>
    <div class="trigger-field">
      <label for="pipeline-trigger-ref">{t('pipeline.run_ref')}</label>
      <input
        id="pipeline-trigger-ref"
        list="pipeline-trigger-refs"
        value={triggerRef}
        oninput={(event) => updateTriggerRef(event.currentTarget.value)}
        onchange={() => loadDispatchSchema(triggerRef)}
        placeholder={t('pipeline.run_ref_placeholder')}
        autocomplete="off"
        disabled={triggering}
        required
      />
      <datalist id="pipeline-trigger-refs">
        {#each triggerBranches as branch}
          <option value={branch.name}></option>
        {/each}
      </datalist>
    </div>
    {#if triggerSchemaLoading}
      <span class="trigger-schema-loading">{t('common.loading')}</span>
    {:else}
      {#each triggerInputDefinitions as input (input.name)}
        <div class="trigger-field">
          <label for={`pipeline-trigger-input-${input.name}`}>
            {input.name}{input.required ? ' *' : ''}
          </label>
          {#if input.type === 'boolean'}
            <input
              id={`pipeline-trigger-input-${input.name}`}
              class="trigger-checkbox"
              type="checkbox"
              checked={triggerInputs[input.name] === 'true'}
              onchange={(event) => setTriggerInput(input.name, event.currentTarget.checked ? 'true' : 'false')}
              disabled={triggering}
            />
          {:else if input.type === 'choice'}
            <select
              id={`pipeline-trigger-input-${input.name}`}
              value={triggerInputs[input.name] ?? ''}
              onchange={(event) => setTriggerInput(input.name, event.currentTarget.value)}
              disabled={triggering}
              required={input.required}
            >
              {#each input.options as option}
                <option value={option}>{option}</option>
              {/each}
            </select>
          {:else}
            <input
              id={`pipeline-trigger-input-${input.name}`}
              type={input.type === 'number' ? 'number' : 'text'}
              step={input.type === 'number' ? 'any' : undefined}
              value={triggerInputs[input.name] ?? ''}
              oninput={(event) => setTriggerInput(input.name, event.currentTarget.value)}
              disabled={triggering}
              required={input.required}
            />
          {/if}
          {#if input.description}
            <small>{input.description}</small>
          {/if}
        </div>
      {/each}
    {/if}
    <button
      type="submit"
      class="btn-primary"
      disabled={
        triggering ||
        triggerSchemaLoading ||
        !triggerRef.trim() ||
        triggerSchemaRef !== triggerRef.trim()
      }
      aria-busy={triggering || triggerSchemaLoading}
    >
      {triggering ? t('pipeline.starting') : t('pipeline.run_pipeline')}
    </button>
  </form>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if pipelineList.length === 0}
    <div class="empty">
      <p>{t('pipeline.no_pipelines')}</p>
      <p class="text-secondary">{t('pipeline.hint', { file: t('pipeline.file') })}</p>
    </div>
  {:else}
    <div class="pipeline-layout">
      <!-- Pipeline list -->
      <div class="pipeline-list">
        <h3>{t('repo.tabs.pipelines')}</h3>
        <div class="list-scroll">
          {#each pipelineList as p}
            <div
              class="pipeline-item"
              class:active={selectedPipeline?.id === p.id}
              onclick={() => selectPipeline(p.id)}
              onkeydown={(e) => selectPipelineByKey(e, p.id)}
              role="button"
              tabindex="0"
            >
              <PipelineBadge status={p.status} />
              <div class="pipeline-info">
                <div class="pipeline-msg truncate">{p.commit_message?.split('\n')[0] || '#' + p.id}</div>
                <div class="pipeline-meta">
                  <span class="mono">{p.commit_sha?.slice(0, 7)}</span>
                  <span>{duration(p.started_at, p.finished_at)}</span>
                </div>
              </div>
            </div>
          {/each}
        </div>
      </div>

      <!-- Pipeline detail -->
      <div class="pipeline-detail">
        {#if selectedPipeline}
          <div class="detail-header">
            <h2>{t('pipeline.detail_title', { id: String(selectedPipeline.id) })}</h2>
            <PipelineBadge status={selectedPipeline.status} />
            <div class="detail-actions">
              {#if selectedPipeline.status === 'failed' || selectedPipeline.status === 'failure' || selectedPipeline.status === 'error'}
                <button class="btn-outline" onclick={() => handleRetry(selectedPipeline.id)}>{t('pipeline.retry')}</button>
              {/if}
              {#if selectedPipeline.status === 'running' || selectedPipeline.status === 'pending' || selectedPipeline.status === 'manual' || selectedPipeline.status === 'waiting_approval'}
                <button class="btn-outline btn-danger" onclick={() => handleCancel(selectedPipeline.id)}>{t('pipeline.cancel')}</button>
              {/if}
            </div>
          </div>

          <div class="detail-info">
            <div><span class="text-secondary">{t('pipeline.commit')}:</span> <code>{selectedPipeline.commit_sha?.slice(0, 7)}</code></div>
            <div><span class="text-secondary">{t('pipeline.branch')}:</span> {selectedPipeline.ref}</div>
            <div><span class="text-secondary">{t('pipeline.duration')}:</span> {duration(selectedPipeline.started_at, selectedPipeline.finished_at)}</div>
          </div>

          <!-- Pipeline Flow Visualization -->
          {#if selectedPipeline.stages?.length > 0}
            <div class="pipeline-flow">
              {#each selectedPipeline.stages as stage, si}
                <div class="flow-stage">
                  <!-- Stage header -->
                  <div class="stage-label">
                    <span class="stage-dot" style="background:{statusColor(stage.status)}"></span>
                    <span class="stage-name">{stage.name}</span>
                    <span class="stage-dur">{duration(stage.started_at, stage.finished_at)}</span>
                  </div>

                  <!-- Connector arrow between stages -->
                  {#if si < selectedPipeline.stages.length - 1}
                    <div class="stage-connector">
                      <svg width="16" height="24" viewBox="0 0 16 24">
                        <line x1="8" y1="0" x2="8" y2="20" stroke="var(--border)" stroke-width="2"/>
                        <polyline points="2,16 8,22 14,16" fill="none" stroke="var(--border)" stroke-width="2"/>
                      </svg>
                    </div>
                  {/if}

                  <!-- Jobs in this stage -->
                  <div class="jobs-flow">
                    {#each stage.jobs as job}
                      <div class="job-card" class:running={job.status === 'running'} class:failed={job.status === 'failed'} onclick={() => viewJobLog(job.id)} onkeydown={(e) => openJobByKey(e, job.id)} role="button" tabindex="0">
                        <div class="job-status-icon" style="color:{statusColor(job.status)}">
                          {#if job.status === 'running'}
                            <span class="spin">{statusIcon(job.status)}</span>
                          {:else}
                            {statusIcon(job.status)}
                          {/if}
                        </div>
                        <div class="job-body">
                          <span class="job-name">{job.name}</span>
                          {#if job.environment_name}<span class="environment-name">🚀 {job.environment_name}</span>{/if}
                          {#if job.if_condition}
                            <span
                              class="job-condition"
                              class:condition-skipped={job.status === 'skipped'}
                              title={`${job.status === 'skipped' ? t('pipeline.condition_skipped') : t('pipeline.condition')}: ${job.if_condition}`}
                            >if</span>
                          {/if}
                          <span class="job-dur">{duration(job.started_at, job.finished_at)}</span>
                        </div>
                        {#if job.exit_code !== null}
                          <span class="exit-code">{job.exit_code}</span>
                        {/if}
                        {#if job.status === 'manual'}
                          <button class="play-job" onclick={(event) => { event.stopPropagation(); handlePlay(job.id); }}>{t('pipeline.play_manual')}</button>
                        {/if}
                        {#if job.status === 'waiting_approval'}
                          <button class="play-job" disabled={approvedJobs.includes(job.id)} onclick={(event) => { event.stopPropagation(); handleApprove(job.id); }}>{approvedJobs.includes(job.id) ? t('pipeline.approval_recorded') : t('pipeline.approve_environment')}</button>
                        {/if}
                      </div>
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          {:else}
            <p class="text-secondary">{t('pipeline.select_detail')}</p>
          {/if}

          <!-- Artifacts published by this pipeline's jobs -->
          <section class="artifacts">
            <h3>{t('pipeline.artifacts')}</h3>
            {#if artifactsError}
              <p class="artifacts-error">{t('pipeline.artifacts_load_failed', { reason: artifactsError })}</p>
            {/if}
            {#if artifactsLoading && artifactList.length === 0}
              <p class="text-secondary">{t('common.loading')}</p>
            {:else if artifactList.length === 0}
              <p class="text-secondary">
                {t('pipeline.artifacts_empty')}
                {t('pipeline.artifacts_hint', { file: t('pipeline.artifacts_field') })}
              </p>
            {:else}
              <ul class="artifact-list">
                {#each artifactList as artifact (artifact.id)}
                  <li class="artifact-row">
                    <span class="artifact-name">{artifact.name}</span>
                    <span class="artifact-size">{artifactSize(artifact.size)}</span>
                    <span class="artifact-created">{formatDate(artifact.created_at)}</span>
                    <span class="artifact-expiry">
                      {t('pipeline.artifact_expires')}:
                      {artifact.expires_at ? formatDate(artifact.expires_at) : t('pipeline.artifact_expires_never')}
                    </span>
                    <button
                      class="btn-outline artifact-download"
                      disabled={downloadingArtifactId === artifact.id}
                      onclick={() => downloadArtifact(artifact)}
                    >{downloadingArtifactId === artifact.id ? t('pipeline.artifact_downloading') : t('pipeline.artifact_download')}</button>
                    <button
                      class="btn-outline artifact-delete"
                      disabled={deletingArtifactId === artifact.id}
                      onclick={() => deleteArtifact(artifact)}
                    >{deletingArtifactId === artifact.id ? t('pipeline.artifact_deleting') : t('pipeline.artifact_delete')}</button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>
        {:else}
          <p class="text-secondary">{t('pipeline.select_detail')}</p>
        {/if}
      </div>
    </div>
  {/if}
</div>

<!-- Log Viewer Modal -->
{#if showLogPanel}
  <div class="log-overlay-wrap">
    <button
      type="button"
      class="log-overlay"
      onclick={closeLog}
      aria-label={t('common.cancel')}
    ></button>
    <div
      class="log-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="pipeline-log-title"
      tabindex="-1"
      onkeydown={closeLogByKey}
    >
      <div class="log-header">
        <div>
          <strong id="pipeline-log-title">{selectedJob?.name || 'Job Log'}</strong>
          {#if selectedJob}
            <PipelineBadge status={selectedJob.status} />
          {/if}
          {#if logStreamStatus === 'connected'}
            <span class="log-live connected">Live</span>
          {:else if logStreamStatus === 'closed'}
            <span class="log-live">Closed</span>
          {:else if logStreamStatus === 'error'}
            <span class="log-live error">{logStreamError || 'Offline'}</span>
          {/if}
        </div>
        <button class="btn-close" onclick={closeLog}>✕</button>
      </div>
      {#if selectedJob?.if_condition}
        <div class="log-condition">
          <span>{selectedJob.status === 'skipped' ? t('pipeline.condition_skipped') : t('pipeline.condition')}</span>
          <code>{selectedJob.if_condition}</code>
        </div>
      {/if}
      <pre class="log-content" bind:this={logContentEl}><code>{logContent || '(no log output)'}</code></pre>
    </div>
  </div>
{/if}

<style>
.empty { text-align: center; padding: 48px; color: var(--text-secondary); }

  .pipeline-trigger {
    display: flex;
    align-items: flex-end;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
    margin: 16px 0 24px;
  }
  .trigger-field { display: flex; flex-direction: column; gap: 4px; }
  .pipeline-trigger label { font-size: 13px; color: var(--text-secondary); }
  .pipeline-trigger input,
  .pipeline-trigger select {
    min-width: 240px;
    padding: 7px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
    color: var(--text-primary);
    font-family: var(--font-mono);
  }
  .pipeline-trigger .trigger-checkbox {
    min-width: 0;
    width: 18px;
    height: 18px;
    margin: 7px 0;
    padding: 0;
  }
  .trigger-field small {
    max-width: 240px;
    color: var(--text-muted);
    font-size: 11px;
  }
  .trigger-schema-loading { align-self: center; color: var(--text-muted); font-size: 13px; }
  .pipeline-trigger button { padding: 7px 14px; }
  .pipeline-trigger button:disabled { opacity: 0.6; cursor: default; }

  @media (max-width: 640px) {
    .pipeline-trigger { align-items: stretch; flex-direction: column; }
    .trigger-field,
    .pipeline-trigger input,
    .pipeline-trigger select { min-width: 0; width: 100%; }
    .pipeline-trigger .trigger-checkbox { width: 18px; }
  }

  .pipeline-layout {
    display: grid;
    grid-template-columns: 320px 1fr;
    gap: 24px;
  }
  @media (max-width: 900px) { .pipeline-layout { grid-template-columns: 1fr; } }

  .pipeline-list {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .list-scroll { overflow-y: auto; max-height: 70vh; }

  h3 { padding: 12px 16px; border-bottom: 1px solid var(--border); font-size: 14px; margin: 0; }

  .pipeline-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border-light);
    cursor: pointer;
  }
  .pipeline-item:last-child { border-bottom: none; }
  .pipeline-item:hover { background: var(--bg-hover); }
  .pipeline-item.active { background: var(--bg-tertiary); border-left: 3px solid var(--accent); }

  .pipeline-info { flex: 1; min-width: 0; }
  .pipeline-msg { font-size: 13px; font-weight: 500; }
  .pipeline-meta { font-size: 11px; color: var(--text-muted); margin-top: 2px; display: flex; gap: 8px; }

  .pipeline-detail {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 24px;
  }

  .detail-header { display: flex; align-items: center; gap: 12px; margin-bottom: 16px; }
  h2 { font-size: 20px; margin: 0; }
  .detail-actions { margin-left: auto; display: flex; gap: 8px; }

  .btn-outline {
    padding: 4px 12px; background: none; border: 1px solid var(--border);
    border-radius: var(--radius); color: var(--text-primary); font-size: 12px; cursor: pointer;
  }
  .btn-outline:hover { background: var(--bg-hover); }
  .btn-danger { border-color: var(--red-dim); color: var(--red); }

  .detail-info {
    display: flex; gap: 24px; font-size: 13px; margin-bottom: 20px;
    padding: 12px 16px; background: var(--bg-primary); border-radius: var(--radius);
    flex-wrap: wrap;
  }
  .detail-info code { font-size: 12px; background: var(--bg-tertiary); padding: 1px 6px; border-radius: 3px; }

  /* ── Visual Pipeline Flow ── */
  .pipeline-flow {
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 8px 0;
  }

  .flow-stage {
    position: relative;
  }

  .stage-label {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
    background: var(--bg-tertiary);
    border-radius: 6px 6px 0 0;
    border: 1px solid var(--border);
    border-bottom: none;
  }
  .stage-dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
  .stage-name { flex: 1; text-transform: uppercase; letter-spacing: 0.5px; font-size: 11px; }
  .stage-dur { font-size: 11px; color: var(--text-muted); font-family: var(--font-mono); }

  .stage-connector {
    display: flex;
    justify-content: center;
    padding: 2px 0;
    height: 28px;
  }

  .jobs-flow {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 0 0 6px 6px;
    background: var(--bg-primary);
  }

  .job-card {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-radius: 4px;
    border: none;
    background: transparent;
    cursor: pointer;
    text-align: left;
    width: 100%;
    font-size: 13px;
    transition: background 0.15s;
  }
  .job-card:hover { background: var(--bg-hover); }
  .job-card.running { background: rgba(88, 166, 255, 0.08); }
  .job-card.failed { background: rgba(248, 81, 73, 0.06); }

  .job-status-icon { width: 20px; text-align: center; font-size: 14px; flex-shrink: 0; }

  .job-body { flex: 1; display: flex; align-items: center; gap: 8px; min-width: 0; }
  .job-name { flex: 1; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .environment-name { color: var(--text-secondary); font-size: 11px; white-space: nowrap; }
  .job-condition {
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-secondary);
    cursor: help;
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 16px;
    padding: 0 5px;
  }
  .job-condition.condition-skipped { border-color: var(--text-muted); color: var(--text-muted); }
  .job-dur { font-size: 11px; color: var(--text-muted); font-family: var(--font-mono); white-space: nowrap; }

  .exit-code {
    font-size: 11px;
    font-family: var(--font-mono);
    padding: 1px 6px;
    border-radius: 3px;
    background: var(--bg-tertiary);
    color: var(--text-muted);
  }
  .play-job {
    padding: 3px 9px; border: 1px solid var(--accent); border-radius: 4px;
    color: var(--accent); background: transparent; cursor: pointer; font-size: 11px;
  }
  .play-job:hover { background: rgba(88, 166, 255, 0.1); }
  .play-job:disabled { opacity: .6; cursor: default; }

  .spin { display: inline-block; animation: spin 1.5s linear infinite; }
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* ── Log Viewer Modal ── */
  .log-overlay-wrap {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 40px;
  }

  .log-overlay {
    position: absolute;
    inset: 0;
    z-index: 1;
    background: rgba(0,0,0,0.5);
    border: none;
    margin: 0;
    padding: 0;
    cursor: default;
  }
  .log-modal {
    position: relative;
    z-index: 2;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    width: 100%;
    max-width: 800px;
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0,0,0,0.2);
  }
  .log-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    font-size: 14px;
  }
  .log-header > div { display: flex; align-items: center; gap: 8px; }
  .log-condition {
    align-items: baseline;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    color: var(--text-secondary);
    display: flex;
    font-size: 12px;
    gap: 10px;
    padding: 8px 16px;
  }
  .log-condition code { color: var(--text-primary); overflow-wrap: anywhere; }
  .log-live {
    font-size: 11px;
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 2px 8px;
  }
  .log-live.connected { color: var(--green); border-color: color-mix(in srgb, var(--green) 45%, var(--border)); }
  .log-live.error { color: var(--red); border-color: color-mix(in srgb, var(--red) 45%, var(--border)); }
  .btn-close {
    background: none; border: none;
    font-size: 18px; cursor: pointer; color: var(--text-muted);
    padding: 4px 8px; border-radius: 4px;
  }
  .btn-close:hover { background: var(--bg-hover); color: var(--text-primary); }

  .log-content {
    overflow: auto;
    padding: 16px;
    margin: 0;
    font-size: 12px;
    line-height: 1.5;
    background: #1a1a2e;
    color: #e0e0e0;
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 60vh;
  }

  /* ── Artifacts ─────────────────────────────────────────────── */
  .artifacts { margin-top: 20px; }
  .artifacts h3 { margin: 0 0 8px; font-size: 14px; }
  .artifacts-error { color: var(--red); margin: 0 0 8px; font-size: 13px; }
  .artifact-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .artifact-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    font-size: 13px;
  }
  .artifact-name { font-weight: 600; word-break: break-all; }
  .artifact-size, .artifact-created, .artifact-expiry { color: var(--text-muted); }
  .artifact-download { margin-left: auto; }
  .artifact-delete { color: var(--red); border-color: var(--red-dim); }
</style>
