<script lang="ts">
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { boards } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let boardList = $state<any[]>([]);
  let activeBoard = $state<any>(null);
  let columns = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');

  // Create board form
  let showCreate = $state(false);
  let newBoardName = $state('');
  let newBoardDesc = $state('');

  // Column form
  let showAddCol = $state(false);
  let newColName = $state('');

  // Card form
  let showAddCard = $state<number | null>(null);
  let newCardTitle = $state('');

  onMount(() => loadBoards());

  function normalizeColumns(board: any) {
    return (board?.columns || []).map((entry: any) => {
      if (entry.column) {
        return { ...entry.column, cards: entry.cards || [] };
      }
      return { ...entry, cards: entry.cards || [] };
    });
  }

  async function loadBoards() {
    try {
      loading = true;
      boardList = await boards.list(owner, repo);
      if (boardList.length > 0) {
        await selectBoard(boardList[0]);
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function selectBoard(board: any) {
    const b = await boards.get(owner, repo, board.id);
    activeBoard = b.board || b;
    columns = normalizeColumns(b);
  }

  async function createBoard() {
    if (!newBoardName.trim()) return;
    error = '';
    try {
      const b = await boards.create(owner, repo, {
        name: newBoardName.trim(),
        description: newBoardDesc.trim() || undefined,
      });
      boardList = [b, ...boardList];
      showCreate = false;
      newBoardName = '';
      newBoardDesc = '';
      await selectBoard(b);
    } catch (e: any) {
      error = e.message || 'Failed to create board';
      console.error('createBoard error:', e);
    }
  }

  async function deleteBoard(id: number) {
    if (!confirm(t('board.confirmDelete'))) return;
    try {
      await boards.delete(owner, repo, id);
      boardList = boardList.filter(b => b.id !== id);
      activeBoard = null;
      columns = [];
      if (boardList.length > 0) await selectBoard(boardList[0]);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function addColumn() {
    if (!newColName.trim() || !activeBoard) return;
    try {
      const col = await boards.createColumn(owner, repo, activeBoard.id, {
        name: newColName.trim(),
      });
      columns = [...columns, col];
      showAddCol = false;
      newColName = '';
    } catch (e: any) {
      error = e.message;
    }
  }

  async function deleteColumn(colId: number) {
    if (!activeBoard) return;
    try {
      await boards.deleteColumn(owner, repo, activeBoard.id, colId);
      columns = columns.filter(c => c.id !== colId);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function addCard(colId: number) {
    if (!newCardTitle.trim() || !activeBoard) return;
    try {
      const card = await boards.createCard(owner, repo, activeBoard.id, colId, {
        note: newCardTitle.trim(),
      });
      const col = columns.find(c => c.id === colId);
      if (col) {
        col.cards = col.cards || [];
        col.cards = [...col.cards, card];
        columns = [...columns];
      }
      showAddCard = null;
      newCardTitle = '';
    } catch (e: any) {
      error = e.message;
    }
  }

  async function deleteCard(cardId: number, colId: number) {
    if (!activeBoard) return;
    try {
      await boards.deleteCard(owner, repo, activeBoard.id, cardId);
      const col = columns.find(c => c.id === colId);
      if (col) {
        col.cards = (col.cards || []).filter((c: any) => c.id !== cardId);
        columns = [...columns];
      }
    } catch (e: any) {
      error = e.message;
    }
  }

  async function moveCard(cardId: number, fromColId: number, toColId: number) {
    if (!activeBoard || fromColId === toColId) return;
    const targetCol = columns.find(c => c.id === toColId);
    const position = targetCol ? (targetCol.cards || []).length : 0;
    try {
      await boards.moveCard(owner, repo, activeBoard.id, cardId, {
        column_id: toColId,
        position,
      });
      await refreshBoard();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function refreshBoard() {
    if (!activeBoard) return;
    try {
      const b = await boards.get(owner, repo, activeBoard.id);
      activeBoard = b.board || b;
      columns = normalizeColumns(b);
    } catch (e: any) {
      error = e.message;
    }
  }

  function closeCreateModal() {
    showCreate = false;
  }

  function selectBoardByKey(e: KeyboardEvent, board: any) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      selectBoard(board);
    }
  }
</script>

<svelte:head>
  <title>Board · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="board" />

  <div class="page-header">
    <h1>{t('board.title')}</h1>
    <button class="btn btn-primary" onclick={() => (showCreate = true)}>
      + {t('board.createBoard')}
    </button>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if showCreate}
    <div class="modal-overlay-wrap">
      <button
        class="modal-overlay"
        type="button"
        aria-label={t('common.close')}
        onclick={closeCreateModal}
      ></button>
      <div class="modal">
        <h3>{t('board.createBoard')}</h3>
        <input class="input" type="text" bind:value={newBoardName} placeholder={t('board.namePlaceholder')} />
        <input class="input" type="text" bind:value={newBoardDesc} placeholder={t('board.descPlaceholder')} />
        <div class="modal-actions">
          <button class="btn" onclick={closeCreateModal}>{t('common.cancel')}</button>
          <button class="btn btn-primary" onclick={createBoard}>{t('common.create')}</button>
        </div>
      </div>
    </div>
  {/if}

  {#if loading}
    <p class="loading-text">{t('common.loading')}...</p>
  {:else if boardList.length === 0}
    <p class="empty-text">{t('board.noBoards')}</p>
  {:else}
    <div class="board-layout">
      <!-- Board selector tabs -->
      <div class="board-tabs">
        {#each boardList as b}
          <div
            class="tab"
            class:active={activeBoard?.id === b.id}
            onclick={() => selectBoard(b)}
            onkeydown={(e) => selectBoardByKey(e, b)}
            role="button"
            tabindex="0"
          >
            {b.name}
            <button
              type="button"
              class="close"
              onclick={(e) => {
                e.stopPropagation();
                deleteBoard(b.id);
              }}
              aria-label={`${t('board.deleteBoard')} ${b.name}`}
            >
              &times;
            </button>
          </div>
        {/each}
      </div>

      {#if activeBoard}
        <div class="board-header">
          <h2>{activeBoard.name}</h2>
          <button class="btn btn-sm" onclick={() => (showAddCol = true)}>
            + {t('board.addColumn')}
          </button>
        </div>

        {#if showAddCol}
          <div class="inline-form">
            <input class="input" type="text" bind:value={newColName} placeholder={t('board.colNamePlaceholder')} />
            <button class="btn btn-primary btn-sm" onclick={addColumn}>{t('common.add')}</button>
            <button class="btn btn-sm" onclick={() => (showAddCol = false)}>{t('common.cancel')}</button>
          </div>
        {/if}

        <div class="kanban-board">
          {#each columns as col (col.id)}
            <div class="kanban-column">
              <div class="col-header">
                <strong>{col.name}</strong>
                <span class="card-count">{(col.cards || []).length}</span>
                <button class="btn-icon" onclick={() => deleteColumn(col.id)} title={t('common.delete')}>&times;</button>
              </div>

              <div class="col-body">
                {#each (col.cards || []) as card (card.id)}
                  <div class="card">
                    <div class="card-header">
                      <span>{card.note || card.issue?.title || `#${card.issue_id}`}</span>
                      <button class="btn-icon btn-icon-sm" onclick={() => deleteCard(card.id, col.id)}>&times;</button>
                    </div>
                    {#if card.issue}
                      <a class="card-link" href={`/${owner}/${repo}/issues/${card.issue.number}`}>
                        #{card.issue.number} {card.issue.title}
                      </a>
                    {/if}
                    <!-- Move dropdown -->
                    <select
                      class="card-move"
                      value={col.id}
                      onchange={(e) => moveCard(card.id, col.id, parseInt((e.target as HTMLSelectElement).value))}
                    >
                      <option value="" disabled>{t('board.moveTo')}</option>
                      {#each columns.filter((c: any) => c.id !== col.id) as targetCol}
                        <option value={targetCol.id}>{targetCol.name}</option>
                      {/each}
                    </select>
                  </div>
                {/each}

                {#if showAddCard === col.id}
                  <div class="inline-form">
                    <input class="input input-sm" type="text" bind:value={newCardTitle} placeholder={t('board.cardTitlePlaceholder')} />
                    <button class="btn btn-primary btn-sm" onclick={() => addCard(col.id)}>{t('common.add')}</button>
                    <button class="btn btn-sm" onclick={() => { showAddCard = null; newCardTitle = ''; }}>{t('common.cancel')}</button>
                  </div>
                {:else}
                  <button class="btn btn-ghost btn-sm add-card-btn" onclick={() => (showAddCard = col.id)}>
                    + {t('board.addCard')}
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .page-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 20px; }
  h1 { font-size: 24px; font-weight: 600; margin: 0; }
  h2 { font-size: 16px; margin: 0; }
  h3 { font-size: 15px; margin: 0 0 12px; }
.loading-text, .empty-text { color: var(--text-secondary, #666); text-align:center; padding:48px; }

  /* Board tabs */
  .board-tabs { display: flex; gap: 4px; margin-bottom: 16px; flex-wrap: wrap; }
  .tab { padding: 6px 12px; border:1px solid var(--border-color, #d1d5db); border-radius:6px; background:var(--bg-primary, #fff); cursor:pointer; font-size:13px; display:flex; align-items:center; gap:6px; color:var(--text-primary, #333); }
  .tab.active { background: var(--accent, #2563eb); color:#fff; border-color:var(--accent, #2563eb); }
  .tab .close { font-size:14px; opacity:0.6; border:none; background:none; color: inherit; line-height:1; padding:0; cursor:pointer; }
  .tab .close:hover { opacity:1; }

  .board-header { display:flex; align-items:center; justify-content:space-between; margin-bottom:12px; }
  .inline-form { display:flex; gap:8px; align-items:center; margin-bottom:12px; }

  /* Kanban */
  .kanban-board { display:flex; gap:12px; overflow-x:auto; padding-bottom:12px; min-height:200px; }
  .kanban-column { background:var(--bg-secondary, #f9fafb); border:1px solid var(--border-color, #e5e7eb); border-radius:8px; min-width:260px; max-width:320px; display:flex; flex-direction:column; }
  .col-header { padding:10px 12px; border-bottom:1px solid var(--border-color, #e5e7eb); display:flex; align-items:center; gap:8px; font-size:13px; }
  .card-count { background:var(--bg-tertiary, #e5e7eb); border-radius:10px; padding:1px 8px; font-size:11px; color:var(--text-secondary, #666); margin-left:auto; }
  .col-body { padding:8px; flex:1; display:flex; flex-direction:column; gap:6px; }

  .card { background:var(--bg-primary, #fff); border:1px solid var(--border-color, #e5e7eb); border-radius:6px; padding:8px 10px; font-size:13px; }
  .card-header { display:flex; justify-content:space-between; align-items:flex-start; gap:4px; }
  .card-link { display:block; font-size:12px; color:var(--link-color, #2563eb); text-decoration:none; margin-top:4px; }
  .card-link:hover { text-decoration:underline; }
  .card-move { margin-top:6px; width:100%; font-size:11px; padding:2px 4px; border:1px solid var(--border-color, #d1d5db); border-radius:4px; }

  .add-card-btn { width:100%; text-align:left; color:var(--text-secondary, #666); font-size:12px; }
  .add-card-btn:hover { background:var(--bg-tertiary, #e5e7eb); }

  /* Modal */
  .modal-overlay-wrap {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.3);
    border: none;
    padding: 0;
    margin: 0;
    z-index: 99;
    cursor: default;
  }
  .modal {
    position: relative;
    z-index: 101;
    background:var(--bg-primary, #fff);
    padding:20px;
    border-radius:12px;
    min-width:300px;
    max-width:400px;
    box-shadow:0 4px 24px rgba(0,0,0,0.15);
  }
  .modal-actions { display:flex; gap:8px; margin-top:12px; justify-content:flex-end; }

  /* Shared */
  .btn { padding:6px 14px; border:1px solid var(--border-color, #d1d5db); border-radius:6px; background:var(--bg-primary, #fff); cursor:pointer; font-size:13px; color:var(--text-primary, #333); }
  .btn:hover { background:var(--bg-secondary, #f3f4f6); }
  .btn-primary { background:var(--accent, #2563eb); color:#fff; border-color:var(--accent, #2563eb); }
  .btn-primary:hover { opacity:0.9; }
  .btn-sm { padding:4px 10px; font-size:12px; }
  .btn-ghost { background:transparent; border:none; }
  .btn-icon { background:none; border:none; cursor:pointer; font-size:16px; color:var(--text-secondary, #666); padding:0 4px; line-height:1; }
  .btn-icon:hover { color:#dc2626; }
  .btn-icon-sm { font-size:14px; }
  .input { padding:6px 10px; border:1px solid var(--border-color, #d1d5db); border-radius:6px; font-size:13px; width:100%; box-sizing:border-box; margin-bottom:8px; background:var(--bg-primary, #fff); color:var(--text-primary, #333); }
  .input-sm { margin-bottom:0; width:auto; flex:1; }
</style>
