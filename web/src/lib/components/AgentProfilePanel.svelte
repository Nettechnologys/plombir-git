<script lang="ts">
  // A bot's agent profile (ADR-0001): what the bot runs when a person assigns
  // it an issue, with which model key, where, and under which limits. The key
  // is write-only — the server answers `has_model_key` and nothing more — so an
  // empty key field means "keep the stored one", never "erase it".
  import {
    ApiError,
    bots,
    instance,
    splitList,
    type AgentPreset,
    type AgentProfile,
  } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  let { username }: { username: string } = $props();

  const t = createT();
  const confirmer = createConfirmer();
  const requests = new LatestRequestFence<string>();
  const presets: AgentPreset[] = ['claude_code', 'codex', 'custom'];
  // Spelled out rather than built from the value, so each label is a key the
  // catalog checks can see.
  const presetLabel = (preset: AgentPreset) =>
    preset === 'claude_code'
      ? t('agents.profile.preset_claude_code')
      : preset === 'codex'
        ? t('agents.profile.preset_codex')
        : t('agents.profile.preset_custom');

  let loading = $state(true);
  let saving = $state(false);
  let deleting = $state(false);
  let error = $state('');
  let success = $state('');
  let profile = $state<AgentProfile | null>(null);
  let agentsEnabled = $state<boolean | null>(null);

  let enabled = $state(false);
  let preset = $state<AgentPreset>('claude_code');
  let image = $state('');
  let command = $state('');
  let runnerTags = $state('');
  let modelKey = $state('');
  let clearModelKey = $state(false);
  let timeoutMinutes = $state(30);
  let maxRunsPerDay = $state(20);
  let maxIterations = $state(5);
  let repositories = $state('');

  function fill(next: AgentProfile | null) {
    profile = next;
    enabled = next?.enabled ?? false;
    preset = next?.preset ?? 'claude_code';
    image = next?.image ?? '';
    command = next?.command ?? '';
    runnerTags = (next?.runner_tags ?? []).join(', ');
    modelKey = '';
    clearModelKey = false;
    timeoutMinutes = Math.round((next?.timeout_secs ?? 1800) / 60);
    maxRunsPerDay = next?.max_runs_per_day ?? 20;
    maxIterations = next?.max_iterations ?? 5;
    repositories = (next?.repositories ?? []).join('\n');
    if (next) agentsEnabled = next.agents_enabled;
  }

  $effect(() => {
    load(username);
  });

  async function load(bot: string) {
    const claim = requests.begin(bot);
    loading = true;
    error = '';
    success = '';
    try {
      const [answer, info] = await Promise.all([
        bots.getAgentProfile(bot).catch((cause) => {
          if (cause instanceof ApiError && cause.status === 404) return null;
          throw cause;
        }),
        instance.get().catch(() => null),
      ]);
      if (!requests.owns(claim, username)) return;
      if (info) agentsEnabled = info.agents_enabled;
      fill(answer);
    } catch (cause: any) {
      if (requests.owns(claim, username)) {
        error = cause.message || t('agents.profile.load_failed');
      }
    } finally {
      if (requests.owns(claim, username)) loading = false;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    const bot = username;
    const claim = requests.begin(bot);
    saving = true;
    error = '';
    success = '';
    const key = modelKey.trim();
    try {
      const saved = await bots.saveAgentProfile(bot, {
        enabled,
        preset,
        image: image.trim() || undefined,
        command: preset === 'custom' ? command.trim() || undefined : undefined,
        runner_tags: splitList(runnerTags),
        model_key: !clearModelKey && key ? key : undefined,
        clear_model_key: clearModelKey || undefined,
        timeout_secs: Math.round(timeoutMinutes * 60),
        max_runs_per_day: maxRunsPerDay,
        max_iterations: maxIterations,
        repositories: splitList(repositories),
      });
      if (!requests.owns(claim, username)) return;
      fill(saved);
      success = t('agents.profile.saved');
    } catch (cause: any) {
      if (requests.owns(claim, username)) {
        error = cause.message || t('agents.profile.save_failed');
      }
    } finally {
      saving = false;
    }
  }

  async function remove() {
    if (deleting || !profile) return;
    if (!(await confirmer.ask({
      title: t('agents.profile.delete_confirm_title'),
      message: t('agents.profile.delete_confirm', { username }),
      confirmLabel: t('common.delete'),
    }))) return;
    const bot = username;
    const claim = requests.begin(bot);
    deleting = true;
    error = '';
    success = '';
    try {
      await bots.deleteAgentProfile(bot);
      if (!requests.owns(claim, username)) return;
      fill(null);
      success = t('agents.profile.deleted');
    } catch (cause: any) {
      if (requests.owns(claim, username)) {
        error = cause.message || t('agents.profile.delete_failed');
      }
    } finally {
      deleting = false;
    }
  }
</script>

<section class="agent-profile" aria-label={t('agents.profile.title')}>
  <h3>{t('agents.profile.title')}</h3>
  <p class="muted">{t('agents.profile.description')}</p>

  {#if agentsEnabled === false}
    <div class="notice agents-off">{t('agents.profile.instance_off')}</div>
  {/if}
  {#if error}
    <div class="error-box">{error}</div>
  {/if}
  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if loading}
    <p class="muted">{t('common.loading')}</p>
  {:else}
    <form class="profile-form" onsubmit={save}>
      <label class="checkbox wide">
        <input type="checkbox" bind:checked={enabled} disabled={saving} />
        {t('agents.profile.enabled')}
      </label>
      <label>
        {t('agents.profile.preset')}
        <select bind:value={preset} disabled={saving}>
          {#each presets as option (option)}
            <option value={option}>{presetLabel(option)}</option>
          {/each}
        </select>
      </label>
      <label>
        {t('agents.profile.image')}
        <input bind:value={image} placeholder={preset === 'custom' ? 'ghcr.io/acme/agent:1' : t('agents.profile.image_default')} disabled={saving} />
      </label>
      {#if preset === 'custom'}
        <label class="wide">
          {t('agents.profile.command')}
          <textarea bind:value={command} rows="3" disabled={saving}></textarea>
        </label>
      {/if}
      <label class="wide">
        {t('agents.profile.model_key')}
        <input
          type="password"
          autocomplete="off"
          bind:value={modelKey}
          placeholder={profile?.has_model_key ? t('agents.profile.model_key_keep') : t('agents.profile.model_key_none')}
          disabled={saving || clearModelKey}
        />
      </label>
      {#if profile?.has_model_key}
        <label class="checkbox wide">
          <input type="checkbox" bind:checked={clearModelKey} disabled={saving} />
          {t('agents.profile.clear_model_key')}
        </label>
      {/if}
      <label class="wide">
        {t('agents.profile.repositories')}
        <textarea bind:value={repositories} rows="3" placeholder="alice/app" disabled={saving}></textarea>
        <span class="muted small">{t('agents.profile.repositories_hint')}</span>
      </label>
      <label>
        {t('agents.profile.runner_tags')}
        <input bind:value={runnerTags} placeholder={t('agents.profile.runner_tags_placeholder')} disabled={saving} />
      </label>
      <label>
        {t('agents.profile.timeout_minutes')}
        <input type="number" min="1" max="240" bind:value={timeoutMinutes} disabled={saving} />
      </label>
      <label>
        {t('agents.profile.max_runs_per_day')}
        <input type="number" min="1" max="500" bind:value={maxRunsPerDay} disabled={saving} />
      </label>
      <label>
        {t('agents.profile.max_iterations')}
        <input type="number" min="1" max="50" bind:value={maxIterations} disabled={saving} />
      </label>
      <div class="profile-actions wide">
        <button type="submit" class="btn btn-primary save-profile" disabled={saving}>
          {saving ? t('agents.profile.saving') : t('common.save')}
        </button>
        {#if profile}
          <button type="button" class="btn btn-danger delete-profile" disabled={deleting} onclick={remove}>
            {deleting ? t('agents.deleting') : t('agents.profile.delete')}
          </button>
        {/if}
      </div>
    </form>
  {/if}
</section>

<ConfirmModal {confirmer} />

<style>
  .agent-profile {
    border-top: 1px solid var(--border-color);
    margin-top: 16px;
    padding-top: 16px;
  }
  .profile-form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px;
  }
  .profile-form label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .profile-form label.checkbox {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .profile-actions {
    display: flex;
    gap: 8px;
  }
  .notice {
    margin: 8px 0;
    padding: 8px 12px;
    border-radius: 6px;
    border: 1px solid var(--warning);
    background: var(--yellow-dim);
  }
  .small {
    font-size: 12px;
  }
</style>
