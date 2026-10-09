<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import Modal from '$lib/components/Modal.svelte';
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
  import {
    LatestRepositoryRequestFence,
    LatestRepositoryResourceRequestFence,
  } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  // Every board mutation is `RepoWrite`; a reader sees the boards and nothing
  // to change them with (card_270a0a77fd79).
  const permission = viewerPermission(() => owner, () => repo);
  const canWrite = $derived(permission.canWrite);

  let boardList = $state<Board[]>([]);
  let activeBoard = $state<Board | null>(null);
  let columns = $state<BoardColumn[]>([]);
  let issueOptions = $state<Issue[]>([]);
  let loading = $state(true);
  let error = $state('');
  let boardMutationBusy = $state(false);
  let boardSelectionBusy = $state(false);
  let boardControlsBusy = $derived(boardMutationBusy || boardSelectionBusy);
  let selectedBoardId = $state<number | null>(null);
  const boardListRequests = new LatestRepositoryRequestFence();
  const boardSelectionRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;

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

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    boardList = [];
    activeBoard = null;
    columns = [];
    issueOptions = [];
    loading = true;
    error = '';
    boardMutationBusy = false;
    boardSelectionBusy = false;
    selectedBoardId = null;
    showCreate = false;
    newBoardName = '';
    newBoardDesc = '';
    showEditBoard = false;
    boardForm = { name: '', description: '' };
    showAddCol = false;
    newColName = '';
    editingColumnId = null;
    editColumnName = '';
    showAddCard = null;
    newCardTitle = '';
    editingCard = null;
    cardForm = { note: '', issueId: '' };
    void loadBoards(expectedOwner, expectedRepo, routeGeneration);
  });

  type BoardRoute = Readonly<{ owner: string; repo: string; generation: number }>;

  function currentRoute(): BoardRoute {
    return { owner, repo, generation: routeGeneration };
  }

  function isCurrentRoute(route: BoardRoute): boolean {
    return routeGeneration === route.generation && owner === route.owner && repo === route.repo;
  }

  async function runBoardMutation(
    operation: (route: BoardRoute) => Promise<void>,
  ): Promise<boolean> {
    if (boardControlsBusy) return false;
    const route = currentRoute();
    boardMutationBusy = true;
    error = '';
    try {
      await operation(route);
      return isCurrentRoute(route);
    } catch (e: any) {
      if (isCurrentRoute(route)) error = e.message;
      return false;
    } finally {
      if (isCurrentRoute(route)) boardMutationBusy = false;
    }
  }

  function normalizeColumns(board: BoardFullResponse): BoardColumn[] {
    return board.columns.map((entry) => ({ ...entry.column, cards: entry.cards || [] }));
  }

  async function loadBoards(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedRoute = routeGeneration,
  ) {
    const route = { owner: expectedOwner, repo: expectedRepo, generation: expectedRoute };
    if (!isCurrentRoute(route)) return;
    const claim = boardListRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      const [boardsData, issuesData] = await Promise.all([
        boards.list(expectedOwner, expectedRepo),
        issues.list(expectedOwner, expectedRepo, undefined, 1, 100),
      ]);
      if (!boardListRequests.owns(claim, owner, repo) || !isCurrentRoute(route)) return;
      boardList = boardsData;
      issueOptions = issuesData.data;
      if (boardsData.length > 0) {
        await selectBoard(boardsData[0], route);
      }
    } catch (e: any) {
      if (boardListRequests.owns(claim, owner, repo) && isCurrentRoute(route)) {
        error = e.message;
      }
    } finally {
      if (boardListRequests.owns(claim, owner, repo) && isCurrentRoute(route)) {
        loading = false;
      }
    }
  }

  async function selectBoard(board: Board, route = currentRoute()) {
    if (!isCurrentRoute(route)) return;
    const expectedOwner = route.owner;
    const expectedRepo = route.repo;
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
        !isCurrentRoute(route) ||
        selectedBoardId !== board.id ||
        !boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) return;
      if (b.board.id !== board.id) throw new Error(t('board.identity_mismatch'));
      activeBoard = b.board;
      columns = normalizeColumns(b);
    } catch (e: any) {
      if (
        isCurrentRoute(route) &&
        selectedBoardId === board.id &&
        boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) error = e.message;
    } finally {
      if (
        isCurrentRoute(route) &&
        selectedBoardId === board.id &&
        boardSelectionRequests.owns(claim, owner, repo, board.id)
      ) boardSelectionBusy = false;
    }
  }

  async function createBoard() {
    const name = newBoardName.trim();
    const description = newBoardDesc.trim();
    if (!name) return;
    await runBoardMutation(async (route) => {
      const b = await boards.create(route.owner, route.repo, {
        name,
        description: description || undefined,
      });
      if (!isCurrentRoute(route)) return;
      boardList = [b, ...boardList];
      showCreate = false;
      newBoardName = '';
      newBoardDesc = '';
      await selectBoard(b, route);
    });
  }

  async function deleteBoard(id: number) {
    if (!(await confirmer.ask({
      title: t('board.deleteBoard'),
      message: t('board.confirmDelete'),
      confirmLabel: t('common.delete'),
    }))) return;
    await runBoardMutation(async (route) => {
      await boards.delete(route.owner, route.repo, id);
      if (!isCurrentRoute(route)) return;
      boardList = boardList.filter(b => b.id !== id);
      selectedBoardId = null;
      activeBoard = null;
      columns = [];
      if (boardList.length > 0) await selectBoard(boardList[0], route);
    });
  }

  function startEditBoard() {
    if (boardControlsBusy || !activeBoard) return;
    boardForm = {
      name: activeBoard.name,
      description: activeBoard.description || '',
    };
    showEditBoard = true;
  }

  async function saveBoard() {
    if (!activeBoard || !boardForm.name.trim()) return;
    const boardId = activeBoard.id;
    const form = { ...boardForm };
    await runBoardMutation(async (route) => {
      const updated = await boards.update(
        route.owner,
        route.repo,
        boardId,
        buildBoardUpdatePayload(form),
      );
      if (!isCurrentRoute(route)) return;
      activeBoard = updated;
      boardList = boardList.map((board) => (board.id === updated.id ? updated : board));
      showEditBoard = false;
    });
  }

  async function addColumn() {
    const name = newColName.trim();
    if (!name || !activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async (route) => {
      const col = await boards.createColumn(route.owner, route.repo, boardId, {
        name,
      });
      if (!isCurrentRoute(route)) return;
      columns = [...columns, { ...col, cards: [] }];
      showAddCol = false;
      newColName = '';
    });
  }

  async function deleteColumn(colId: number) {
    if (!activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async (route) => {
      await boards.deleteColumn(route.owner, route.repo, boardId, colId);
      if (!isCurrentRoute(route)) return;
      columns = columns.filter(c => c.id !== colId);
    });
  }

  function startEditColumn(column: BoardColumn) {
    if (boardControlsBusy) return;
    editingColumnId = column.id;
    editColumnName = column.name;
  }

  async function saveColumn(column: BoardColumn) {
    if (!activeBoard || !editColumnName.trim()) return;
    const boardId = activeBoard.id;
    const name = editColumnName;
    await runBoardMutation(async (route) => {
      const updated = await boards.updateColumn(
        route.owner,
        route.repo,
        boardId,
        column.id,
        buildColumnUpdatePayload(name),
      );
      if (!isCurrentRoute(route)) return;
      columns = columns.map((entry) =>
        entry.id === updated.id ? { ...entry, ...updated } : entry
      );
      editingColumnId = null;
      editColumnName = '';
    });
  }

  async function addCard(colId: number) {
    const note = newCardTitle.trim();
    if (!note || !activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async (route) => {
      const card = await boards.createCard(route.owner, route.repo, boardId, colId, {
        note,
      });
      if (!isCurrentRoute(route)) return;
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
    await runBoardMutation(async (route) => {
      await boards.deleteCard(route.owner, route.repo, boardId, cardId);
      if (!isCurrentRoute(route)) return;
      const col = columns.find(c => c.id === colId);
      if (col) {
        col.cards = (col.cards || []).filter((c: any) => c.id !== cardId);
        columns = [...columns];
      }
    });
  }

  function startEditCard(card: BoardCard) {
    if (boardControlsBusy) return;
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
    const form = { ...cardForm };
    await runBoardMutation(async (route) => {
      await boards.updateCard(
        route.owner,
        route.repo,
        boardId,
        cardId,
        buildBoardCardUpdatePayload(form),
      );
      if (!isCurrentRoute(route)) return;
      editingCard = null;
      await refreshBoard(route, boardId);
    });
  }

  async function moveCard(cardId: number, fromColId: number, toColId: number) {
    if (!activeBoard || fromColId === toColId) return;
    const boardId = activeBoard.id;
    const targetCol = columns.find(c => c.id === toColId);
    const position = targetCol ? (targetCol.cards || []).length : 0;
    await runBoardMutation(async (route) => {
      await boards.moveCard(route.owner, route.repo, boardId, cardId, {
        column_id: toColId,
        position,
      });
      if (!isCurrentRoute(route)) return;
      await refreshBoard(route, boardId);
    });
  }

  async function reorderCard(column: BoardColumn, cardId: number, targetIndex: number) {
    if (!activeBoard) return;
    const boardId = activeBoard.id;
    await runBoardMutation(async (route) => {
      await publishBoardCardOrder({
        cards: column.cards || [],
        cardId,
        targetIndex,
        optimisticUpdate: (cards) => {
          if (!isCurrentRoute(route)) return;
          columns = columns.map((entry) =>
            entry.id === column.id ? { ...entry, cards } : entry
          );
        },
        publish: (positions) =>
          boards.reorderCards(route.owner, route.repo, boardId, {
            column_id: column.id,
            positions,
          }),
        reload: () => refreshBoard(route, boardId),
      });
    });
  }

  async function refreshBoard(route: BoardRoute, boardId: number) {
    if (!isCurrentRoute(route) || activeBoard?.id !== boardId) return;
    const b = await boards.get(route.owner, route.repo, boardId);
    if (!isCurrentRoute(route) || activeBoard?.id !== boardId) return;
    if (b.board.id !== boardId) throw new Error(t('board.identity_mismatch'));
    activeBoard = b.board;
    columns = normalizeColumns(b);
  }

  function closeCreateModal() {
    showCreate = false;
  }

  function openCreateModal() {
    if (boardControlsBusy) return;
    showCreate = true;
  }

  function openAddColumnForm() {
    if (boardControlsBusy) return;
    showAddCol = true;
  }

  function openAddCardForm(columnId: number) {
    if (boardControlsBusy) return;
    showAddCard = columnId;
  }

  function selectBoardByKey(e: KeyboardEvent, board: Board) {
    if (boardMutationBusy) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      selectBoard(board);
    }
  }
</script>

<svelte:head>
  <title>{t('board.page_title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="board" />

  <div class="page-header">
    <h1>{t('board.title')}</h1>
    {#if canWrite}
      <button
        class="btn btn-primary"
        onclick={openCreateModal}
        disabled={boardControlsBusy}
        aria-busy={boardControlsBusy}
      >
        + {t('board.createBoard')}
      </button>
    {/if}
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if showCreate}
    <Modal onclose={closeCreateModal} labelledby="board-create-title" width="400px" padding="20px">
      <div class="modal">
        <h3 id="board-create-title">{t('board.createBoard')}</h3>
        <input class="input" type="text" bind:value={newBoardName} placeholder={t('board.namePlaceholder')} disabled={boardControlsBusy} />
        <input class="input" type="text" bind:value={newBoardDesc} placeholder={t('board.descPlaceholder')} disabled={boardControlsBusy} />
        <div class="modal-actions">
          <button class="btn" onclick={closeCreateModal}>{t('common.cancel')}</button>
          <button class="btn btn-primary" onclick={createBoard} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.create')}</button>
        </div>
      </div>
    </Modal>
  {/if}

  {#if editingCard}
    <Modal onclose={() => (editingCard = null)} labelledby="board-card-edit-title" width="400px" padding="20px">
      <div class="modal">
        <h3 id="board-card-edit-title">{t('board.editCard')}</h3>
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
          <button class="btn btn-primary" onclick={saveCard} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.save')}</button>
        </div>
      </div>
    </Modal>
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
            aria-disabled={boardMutationBusy}
            aria-busy={boardSelectionBusy && selectedBoardId === b.id}
            onclick={() => { if (!boardMutationBusy) selectBoard(b); }}
            onkeydown={(e) => selectBoardByKey(e, b)}
            role="button"
            tabindex={boardMutationBusy ? -1 : 0}
          >
            {b.name}
            {#if canWrite}
              <button
                type="button"
                class="close"
                onclick={(e) => {
                  e.stopPropagation();
                  deleteBoard(b.id);
                }}
                disabled={boardControlsBusy}
                aria-busy={boardControlsBusy}
                aria-label={`${t('board.deleteBoard')} ${b.name}`}
              >
                &times;
              </button>
            {/if}
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
          {#if canWrite}
            <div class="board-header-actions">
              <button class="btn btn-sm" onclick={startEditBoard} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.edit')}</button>
              <button class="btn btn-sm" onclick={openAddColumnForm} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>
                + {t('board.addColumn')}
              </button>
            </div>
          {/if}
        </div>

        {#if showEditBoard}
          <div class="inline-form board-edit-form">
            <input class="input" type="text" bind:value={boardForm.name} placeholder={t('board.namePlaceholder')} disabled={boardControlsBusy} />
            <input class="input" type="text" bind:value={boardForm.description} placeholder={t('board.descPlaceholder')} disabled={boardControlsBusy} />
            <button class="btn btn-primary btn-sm" onclick={saveBoard} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.save')}</button>
            <button class="btn btn-sm" onclick={() => (showEditBoard = false)}>{t('common.cancel')}</button>
          </div>
        {/if}

        {#if showAddCol}
          <div class="inline-form">
            <input class="input" type="text" bind:value={newColName} placeholder={t('board.colNamePlaceholder')} disabled={boardControlsBusy} />
            <button class="btn btn-primary btn-sm" onclick={addColumn} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.add')}</button>
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
                    disabled={boardControlsBusy}
                    onkeydown={(event) => {
                      if (event.key === 'Enter') saveColumn(col);
                      if (event.key === 'Escape') editingColumnId = null;
                    }}
                  />
                  <button class="btn-icon btn-icon-sm" onclick={() => saveColumn(col)} title={t('common.save')} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>✓</button>
                  <button class="btn-icon btn-icon-sm" onclick={() => (editingColumnId = null)} title={t('common.cancel')}>×</button>
                {:else}
                  <strong>{col.name}</strong>
                  {#if canWrite}
                    <button class="btn-icon btn-icon-sm" onclick={() => startEditColumn(col)} title={t('common.edit')} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>✎</button>
                  {/if}
                {/if}
                <span class="card-count">{(col.cards || []).length}</span>
                {#if canWrite}
                  <button class="btn-icon" onclick={() => deleteColumn(col.id)} title={t('common.delete')} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>&times;</button>
                {/if}
              </div>

              <div class="col-body">
                {#each (col.cards || []) as card, cardIndex (card.id)}
                  <div class="card">
                    <div class="card-header">
                      <span>{card.note || card.issue?.title || `#${card.issue_id}`}</span>
                      {#if canWrite}
                      <div class="card-actions">
                        <button
                          class="btn-icon btn-icon-sm"
                          disabled={boardControlsBusy || cardIndex === 0}
                          aria-busy={boardControlsBusy}
                          onclick={() => reorderCard(col, card.id, cardIndex - 1)}
                          title={t('board.move_card_up')}
                        >↑</button>
                        <button
                          class="btn-icon btn-icon-sm"
                          disabled={boardControlsBusy || cardIndex === (col.cards || []).length - 1}
                          aria-busy={boardControlsBusy}
                          onclick={() => reorderCard(col, card.id, cardIndex + 1)}
                          title={t('board.move_card_down')}
                        >↓</button>
                        <button class="btn-icon btn-icon-sm" onclick={() => startEditCard(card)} title={t('common.edit')} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>✎</button>
                        <button class="btn-icon btn-icon-sm" onclick={() => deleteCard(card.id, col.id)} title={t('common.delete')} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>&times;</button>
                      </div>
                      {/if}
                    </div>
                    {#if card.issue}
                      <a class="card-link" href={`/${owner}/${repo}/issues/${card.issue.number}`}>
                        #{card.issue.number} {card.issue.title}
                      </a>
                    {/if}
                    <!-- Move dropdown -->
                    {#if canWrite}
                    <select
                      class="card-move"
                      value={col.id}
                      disabled={boardControlsBusy}
                      aria-busy={boardControlsBusy}
                      onchange={(e) => moveCard(card.id, col.id, parseInt((e.target as HTMLSelectElement).value))}
                    >
                      <option value="" disabled>{t('board.moveTo')}</option>
                      {#each columns.filter((c: any) => c.id !== col.id) as targetCol}
                        <option value={targetCol.id}>{targetCol.name}</option>
                      {/each}
                    </select>
                    {/if}
                  </div>
                {/each}

                {#if showAddCard === col.id}
                  <div class="inline-form">
                    <input class="input input-sm" type="text" bind:value={newCardTitle} placeholder={t('board.cardTitlePlaceholder')} disabled={boardControlsBusy} />
                    <button class="btn btn-primary btn-sm" onclick={() => addCard(col.id)} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>{t('common.add')}</button>
                    <button class="btn btn-sm" onclick={() => { showAddCard = null; newCardTitle = ''; }}>{t('common.cancel')}</button>
                  </div>
                {:else if canWrite}
                  <button class="btn btn-ghost btn-sm add-card-btn" onclick={() => openAddCardForm(col.id)} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>
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

<ConfirmModal {confirmer} />

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
  /* The panel, backdrop and focus handling are lib/components/Modal.svelte. */
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
