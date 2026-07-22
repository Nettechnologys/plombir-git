<script lang="ts">
  import { page } from '$app/stores';
  import {
    branchProtections,
    type BranchProtectionPayload,
    type BranchProtectionRule
  } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let rules = $state<BranchProtectionRule[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let busyId = $state<number | null>(null);
  let error = $state('');
  let success = $state('');
  let editingId = $state<number | null>(null);
  let form = $state({
    branch_name: 'main',
    require_pr: true,
    require_status_check: false,
    required_status_checks: '',
    require_approval: true,
    required_approvals: 1,
    allow_force_push: false,
    require_signed_commits: false,
    allowed_push_user_ids: ''
  });

  $effect(() => {
    loadRules();
  });

  function parseStringList(value: string): string[] | undefined {
    const items = value
      .split(',')
      .map((item) => item.trim())
      .filter(Boolean);
    return items.length > 0 ? items : undefined;
  }

  function parseNumberList(value: string): number[] | undefined {
    const ids = value
      .split(',')
      .map((item) => Number(item.trim()))
      .filter((item) => Number.isInteger(item) && item > 0);
    return ids.length > 0 ? ids : undefined;
  }

  function parseJsonArray(value: string | null): string {
    if (!value) return '';
    try {
      const parsed = JSON.parse(value);
      return Array.isArray(parsed) ? parsed.join(', ') : '';
    } catch {
      return '';
    }
  }

  function parseJsonNumberArray(value: string | null): string {
    if (!value) return '';
    try {
      const parsed = JSON.parse(value);
      return Array.isArray(parsed) ? parsed.join(', ') : '';
    } catch {
      return '';
    }
  }

  function payload(includeBranch: boolean): BranchProtectionPayload {
    return {
      ...(includeBranch ? { branch_name: form.branch_name.trim() } : {}),
      require_pr: form.require_pr,
      require_status_check: form.require_status_check,
      required_status_checks: parseStringList(form.required_status_checks),
      require_approval: form.require_approval,
      required_approvals: form.require_approval ? Number(form.required_approvals || 1) : undefined,
      allow_force_push: form.allow_force_push,
      require_signed_commits: form.require_signed_commits,
      allowed_push_user_ids: parseNumberList(form.allowed_push_user_ids)
    };
  }

  function resetForm() {
    editingId = null;
    form = {
      branch_name: 'main',
      require_pr: true,
      require_status_check: false,
      required_status_checks: '',
      require_approval: true,
      required_approvals: 1,
      allow_force_push: false,
      require_signed_commits: false,
      allowed_push_user_ids: ''
    };
  }

  async function loadRules() {
    try {
      loading = true;
      error = '';
      rules = await branchProtections.list(owner, repo);
    } catch (err: any) {
      error = err.message || t('settings.branch_protection.load_failed');
    } finally {
      loading = false;
    }
  }

  function editRule(rule: BranchProtectionRule) {
    editingId = rule.id;
    form = {
      branch_name: rule.branch_name,
      require_pr: rule.require_pr,
      require_status_check: rule.require_status_check,
      required_status_checks: parseJsonArray(rule.required_status_checks),
      require_approval: rule.require_approval,
      required_approvals: rule.required_approvals || 1,
      allow_force_push: rule.allow_force_push,
      require_signed_commits: rule.require_signed_commits,
      allowed_push_user_ids: parseJsonNumberArray(rule.allowed_push_user_ids)
    };
  }

  async function saveRule(event: SubmitEvent) {
    event.preventDefault();

    if (!form.branch_name.trim()) {
      error = t('settings.branch_protection.branch_required');
      return;
    }

    try {
      saving = true;
      error = '';
      success = '';
      if (editingId) {
        await branchProtections.update(owner, repo, editingId, payload(false));
        success = t('settings.branch_protection.updated');
      } else {
        await branchProtections.create(owner, repo, payload(true) as BranchProtectionPayload & { branch_name: string });
        success = t('settings.branch_protection.created');
      }
      resetForm();
      await loadRules();
    } catch (err: any) {
      error = err.message || t('settings.branch_protection.save_failed');
    } finally {
      saving = false;
    }
  }

  async function deleteRule(rule: BranchProtectionRule) {
    if (!confirm(t('settings.branch_protection.delete_confirm', { branch: rule.branch_name }))) return;

    try {
      busyId = rule.id;
      error = '';
      success = '';
      await branchProtections.remove(owner, repo, rule.id);
      success = t('settings.branch_protection.deleted');
      if (editingId === rule.id) resetForm();
      await loadRules();
    } catch (err: any) {
      error = err.message || t('settings.branch_protection.delete_failed');
    } finally {
      busyId = null;
    }
  }
</script>

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
        <label for="required-checks">{t('settings.branch_protection.required_checks')}</label>
        <input id="required-checks" bind:value={form.required_status_checks} disabled={saving || !form.require_status_check} placeholder="test, lint" />
      </div>

      <div class="form-group">
        <label for="allowed-pushers">{t('settings.branch_protection.allowed_pushers')}</label>
        <input id="allowed-pushers" bind:value={form.allowed_push_user_ids} disabled={saving} placeholder="42, 108" />
      </div>

      <div class="form-actions">
        {#if editingId}
          <button class="btn btn-outline" type="button" onclick={resetForm} disabled={saving}>
            {t('common.cancel')}
          </button>
        {/if}
        <button class="btn btn-primary" type="submit" disabled={saving}>
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
                <td>{new Date(rule.updated_at).toLocaleDateString()}</td>
                <td class="actions">
                  <button class="btn btn-outline" onclick={() => editRule(rule)} disabled={busyId === rule.id}>
                    {t('common.edit')}
                  </button>
                  <button class="btn btn-danger" onclick={() => deleteRule(rule)} disabled={busyId === rule.id}>
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

  label {
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-primary);
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
