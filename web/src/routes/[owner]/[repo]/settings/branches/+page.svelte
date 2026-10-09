<script lang="ts">
  import { page } from '$app/stores';
  import {
    allowedUserLabel,
    branchProtections,
    buildBranchProtectionPayload,
    parseStoredStringList,
    type BranchProtectionPayload,
    type BranchProtectionRule
  } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let rules = $state<BranchProtectionRule[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let busyRows = $state<Set<string>>(new Set());
  let error = $state('');
  let success = $state('');
  let editingId = $state<number | null>(null);
  let requiredStatusChecksUnavailable = $state(false);
  let form = $state({
    branch_name: 'main',
    require_pr: true,
    require_status_check: false,
    required_status_checks: [] as string[],
    require_approval: true,
    required_approvals: 1,
    allow_force_push: false,
    require_signed_commits: false,
    allowed_push_users: ''
  });
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    rules = [];
    loading = true;
    saving = false;
    busyRows = new Set();
    error = '';
    success = '';
    resetForm();
    void loadRules(expectedOwner, expectedRepo);
  });

  function rowKey(id: number | null, branchName = form.branch_name.trim()): string {
    return id === null ? `new:${branchName}` : `id:${id}`;
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy(key: string): boolean {
    return busyRows.has(key);
  }

  function claimMutation(key: string): boolean {
    if (isBusy(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }

  function releaseMutation(key: string): void {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  function payload(includeBranch: boolean): BranchProtectionPayload {
    return buildBranchProtectionPayload(form, includeBranch);
  }

  function resetForm() {
    editingId = null;
    requiredStatusChecksUnavailable = false;
    form = {
      branch_name: 'main',
      require_pr: true,
      require_status_check: false,
      required_status_checks: [],
      require_approval: true,
      required_approvals: 1,
      allow_force_push: false,
      require_signed_commits: false,
      allowed_push_users: ''
    };
  }

  function replaceUnreadableStatusChecks() {
    requiredStatusChecksUnavailable = false;
    form.required_status_checks = [];
  }

  function addRequiredStatusCheck() {
    form.required_status_checks = [...form.required_status_checks, ''];
  }

  function removeRequiredStatusCheck(index: number) {
    form.required_status_checks = form.required_status_checks.filter((_, itemIndex) => itemIndex !== index);
  }

  async function loadRules(expectedOwner: string, expectedRepo: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      error = '';
      const next = await branchProtections.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) {
        rules = next;
        error = '';
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.branch_protection.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, owner, repo)) loading = false;
    }
  }

  function editRule(rule: BranchProtectionRule) {
    const statusChecks = parseStoredStringList(rule.required_status_checks);
    editingId = rule.id;
    requiredStatusChecksUnavailable = statusChecks.kind === 'unavailable';
    form = {
      branch_name: rule.branch_name,
      require_pr: rule.require_pr,
      require_status_check: rule.require_status_check,
      required_status_checks: statusChecks.kind === 'parsed' ? statusChecks.value : [],
      require_approval: rule.require_approval,
      required_approvals: rule.required_approvals || 1,
      allow_force_push: rule.allow_force_push,
      require_signed_commits: rule.require_signed_commits,
      allowed_push_users: (rule.allowed_push_users ?? []).map(allowedUserLabel).join(', ')
    };
  }

  async function saveRule(event: SubmitEvent) {
    event.preventDefault();

    if (requiredStatusChecksUnavailable) {
      error = t('settings.branch_protection.required_checks_unreadable_save_blocked');
      return;
    }

    if (!form.branch_name.trim()) {
      error = t('settings.branch_protection.branch_required');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedEditingId = editingId;
    const key = rowKey(expectedEditingId);
    const rulePayload = payload(expectedEditingId === null);
    if (!claimMutation(key)) return;

    try {
      saving = true;
      error = '';
      success = '';
      let successMessage: string;
      if (expectedEditingId !== null) {
        await branchProtections.update(expectedOwner, expectedRepo, expectedEditingId, rulePayload);
        successMessage = t('settings.branch_protection.updated');
      } else {
        await branchProtections.create(
          expectedOwner,
          expectedRepo,
          rulePayload as BranchProtectionPayload & { branch_name: string },
        );
        successMessage = t('settings.branch_protection.created');
      }
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = successMessage;
      resetForm();
      await loadRules(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.branch_protection.save_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        saving = false;
        releaseMutation(key);
      }
    }
  }

  async function deleteRule(rule: BranchProtectionRule) {
    if (!(await confirmer.ask({
      title: t('settings.branch_protection.delete_confirm_title'),
      message: t('settings.branch_protection.delete_confirm', { branch: rule.branch_name }),
      confirmLabel: t('common.delete'),
    }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(rule.id);
    if (!claimMutation(key)) return;

    try {
      error = '';
      success = '';
      await branchProtections.remove(expectedOwner, expectedRepo, rule.id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = t('settings.branch_protection.deleted');
      if (editingId === rule.id) resetForm();
      await loadRules(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.branch_protection.delete_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>

<svelte:head>
  <title>{t('settings.branch_protection.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="branch-protection-page">
  <div class="page-header">
    <div>
      <h1>{t('settings.branch_protection.title')}</h1>
      <p>{t('settings.branch_protection.desc')}</p>
    </div>
  </div>

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  <section class="section">
    <h2>{editingId ? t('settings.branch_protection.edit_title') : t('settings.branch_protection.create_title')}</h2>
    <form class="rule-form" onsubmit={saveRule}>
      <div class="form-grid">
        <div class="form-group">
          <label for="protected-branch">{t('settings.branch_protection.branch')}</label>
          <input id="protected-branch" bind:value={form.branch_name} disabled={saving || Boolean(editingId)} placeholder="main" />
        </div>

        <div class="form-group">
          <label for="required-approvals">{t('settings.branch_protection.required_approvals')}</label>
          <input id="required-approvals" type="number" min="1" bind:value={form.required_approvals} disabled={saving || !form.require_approval} />
        </div>

        <label class="check-row">
          <input type="checkbox" bind:checked={form.require_pr} disabled={saving} />
          <span>{t('settings.branch_protection.require_pr')}</span>
        </label>

        <label class="check-row">
          <input type="checkbox" bind:checked={form.require_approval} disabled={saving} />
          <span>{t('settings.branch_protection.require_approval')}</span>
        </label>

        <label class="check-row">
          <input type="checkbox" bind:checked={form.require_status_check} disabled={saving} />
          <span>{t('settings.branch_protection.require_status_check')}</span>
        </label>

        <label class="check-row">
          <input type="checkbox" bind:checked={form.allow_force_push} disabled={saving} />
          <span>{t('settings.branch_protection.allow_force_push')}</span>
        </label>

        <label class="check-row">
          <input type="checkbox" bind:checked={form.require_signed_commits} disabled={saving} />
          <span>{t('settings.branch_protection.require_signed_commits', 'Require cryptographically signed commits')}</span>
        </label>
      </div>

      <div class="form-group">
        {#if requiredStatusChecksUnavailable}
          <span class="field-label">{t('settings.branch_protection.required_checks')}</span>
          <div class="stored-value-error" role="alert">
            <span>{t('settings.branch_protection.required_checks_unreadable')}</span>
            <button
              class="btn btn-outline replace-unreadable-checks"
              type="button"
              onclick={replaceUnreadableStatusChecks}
              disabled={saving}
            >
              {t('settings.branch_protection.replace_required_checks')}
            </button>
          </div>
        {:else}
          <span id="required-checks-label" class="field-label">{t('settings.branch_protection.required_checks')}</span>
          <span class="field-hint">{t('settings.branch_protection.required_checks_hint')}</span>
          <div class="status-check-editor" aria-labelledby="required-checks-label">
            {#if form.required_status_checks.length === 0}
              <p class="status-checks-empty">{t('settings.branch_protection.required_checks_empty')}</p>
            {/if}
            {#each form.required_status_checks as _, index}
              <div class="status-check-row">
                <label for={`required-check-${index}`}>
                  {t('settings.branch_protection.required_check_name', { number: index + 1 })}
                </label>
                <div class="status-check-control">
                  <input
                    id={`required-check-${index}`}
                    bind:value={form.required_status_checks[index]}
                    disabled={saving || !form.require_status_check}
                    placeholder="test [os=linux, version=stable]"
                  />
                  <button
                    class="btn btn-outline remove-required-check"
                    type="button"
                    onclick={() => removeRequiredStatusCheck(index)}
                    disabled={saving || !form.require_status_check}
                    aria-label={t('settings.branch_protection.remove_required_check', { number: index + 1 })}
                  >
                    {t('common.delete')}
                  </button>
                </div>
              </div>
            {/each}
            <button
              class="btn btn-outline add-required-check"
              type="button"
              onclick={addRequiredStatusCheck}
              disabled={saving || !form.require_status_check}
            >
              {t('settings.branch_protection.add_required_check')}
            </button>
          </div>
        {/if}
      </div>

      <div class="form-group">
        <label for="allowed-pushers">{t('settings.branch_protection.allowed_pushers')}</label>
        <input id="allowed-pushers" bind:value={form.allowed_push_users} disabled={saving} placeholder="alice, bob" />
      </div>

      <div class="form-actions">
        {#if editingId}
          <button class="btn btn-outline" type="button" onclick={resetForm} disabled={saving || isBusy(rowKey(editingId))}>
            {t('common.cancel')}
          </button>
        {/if}
        <button class="btn btn-primary" type="submit" disabled={saving || requiredStatusChecksUnavailable || isBusy(rowKey(editingId))} aria-busy={saving}>
          {saving ? t('common.loading') : t('common.save')}
        </button>
      </div>
    </form>
  </section>

  <section class="section">
    <h2>{t('settings.branch_protection.current')}</h2>

    {#if loading}
      <div class="loading">{t('common.loading')}</div>
    {:else if rules.length === 0}
      <div class="empty-state">{t('settings.branch_protection.empty')}</div>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{t('settings.branch_protection.branch')}</th>
              <th>{t('settings.branch_protection.rules')}</th>
              <th>{t('settings.branch_protection.updated_at')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each rules as rule (rule.id)}
              <tr>
                <td><code>{rule.branch_name}</code></td>
                <td>
                  <div class="rule-list">
                    {#if rule.require_pr}<span>{t('settings.branch_protection.require_pr')}</span>{/if}
                    {#if rule.require_approval}<span>{t('settings.branch_protection.approvals_count', { count: rule.required_approvals || 1 })}</span>{/if}
                    {#if rule.require_status_check}<span>{t('settings.branch_protection.status_checks_enabled')}</span>{/if}
                    {#if rule.allow_force_push}<span>{t('settings.branch_protection.force_push_allowed')}</span>{/if}
                    {#if rule.require_signed_commits}<span>{t('settings.branch_protection.signed_commits_required', 'Signed commits required')}</span>{/if}
                  </div>
                </td>
                <td>{formatDate(rule.updated_at)}</td>
                <td class="actions">
                  <button class="btn btn-outline" onclick={() => editRule(rule)} disabled={isBusy(rowKey(rule.id))}>
                    {t('common.edit')}
                  </button>
                  <button class="btn btn-danger" onclick={() => deleteRule(rule)} disabled={isBusy(rowKey(rule.id))} aria-busy={isBusy(rowKey(rule.id))}>
                    {t('common.delete')}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<ConfirmModal {confirmer} />

<style>
  .branch-protection-page {
    max-width: 960px;
  }

  .page-header {
    margin-bottom: 2rem;
  }

  h1 {
    font-size: 1.75rem;
    margin: 0 0 0.5rem;
    color: var(--text-primary);
  }

  h2 {
    font-size: 1.1rem;
    margin: 0 0 1rem;
    color: var(--text-primary);
  }

  p {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }

  .section {
    margin-bottom: 2.5rem;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
  }

  .rule-form {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .form-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 1rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  label,
  .field-label {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .field-hint,
  .status-checks-empty {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .status-check-editor {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .status-check-row {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .status-check-control {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .status-check-control input {
    flex: 1 1 auto;
    min-width: 0;
  }

  .status-check-editor > .btn {
    align-self: flex-start;
  }

  input {
    padding: 0.65rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 0.95rem;
  }

  .check-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-height: 42px;
  }

  .check-row input {
    width: 16px;
    height: 16px;
  }

  .form-actions,
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.75rem;
  }

  .success-box,
  .error-box,
  .empty-state,
  .loading {
    padding: 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .success-box {
    background: var(--success-bg, #dcfce7);
    color: var(--success-text, #166534);
  }

  .error-box {
    background: var(--danger-bg, #fee2e2);
    color: var(--danger-text, #991b1b);
  }

  .stored-value-error {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.75rem;
    border: 1px solid var(--danger, #dc2626);
    border-radius: 6px;
    background: var(--danger-bg, #fee2e2);
    color: var(--danger-text, #991b1b);
  }

  .stored-value-error .btn {
    flex: 0 0 auto;
  }

  .empty-state,
  .loading {
    background: var(--bg-secondary);
    color: var(--text-secondary);
    text-align: center;
  }

  .table-wrap {
    overflow-x: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    padding: 0.8rem;
    border-bottom: 1px solid var(--border);
    text-align: left;
    vertical-align: top;
  }

  th {
    background: var(--bg-secondary);
    font-size: 0.8rem;
    color: var(--text-secondary);
    text-transform: uppercase;
  }

  tr:last-child td {
    border-bottom: 0;
  }

  code {
    font-family: var(--font-mono, monospace);
    font-size: 0.9rem;
  }

  .rule-list {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .rule-list span {
    padding: 0.2rem 0.5rem;
    border-radius: 999px;
    background: var(--bg-secondary);
    color: var(--text-secondary);
    font-size: 0.8rem;
  }

  .btn {
    padding: 0.55rem 0.9rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    cursor: pointer;
  }

  .btn-primary {
    background: var(--accent);
    border-color: var(--accent);
    color: white;
  }

  .btn-outline {
    background: transparent;
    color: var(--text-primary);
  }

  .btn-danger {
    background: var(--danger, #dc2626);
    border-color: var(--danger, #dc2626);
    color: white;
  }

  @media (max-width: 720px) {
    .form-grid {
      grid-template-columns: 1fr;
    }

    .actions {
      flex-direction: column;
    }
  }
</style>
