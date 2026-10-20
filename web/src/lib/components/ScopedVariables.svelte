<script lang="ts">
  import { ciVariables, type CiVariable, type CiVariableScope } from '$lib/api/client.svelte';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

  let { scope }: { scope: CiVariableScope } = $props();

  let items = $state<CiVariable[]>([]);
  let name = $state('');
  let value = $state('');
  let error = $state('');
  let saving = $state(false);
  let busyRows = $state<Set<string>>(new Set());
  let generation = 0;
  // Only the newest list request may fill the list: an initial load that
  // answers after a save's reload would otherwise put the old rows back.
  let listRequest = 0;

  const scopeKey = (target: CiVariableScope) => (target.kind === 'repo' ? `repo:${target.owner}/${target.repo}` : `org:${target.name}`);

  $effect(() => {
    const expected = scope;
    const current = ++generation;
    items = [];
    error = '';
    saving = false;
    busyRows = new Set();
    void load(expected, current);
  });

  function isCurrent(expected: CiVariableScope, current: number) {
    return generation === current && scopeKey(scope) === scopeKey(expected);
  }

  async function load(expected: CiVariableScope, current: number) {
    const ticket = ++listRequest;
    try {
      const next = await ciVariables.list(expected);
      if (ticket === listRequest && isCurrent(expected, current)) { items = next; error = ''; }
    } catch (e: any) {
      if (ticket === listRequest && isCurrent(expected, current)) error = e.message;
    }
  }

  function claim(key: string) {
    if (busyRows.has(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }
  function release(key: string) {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const expected = scope;
    const current = generation;
    const variableName = name.trim().toUpperCase();
    if (!claim(variableName)) return;
    try {
      saving = true;
      error = '';
      await ciVariables.put(expected, variableName, value);
      if (!isCurrent(expected, current)) return;
      name = '';
      value = '';
      await load(expected, current);
    } catch (e: any) {
      if (isCurrent(expected, current)) error = e.message;
    } finally {
      if (isCurrent(expected, current)) {
        saving = false;
        release(variableName);
      }
    }
  }

  async function remove(item: CiVariable) {
    if (!(await confirmer.ask({
      title: t('settings.ci_variables.delete_confirm_title'),
      message: t('settings.ci_variables.delete_confirm', { name: item.name }),
      confirmLabel: t('common.delete'),
    }))) return;
    const expected = scope;
    const current = generation;
    if (!claim(item.name)) return;
    try {
      await ciVariables.delete(expected, item.name);
      if (isCurrent(expected, current)) await load(expected, current);
    } catch (e: any) {
      if (isCurrent(expected, current)) error = e.message;
    } finally {
      if (isCurrent(expected, current)) release(item.name);
    }
  }
</script>

<div class="settings-page">
  <header>
    <h1>{t('settings.ci_variables.title')}</h1>
    <p>{scope.kind === 'org' ? t('settings.ci_variables.desc_org') : t('settings.ci_variables.desc')}</p>
  </header>
  {#if error}<div class="message" role="alert">{error}</div>{/if}
  <section>
    <h2>{t('settings.ci_variables.add_title')}</h2>
    <form onsubmit={save}>
      <label for="variable-name">{t('settings.ci_variables.name')}</label>
      <input id="variable-name" bind:value={name} pattern="[A-Za-z_][A-Za-z0-9_]*" maxlength="100" placeholder="DEPLOY_REGION" disabled={saving} required />
      <label for="variable-value">{t('settings.ci_variables.value')}</label>
      <textarea id="variable-value" bind:value={value} maxlength="49152" rows="3" disabled={saving} required></textarea>
      <button class="btn btn-primary" disabled={saving} aria-busy={saving}>{t('settings.ci_variables.save')}</button>
    </form>
  </section>
  <section>
    <h2>{t('settings.ci_variables.list_title')}</h2>
    {#if items.length === 0}
      <p>{t('settings.ci_variables.empty')}</p>
    {:else}
      <div class="list">
        {#each items as item (item.name)}
          <article>
            <div>
              <strong>{item.name}</strong>
              <code>{item.value}</code>
              <small>{t('common.updated', { date: formatDateTime(item.updated_at) })}</small>
            </div>
            <button class="btn btn-danger" disabled={busyRows.has(item.name)} aria-busy={busyRows.has(item.name)} onclick={() => remove(item)}>{t('common.delete')}</button>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<ConfirmModal {confirmer} />

<style>
  .settings-page { max-width: 880px; }
  header, section { margin-bottom: 28px; }
  header p, small { color: var(--text-secondary); }
  form { display: grid; gap: 9px; }
  input, textarea { padding: 8px 10px; font: inherit; }
  .message { color: var(--red); padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); }
  .list { display: grid; gap: 10px; }
  article { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 14px; border: 1px solid var(--border); border-radius: var(--radius); }
  article > div { min-width: 0; }
  code { display: block; margin-top: 5px; white-space: pre-wrap; overflow-wrap: anywhere; }
  small { display: block; margin-top: 5px; }
</style>
