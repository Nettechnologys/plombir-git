<script lang="ts">
  import { attachments, type Attachment, type AttachmentTarget } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  // Uploading and deleting are `RepoWrite` on the server; a reader sees the
  // files and nothing to do with them (card_270a0a77fd79).
  let { owner, repo, target, targetId, canWrite = false }: {
    owner: string;
    repo: string;
    target: AttachmentTarget;
    targetId: number;
    canWrite?: boolean;
  } = $props();

  const t = createT();
  let items = $state<Attachment[]>([]);
  let uploading = $state(false);
  let deletingId = $state<number | null>(null);
  let error = $state('');

  $effect(() => {
    owner;
    repo;
    target;
    targetId;
    load();
  });

  async function load() {
    try {
      items = await attachments.list(owner, repo, target, targetId);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function upload(event: Event) {
    const field = event.currentTarget as HTMLInputElement;
    const file = field.files?.[0];
    if (!file) return;
    try {
      uploading = true;
      error = '';
      const item = await attachments.upload(owner, repo, target, targetId, file);
      items = [...items, item];
      field.value = '';
    } catch (e: any) {
      error = e.message;
    } finally {
      uploading = false;
    }
  }

  async function remove(id: number) {
    try {
      deletingId = id;
      error = '';
      await attachments.remove(owner, repo, target, targetId, id);
      items = items.filter((item) => item.id !== id);
    } catch (e: any) {
      error = e.message;
    } finally {
      deletingId = null;
    }
  }

  function sizeLabel(size: number): string {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
    return `${(size / 1024 / 1024).toFixed(1)} MiB`;
  }
</script>

{#if canWrite || items.length > 0 || error}
<section class="attachment-panel">
  <div class="attachment-heading">
    <div>
      <h3>{t('attachments.title')}</h3>
      {#if canWrite}<p>{t('attachments.hint')}</p>{/if}
    </div>
    {#if canWrite}
      <label class="upload-button" class:disabled={uploading}>
        {uploading ? t('attachments.uploading') : t('attachments.upload')}
        <input type="file" onchange={upload} disabled={uploading} />
      </label>
    {/if}
  </div>

  {#if error}<div class="error-banner">{error}</div>{/if}
  {#if items.length > 0}
    <ul>
      {#each items as item (item.id)}
        <li>
          <a href={item.browser_download_url} download={item.name} data-sveltekit-reload>{item.name}</a>
          <span>{sizeLabel(item.size)}</span>
          {#if canWrite}
            <button class="btn-secondary" onclick={() => remove(item.id)} disabled={deletingId === item.id}>
              {t('attachments.delete')}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>
{/if}

<style>
  .attachment-panel { border: 1px solid var(--border); border-radius: var(--radius); padding: 16px; margin: 16px 0; }
  .attachment-heading { display: flex; justify-content: space-between; align-items: center; gap: 16px; }
  h3 { margin: 0 0 4px; font-size: 15px; }
  p { margin: 0; color: var(--text-muted); font-size: 12px; }
  .upload-button { display: inline-flex; padding: 6px 12px; border-radius: var(--radius); background: var(--green-dim); color: white; cursor: pointer; font-size: 13px; white-space: nowrap; }
  .upload-button.disabled { opacity: .55; cursor: default; }
  .upload-button input { display: none; }
  ul { list-style: none; margin: 14px 0 0; padding: 0; border-top: 1px solid var(--border); }
  li { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 12px; align-items: center; padding: 10px 0; border-bottom: 1px solid var(--border-light); }
  li:last-child { border-bottom: 0; }
  li a { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  li span { color: var(--text-muted); font-size: 12px; }
  @media (max-width: 560px) {
    .attachment-heading { align-items: flex-start; }
    li { grid-template-columns: minmax(0, 1fr) auto; }
    li span { grid-row: 2; }
  }
</style>
