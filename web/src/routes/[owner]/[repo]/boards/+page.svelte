<script lang="ts">
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import {
    boards,
    buildBoardCardUpdatePayload,
    buildBoardUpdatePayload,
    buildColumnUpdatePayload,
    issues,
    type Board,
    type BoardCard,
    type BoardCardEditFormState,
    type BoardColumn,
    type BoardEditFormState,
    type BoardFullResponse,
    type Issue,
  } from '$lib/api/client.svelte';
  import { publishBoardCardOrder } from '$lib/api/boardOrder';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let boardList = $state<Board[]>([]);
  let activeBoard = $state<Board | null>(null);
  let columns = $state<BoardColumn[]>([]);
  let issueOptions = $state<Issue[]>([]);
  let loading = $state(true);
  let error = $state('');
  let boardMutationBusy = $state(false);
  let boardSelectionBusy = $state(false);
  let selectedBoardId = $state<number | null>(null);
  const boardSelectionRequests = new LatestRepositoryResourceRequestFence<number>();

  // Create board form
  let showCreate = $state(false);
  let newBoardName = $state('');
  let newBoardDesc = $state('');
  let showEditBoard = $state(false);
  let boardForm = $state<BoardEditFormState>({ name: '', description: '' });

  // Column form
  let showAddCol = $state(false);
  let newColName = $state('');
  let editingColumnId = $state<number | null>(null);
  let editColumnName = $state('');

  // Card form
  let showAddCard = $state<number | null>(null);
  let newCardTitle = $state('');
  let editingCard = $state<BoardCard | null>(null);
  let cardForm = $state<BoardCardEditFormState>({ note: '', issueId: '' });

  onMount(() => loadBoards());

  async function runBoardMutation(operation: () => Promise<void>): Promise<boolean> {
    if (boardMutationBusy || boardSelectionBusy) return false;
    boardMutationBusy = true;
    error = '';
    try {
      await operation();
      return true;
    } catch (e: any) {
      error = e.message;
      return false;
    } finally {
      boardMutationBusy = false;
    }
  }

  function normalizeColumns(board: BoardFullResponse): BoardColumn[] {
    return board.columns.map((entry) => ({ ...entry.column, cards: entry.cards || [] }));
  }

  async function loadBoards() {
    try {
      loading = true;
      const [boardsData, issuesData] = await Promise.all([
        boards.list(owner, repo),
        issues.list(owner, repo, undefined, 1, 100),
      ]);
      boardList = boardsData;
      issueOptions = issuesData.data;
      if (boardList.length > 0) {
        await selectBoard(boardList[0]);
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function selectBoard(board: Board) {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const claim = boardSelectionRequests.begin(expectedOwner, expectedRepo, board.id);
    selectedBoardId = board.id;
    boardSelectionBusy = true;
    error = '';
    activeBoard = null;
    columns = [];
    showEditBoard = false;
    editingColumnId = null;
    editingCard = null;
    try {
      const b = await boards.get(expectedOwner, expectedRepo, board.id);
      if (
        selectedBoardId !== board.id ||
        !boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) return;
      if (b.board.id !== board.id) throw new Error('Board response identity mismatch');
      activeBoard = b.board;
      columns = normalizeColumns(b);
    } catch (e: any) {
      if (
        selectedBoardId === board.id &&
        boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) error = e.message;
    } finally {
      if (
        selectedBoardId === board.id &&
        boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) boardSelectionBusy = false;
    }
  }

  async function createBoard() {
    if (!newBoardName.trim()) return;
    await runBoardMutation(async () => {
      const b = await boards.create(owner, repo, {
        name: newBoardName.trim(),
        description: newBoardDesc.trim() || undefined,
      });
      boardList = [b, ...boardList];
      showCreate = false;
      newBoardName = '';
      newBoardDesc = '';
      await selectBoard(b);
    });
  }

  async function deleteBoard(id: number) {
    if (!confirm(t('board.confirmDelete'))) return;
    await runBoardMutation(async () => {
      await boards.delete(owner, repo, id);
      boardList = boardList.filter(b => b.id !== id);
      selectedBoardId = null;
      activeBoard = null;
      columns = [];
      if (boardList.length > 0) await selectBoard(boardList[0]);
    });
  }

  function startEditBoard() {
    if (!activeBoard) return;
    boardForm = {
      name: activeBoard.name,
      description: activeBoard.description || '',
    };
    showEditBoard = true;
  }

  async function saveBoard() {
    if (!activeBoard || !boardForm.name.trim()) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      const updated = await boards.update(
        owner,
        repo,
        boardId,
        buildBoardUpdatePayload(boardForm),
      );
      activeBoard = updated;
      boardList = boardList.map((board) => (board.id === updated.id ? updated : board));
      showEditBoard = false;
    });
  }

  async function addColumn() {
    if (!newColName.trim() || !activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      const col = await boards.createColumn(owner, repo, boardId, {
        name: newColName.trim(),
      });
      columns = [...columns, { ...col, cards: [] }];
      showAddCol = false;
      newColName = '';
    });
  }

  async function deleteColumn(colId: number) {
    if (!activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      await boards.deleteColumn(owner, repo, boardId, colId);
      columns = columns.filter(c => c.id !== colId);
    });
  }

  function startEditColumn(column: BoardColumn) {
    editingColumnId = column.id;
    editColumnName = column.name;
  }

  async function saveColumn(column: BoardColumn) {
    if (!activeBoard || !editColumnName.trim()) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      const updated = await boards.updateColumn(
        owner,
        repo,
        boardId,
        column.id,
        buildColumnUpdatePayload(editColumnName),
      );
      columns = columns.map((entry) =>
        entry.id === updated.id ? { ...entry, ...updated } : entry
      );
      editingColumnId = null;
      editColumnName = '';
    });
  }

  async function addCard(colId: number) {
    if (!newCardTitle.trim() || !activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      const card = await boards.createCard(owner, repo, boardId, colId, {
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
    });
  }

  async function deleteCard(cardId: number, colId: number) {
    if (!activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      await boards.deleteCard(owner, repo, boardId, cardId);
      const col = columns.find(c => c.id === colId);
      if (col) {
        col.cards = (col.cards || []).filter((c: any) => c.id !== cardId);
        columns = [...columns];
      }
    });
  }

  function startEditCard(card: BoardCard) {
    editingCard = card;
    cardForm = {
      note: card.note || '',
      issueId: card.issue_id === null ? '' : String(card.issue_id),
    };
  }

  async function saveCard() {
    if (!activeBoard || !editingCard) return;
    const boardId = activeBoard.id;
    const cardId = editingCard.id;
    await runBoardMutation(async () => {
      await boards.updateCard(
        owner,
        repo,
        boardId,
        cardId,
        buildBoardCardUpdatePayload(cardForm),
      );
      editingCard = null;
      await refreshBoard();
    });
  }

  async function moveCard(cardId: number, fromColId: number, toColId: number) {
    if (!activeBoard || fromColId === toColId) return;
    const boardId = activeBoard.id;
    const targetCol = columns.find(c => c.id === toColId);
    const position = targetCol ? (targetCol.cards || []).length : 0;
    await runBoardMutation(async () => {
      await boards.moveCard(owner, repo, boardId, cardId, {
        column_id: toColId,
        position,
      });
      await refreshBoard();
    });
  }

  async function reorderCard(column: BoardColumn, cardId: number, targetIndex: number) {
    if (!activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async () => {
      await publishBoardCardOrder({
        cards: column.cards || [],
        cardId,
        targetIndex,
        optimisticUpdate: (cards) => {
          columns = columns.map((entry) =>
            entry.id === column.id ? { ...entry, cards } : entry
          );
        },
        publish: (positions) =>
          boards.reorderCards(owner, repo, boardId, { column_id: column.id, positions }),
        reload: refreshBoard,
      });
    });
  }

  async function refreshBoard() {
    if (!activeBoard) return;
    const b = await boards.get(owner, repo, activeBoard.id);
    activeBoard = b.board;
    columns = normalizeColumns(b);
  }

  function closeCreateModal() {
    showCreate = false;
  }

  function selectBoardByKey(e: KeyboardEvent, board: Board) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      selectBoard(board);
    }
  }
</script>

<svelte:head>
  <title>Board · {owner}/{repo} · ForgeKeep</title>
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

  {#if editingCard}
    <div class="modal-overlay-wrap">
      <button
        class="modal-overlay"
        type="button"
        aria-label={t('common.close')}
        onclick={() => (editingCard = null)}
      ></button>
      <div class="modal">
        <h3>{t('board.editCard')}</h3>
        <label class="modal-field">
          {t('board.cardNote')}
          <textarea class="input" rows="4" bind:value={cardForm.note}></textarea>
        </label>
        <label class="modal-field">
          {t('board.linkedIssue')}
          <select class="input" bind:value={cardForm.issueId}>
            <option value="">{t('board.noIssue')}</option>
            {#each issueOptions as issue (issue.id)}
              <option value={String(issue.id)}>#{issue.number} {issue.title}</option>
            {/each}
            {#if editingCard.issue && !issueOptions.some((issue) => issue.id === editingCard?.issue_id)}
              <option value={String(editingCard.issue.id)}>
                #{editingCard.issue.number} {editingCard.issue.title}
              </option>
            {/if}
          </select>
        </label>
        <div class="modal-actions">
          <button class="btn" onclick={() => (editingCard = null)}>{t('common.cancel')}</button>
          <button class="btn btn-primary" onclick={saveCard} disabled={boardMutationBusy} aria-busy={boardMutationBusy}>{t('common.save')}</button>
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
            class:active={selectedBoardId === b.id}
            aria-busy={boardSelectionBusy && selectedBoardId === b.id}
            onclick={() => { if (!boardMutationBusy) selectBoard(b); }}
            onkeydown={(e) => { if (!boardMutationBusy) selectBoardByKey(e, b); }}
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
              disabled={boardMutationBusy || boardSelectionBusy}
              aria-label={`${t('board.deleteBoard')} ${b.name}`}
            >
              &times;
            </button>
          </div>
        {/each}
      </div>

      {#if boardSelectionBusy}
        <p class="loading-text board-selection-loading">{t('common.loading')}...</p>
      {:else if activeBoard}
        <div class="board-header">
          <div>
            <h2>{activeBoard.name}</h2>
            {#if activeBoard.description}
              <p>{activeBoard.description}</p>
            {/if}
          </div>
          <div class="board-header-actions">
            <button class="btn btn-sm" onclick={startEditBoard}>{t('common.edit')}</button>
            <button class="btn btn-sm" onclick={() => (showAddCol = true)}>
              + {t('board.addColumn')}
            </button>
          </div>
        </div>

        {#if showEditBoard}
          <div class="inline-form board-edit-form">
            <input class="input" type="text" bind:value={boardForm.name} placeholder={t('board.namePlaceholder')} />
            <input class="input" type="text" bind:value={boardForm.description} placeholder={t('board.descPlaceholder')} />
            <button class="btn btn-primary btn-sm" onclick={saveBoard}>{t('common.save')}</button>
            <button class="btn btn-sm" onclick={() => (showEditBoard = false)}>{t('common.cancel')}</button>
          </div>
        {/if}

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
                {#if editingColumnId === col.id}
                  <input
                    class="input input-sm column-name-input"
                    bind:value={editColumnName}
                    onkeydown={(event) => {
                      if (event.key === 'Enter') saveColumn(col);
                      if (event.key === 'Escape') editingColumnId = null;
                    }}
                  />
                  <button class="btn-icon btn-icon-sm" onclick={() => saveColumn(col)} title={t('common.save')}>✓</button>
                  <button class="btn-icon btn-icon-sm" onclick={() => (editingColumnId = null)} title={t('common.cancel')}>×</button>
                {:else}
                  <strong>{col.name}</strong>
                  <button class="btn-icon btn-icon-sm" onclick={() => startEditColumn(col)} title={t('common.edit')}>✎</button>
                {/if}
                <span class="card-count">{(col.cards || []).length}</span>
                <button class="btn-icon" onclick={() => deleteColumn(col.id)} title={t('common.delete')}>&times;</button>
              </div>

              <div class="col-body">
                {#each (col.cards || []) as card, cardIndex (card.id)}
                  <div class="card">
                    <div class="card-header">
                      <span>{card.note || card.issue?.title || `#${card.issue_id}`}</span>
                      <div class="card-actions">
                        <button
                          class="btn-icon btn-icon-sm"
                          disabled={boardMutationBusy || cardIndex === 0}
                          aria-busy={boardMutationBusy}
                          onclick={() => reorderCard(col, card.id, cardIndex - 1)}
                          title="Move card up"
                        >↑</button>
                        <button
                          class="btn-icon btn-icon-sm"
                          disabled={boardMutationBusy || cardIndex === (col.cards || []).length - 1}
                          aria-busy={boardMutationBusy}
                          onclick={() => reorderCard(col, card.id, cardIndex + 1)}
                          title="Move card down"
                        >↓</button>
                        <button class="btn-icon btn-icon-sm" onclick={() => startEditCard(card)} title={t('common.edit')} disabled={boardMutationBusy}>✎</button>
                        <button class="btn-icon btn-icon-sm" onclick={() => deleteCard(card.id, col.id)} title={t('common.delete')} disabled={boardMutationBusy} aria-busy={boardMutationBusy}>&times;</button>
                      </div>
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
                      disabled={boardMutationBusy}
                      aria-busy={boardMutationBusy}
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

  .board-header { display:flex; align-items:flex-start; justify-content:space-between; gap:12px; margin-bottom:12px; }
  .board-header p { margin:4px 0 0; color:var(--text-secondary, #666); font-size:13px; }
  .board-header-actions { display:flex; gap:8px; }
  .inline-form { display:flex; gap:8px; align-items:center; margin-bottom:12px; }
  .board-edit-form .input { margin-bottom:0; }

  /* Kanban */
  .kanban-board { display:flex; gap:12px; overflow-x:auto; padding-bottom:12px; min-height:200px; }
  .kanban-column { background:var(--bg-secondary, #f9fafb); border:1px solid var(--border-color, #e5e7eb); border-radius:8px; min-width:260px; max-width:320px; display:flex; flex-direction:column; }
  .col-header { padding:10px 12px; border-bottom:1px solid var(--border-color, #e5e7eb); display:flex; align-items:center; gap:8px; font-size:13px; }
  .card-count { background:var(--bg-tertiary, #e5e7eb); border-radius:10px; padding:1px 8px; font-size:11px; color:var(--text-secondary, #666); margin-left:auto; }
  .col-body { padding:8px; flex:1; display:flex; flex-direction:column; gap:6px; }

  .card { background:var(--bg-primary, #fff); border:1px solid var(--border-color, #e5e7eb); border-radius:6px; padding:8px 10px; font-size:13px; }
  .card-header { display:flex; justify-content:space-between; align-items:flex-start; gap:4px; }
  .card-actions { display:flex; gap:2px; }
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
  .modal-field { display:flex; flex-direction:column; gap:4px; margin-top:10px; color:var(--text-secondary, #666); font-size:12px; }
  .modal-field textarea { resize:vertical; }

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
  .column-name-input { min-width:80px; }
</style>
