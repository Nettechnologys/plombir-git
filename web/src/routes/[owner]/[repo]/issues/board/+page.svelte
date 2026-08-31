<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { boards } from '$lib/api/client.svelte';
  import { publishBoardCardOrder } from '$lib/api/boardOrder';
  import {
    LatestRepositoryRequestFence,
    LatestRepositoryResourceRequestFence,
  } from '$lib/asyncStateOwnership';

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let boardList = $state<any[]>([]);
  let activeBoardId = $state<number | null>(null);
  let activeBoard = $state<any | null>(null);
  let loading = $state(true);
  let error = $state('');
  let boardMutationBusy = $state(false);
  let boardSelectionBusy = $state(false);
  let boardControlsBusy = $derived(boardMutationBusy || boardSelectionBusy);
  const boardListRequests = new LatestRepositoryRequestFence();
  const boardSelectionRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;

  // Board creation
  let showCreateBoard = $state(false);
  let newBoardName = $state('');

  // Column creation
  let showAddColumn = $state(false);
  let newColumnName = $state('');

  // Card creation: keyed by column id
  let showAddCard = $state<Record<number, boolean>>({});
  let newCardNote = $state<Record<number, string>>({});

  // Drag state
  let draggingCardId = $state<number | null>(null);
  let draggingFromColId = $state<number | null>(null);
  let dragOverColId = $state<number | null>(null);

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    boardList = [];
    activeBoardId = null;
    activeBoard = null;
    loading = true;
    error = '';
    boardMutationBusy = false;
    boardSelectionBusy = false;
    showCreateBoard = false;
    newBoardName = '';
    showAddColumn = false;
    newColumnName = '';
    showAddCard = {};
    newCardNote = {};
    draggingCardId = null;
    draggingFromColId = null;
    dragOverColId = null;
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
      error = '';
      const boardsData = await boards.list(expectedOwner, expectedRepo);
      if (!boardListRequests.owns(claim, owner, repo) || !isCurrentRoute(route)) return;
      boardList = boardsData;
      const nextBoardId =
        activeBoardId !== null && boardsData.some((board) => board.id === activeBoardId)
          ? activeBoardId
          : (boardsData[0]?.id ?? null);
      activeBoardId = nextBoardId;
      if (nextBoardId !== null) {
        await loadBoard(nextBoardId, route);
      } else {
        activeBoard = null;
        boardSelectionBusy = false;
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

  async function loadBoard(id: number, route = currentRoute()) {
    if (!isCurrentRoute(route)) return;
    const expectedOwner = route.owner;
    const expectedRepo = route.repo;
    const claim = boardSelectionRequests.begin(expectedOwner, expectedRepo, id);
    activeBoardId = id;
    boardSelectionBusy = true;
    error = '';
    activeBoard = null;
    showAddCard = {};
    draggingCardId = null;
    draggingFromColId = null;
    dragOverColId = null;
    try {
      const board = await boards.get(expectedOwner, expectedRepo, id);
      if (
        !isCurrentRoute(route) ||
        activeBoardId !== id ||
        !boardSelectionRequests.owns(claim, owner, repo, id)
      ) return;
      if (board.board.id !== id) throw new Error('Board response identity mismatch');
      activeBoard = board;
    } catch (e: any) {
      if (
        isCurrentRoute(route) &&
        activeBoardId === id &&
        boardSelectionRequests.owns(claim, owner, repo, id)
      ) error = e.message;
    } finally {
      if (
        isCurrentRoute(route) &&
        activeBoardId === id &&
        boardSelectionRequests.owns(claim, owner, repo, id)
      ) boardSelectionBusy = false;
    }
  }

  async function handleCreateBoard() {
    const name = newBoardName.trim();
    if (!name) return;
    await runBoardMutation(async (route) => {
      const board = await boards.create(route.owner, route.repo, { name });
      if (!isCurrentRoute(route)) return;
      newBoardName = '';
      showCreateBoard = false;
      activeBoardId = board.id;
      await loadBoards(route.owner, route.repo, route.generation);
    });
  }

  async function handleAddColumn() {
    const name = newColumnName.trim();
    const boardId = activeBoardId;
    if (!name || boardId === null) return;
    await runBoardMutation(async (route) => {
      await boards.createColumn(route.owner, route.repo, boardId, { name });
      if (!isCurrentRoute(route)) return;
      newColumnName = '';
      showAddColumn = false;
      await loadBoard(boardId, route);
    });
  }

  async function handleDeleteColumn(colId: number) {
    if (!confirm('Delete this column and all its cards?')) return;
    const boardId = activeBoardId;
    if (boardId === null) return;
    await runBoardMutation(async (route) => {
      await boards.deleteColumn(route.owner, route.repo, boardId, colId);
      if (!isCurrentRoute(route)) return;
      await loadBoard(boardId, route);
    });
  }

  async function handleAddCard(colId: number) {
    const note = (newCardNote[colId] ?? '').trim();
    const boardId = activeBoardId;
    if (!note || boardId === null) return;
    await runBoardMutation(async (route) => {
      await boards.createCard(route.owner, route.repo, boardId, colId, { note });
      if (!isCurrentRoute(route)) return;
      newCardNote = { ...newCardNote, [colId]: '' };
      showAddCard = { ...showAddCard, [colId]: false };
      await loadBoard(boardId, route);
    });
  }

  async function handleDeleteCard(cardId: number) {
    const boardId = activeBoardId;
    if (boardId === null) return;
    await runBoardMutation(async (route) => {
      await boards.deleteCard(route.owner, route.repo, boardId, cardId);
      if (!isCurrentRoute(route)) return;
      await loadBoard(boardId, route);
    });
  }

  function toggleCreateBoardForm() {
    if (boardControlsBusy) return;
    showCreateBoard = !showCreateBoard;
  }

  function toggleAddColumnForm() {
    if (boardControlsBusy || activeBoardId === null) return;
    showAddColumn = !showAddColumn;
  }

  function openAddCardForm(columnId: number) {
    if (boardControlsBusy) return;
    showAddCard = { ...showAddCard, [columnId]: true };
  }

  function selectBoard(id: number) {
    if (boardMutationBusy) return;
    loadBoard(id);
  }

  // ── Drag & Drop ──────────────────────────────────
  function onDragStart(e: DragEvent, cardId: number, colId: number) {
    if (boardControlsBusy) {
      e.preventDefault();
      return;
    }
    draggingCardId = cardId;
    draggingFromColId = colId;
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function onDragOver(e: DragEvent, colId: number) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dragOverColId = colId;
  }

  function onDragLeave(e: DragEvent) {
    const rel = e.relatedTarget as Element | null;
    if (!rel || !(e.currentTarget as Element).contains(rel)) {
      dragOverColId = null;
    }
  }

  function setColumnCards(colId: number, cards: any[]) {
    if (!activeBoard?.columns) return;
    activeBoard = {
      ...activeBoard,
      columns: activeBoard.columns.map((entry: any) =>
        entry.column.id === colId ? { ...entry, cards } : entry
      ),
    };
  }

  async function reorderCard(
    route: BoardRoute,
    boardId: number,
    colId: number,
    cardId: number,
    targetIndex: number,
  ) {
    if (!isCurrentRoute(route)) return;
    const column = activeBoard?.columns?.find((entry: any) => entry.column.id === colId);
    if (!column) return;

    await publishBoardCardOrder({
      cards: column.cards,
      cardId,
      targetIndex,
      optimisticUpdate: (cards) => {
        if (isCurrentRoute(route)) setColumnCards(colId, cards);
      },
      publish: (positions) =>
        boards.reorderCards(route.owner, route.repo, boardId, {
          column_id: colId,
          positions,
        }),
      reload: () => loadBoard(boardId, route),
    });
  }

  async function onDrop(e: DragEvent, colId: number, targetIndex?: number) {
    e.preventDefault();
    e.stopPropagation();
    dragOverColId = null;
    if (draggingCardId === null || draggingFromColId === null) return;
    if (boardControlsBusy) {
      draggingCardId = null;
      draggingFromColId = null;
      return;
    }
    if (activeBoardId === null) return;

    const boardId = activeBoardId;
    const cardId = draggingCardId;
    const fromColId = draggingFromColId;
    const targetCol = activeBoard?.columns?.find((c: any) => c.column.id === colId);

    await runBoardMutation(async (route) => {
      if (!isCurrentRoute(route)) return;
      draggingCardId = null;
      draggingFromColId = null;
      if (fromColId === colId) {
        const position = targetIndex ?? Math.max((targetCol?.cards.length ?? 1) - 1, 0);
        await reorderCard(route, boardId, colId, cardId, position);
        return;
      }

      const position = targetIndex ?? targetCol?.cards.length ?? 0;
      await boards.moveCard(route.owner, route.repo, boardId, cardId, {
        column_id: colId,
        position,
      });
      if (!isCurrentRoute(route)) return;
      await loadBoard(boardId, route);
    });
  }
</script>

<svelte:head>
  <title>Board · {owner}/{repo} · ForgeKeep</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="board" starsCount={0} />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="loading-text">Loading…</p>
  {:else if boardList.length === 0 && !showCreateBoard}
    <!-- Empty state -->
    <div class="empty-state">
      <div class="empty-icon">📋</div>
      <h2>No boards yet</h2>
      <p>Create your first project board to organize issues.</p>
      <button
        class="btn-primary"
        onclick={toggleCreateBoardForm}
        disabled={boardControlsBusy}
        aria-busy={boardControlsBusy}
      >Create Board</button>
    </div>
  {:else}
    <!-- Board selector + controls -->
    <div class="board-toolbar">
      <div class="board-tabs">
        {#each boardList as b}
          <button
            class="board-tab"
            class:active={b.id === activeBoardId}
            disabled={boardMutationBusy}
            aria-busy={boardMutationBusy || (boardSelectionBusy && activeBoardId === b.id)}
            onclick={() => selectBoard(b.id)}
          >{b.name}</button>
        {/each}
        <button
          class="btn-ghost btn-sm"
          onclick={toggleCreateBoardForm}
          disabled={boardControlsBusy}
          aria-busy={boardControlsBusy}
        >+ Board</button>
      </div>
      <button
        class="btn-outline btn-sm"
        onclick={toggleAddColumnForm}
        disabled={boardControlsBusy || activeBoardId === null}
        aria-busy={boardControlsBusy}
      >+ Column</button>
    </div>

    {#if showCreateBoard}
      <div class="inline-form">
        <input
          class="form-input"
          placeholder="Board name"
          bind:value={newBoardName}
          disabled={boardControlsBusy}
          onkeydown={(e) => e.key === 'Enter' && handleCreateBoard()}
        />
        <button class="btn-primary btn-sm" onclick={handleCreateBoard} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>
          {boardControlsBusy ? '…' : 'Create'}
        </button>
        <button class="btn-ghost btn-sm" onclick={() => { showCreateBoard = false; newBoardName = ''; }}>Cancel</button>
      </div>
    {/if}

    {#if showAddColumn}
      <div class="inline-form">
        <input
          class="form-input"
          placeholder="Column name"
          bind:value={newColumnName}
          disabled={boardControlsBusy}
          onkeydown={(e) => e.key === 'Enter' && handleAddColumn()}
        />
        <button class="btn-primary btn-sm" onclick={handleAddColumn} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>Add</button>
        <button class="btn-ghost btn-sm" onclick={() => { showAddColumn = false; newColumnName = ''; }}>Cancel</button>
      </div>
    {/if}

    <!-- Board columns -->
    {#if boardSelectionBusy}
      <p class="loading-text board-selection-loading">Loading…</p>
    {:else if activeBoard?.columns}
      <div class="board-container">
        {#each activeBoard.columns as { column, cards } (column.id)}
          <div
            class="board-column"
            class:drag-over={dragOverColId === column.id}
            ondragover={(e) => onDragOver(e, column.id)}
            ondragleave={(e) => onDragLeave(e)}
            ondrop={(e) => onDrop(e, column.id)}
            role="list"
            aria-label={column.name}
          >
            <div class="column-header" style="border-top: 3px solid {column.color || '#6366f1'}">
              <span class="column-name">{column.name}</span>
              <div class="column-actions">
                <span class="card-count">{cards.length}</span>
                <button
                  class="btn-ghost btn-xs"
                  onclick={() => handleDeleteColumn(column.id)}
                  title="Delete column"
                  disabled={boardControlsBusy}
                  aria-busy={boardControlsBusy}
                >✕</button>
              </div>
            </div>

            <div class="column-body" role="listitem">
              {#each cards as card, cardIndex (card.id)}
                <div
                  class="card"
                  class:dragging={draggingCardId === card.id}
                  draggable={!boardControlsBusy}
                  ondragstart={(e) => onDragStart(e, card.id, column.id)}
                  ondrop={(e) =>
                    onDrop(
                      e,
                      column.id,
                      draggingFromColId === column.id ? cardIndex : undefined,
                    )}
                  role="button"
                  aria-disabled={boardControlsBusy}
                  aria-busy={boardControlsBusy}
                  tabindex={boardControlsBusy ? -1 : 0}
                >
                  <div class="card-content">
                    {#if card.issue}
                      <a href={`/${owner}/${repo}/issues/${card.issue.number}`} class="card-issue-link">
                        #{card.issue.number}
                      </a>
                    {/if}
                    <span class="card-note">{card.note || ''}</span>
                  </div>
                  <button
                    class="card-delete"
                    onclick={() => handleDeleteCard(card.id)}
                    title="Remove card"
                    disabled={boardControlsBusy}
                    aria-busy={boardControlsBusy}
                  >✕</button>
                </div>
              {/each}

              <!-- Add card -->
              {#if showAddCard[column.id]}
                <div class="add-card-form">
                  <textarea
                    class="card-textarea"
                    rows="2"
                    placeholder="Add a note…"
                    bind:value={newCardNote[column.id]}
                    disabled={boardControlsBusy}
                    onkeydown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleAddCard(column.id); } }}
                  ></textarea>
                  <div class="add-card-actions">
                    <button class="btn-primary btn-xs" onclick={() => handleAddCard(column.id)} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>Add</button>
                    <button class="btn-ghost btn-xs" onclick={() => showAddCard = { ...showAddCard, [column.id]: false }}>Cancel</button>
                  </div>
                </div>
              {:else}
                <button class="add-card-btn" onclick={() => openAddCardForm(column.id)} disabled={boardControlsBusy} aria-busy={boardControlsBusy}>
                  + Add card
                </button>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
.loading-text { color: var(--text-secondary); text-align: center; padding: 48px; }

  /* ── Empty state ── */
  .empty-state {
    text-align: center; padding: 80px 24px;
    display: flex; flex-direction: column; align-items: center; gap: 12px;
  }
  .empty-icon { font-size: 48px; }
  .empty-state h2 { font-size: 20px; font-weight: 600; margin: 0; }
  .empty-state p { color: var(--text-secondary); font-size: 14px; margin: 0; }

  /* ── Toolbar ── */
  .board-toolbar {
    display: flex; align-items: center; justify-content: space-between;
    margin-bottom: 16px; flex-wrap: wrap; gap: 8px;
  }
  .board-tabs { display: flex; gap: 4px; align-items: center; flex-wrap: wrap; }
  .board-tab {
    padding: 5px 12px; border: 1px solid var(--border);
    border-radius: var(--radius); background: none; color: var(--text-primary);
    font-size: 13px; cursor: pointer;
  }
  .board-tab:hover { background: var(--bg-secondary); }
  .board-tab.active { background: var(--accent); color: #fff; border-color: var(--accent); }

  /* ── Inline forms ── */
  .inline-form {
    display: flex; gap: 8px; align-items: center;
    margin-bottom: 16px; flex-wrap: wrap;
  }
  .form-input {
    flex: 1; min-width: 180px; padding: 6px 12px;
    border: 1px solid var(--border); border-radius: var(--radius);
    background: var(--bg-primary); color: var(--text-primary); font-size: 14px;
  }

  /* ── Board layout ── */
  .board-container {
    display: flex; gap: 16px; overflow-x: auto; padding-bottom: 16px;
    align-items: flex-start;
  }

  .board-column {
    flex: 0 0 280px; background: var(--bg-secondary);
    border: 1px solid var(--border); border-radius: var(--radius);
    display: flex; flex-direction: column; min-height: 200px;
    transition: background 0.15s;
  }
  .board-column.drag-over { background: var(--bg-hover); border-color: var(--accent); }

  .column-header {
    display: flex; align-items: center; justify-content: space-between;
    padding: 10px 12px; border-bottom: 1px solid var(--border);
  }
  .column-name { font-size: 13px; font-weight: 600; }
  .column-actions { display: flex; align-items: center; gap: 4px; }
  .card-count {
    background: var(--bg-tertiary); color: var(--text-muted);
    border-radius: 10px; font-size: 11px; font-weight: 600; padding: 1px 7px;
  }

  .column-body { padding: 8px; flex: 1; display: flex; flex-direction: column; gap: 8px; }

  /* ── Cards ── */
  .card {
    background: var(--bg-primary); border: 1px solid var(--border);
    border-radius: var(--radius); padding: 10px 10px 10px 12px;
    cursor: grab; transition: border-color 0.15s, box-shadow 0.15s;
    display: flex; align-items: flex-start; justify-content: space-between; gap: 8px;
  }
  .card:hover { border-color: var(--text-muted); box-shadow: 0 1px 4px rgba(0,0,0,0.15); }
  .card.dragging { opacity: 0.4; cursor: grabbing; }
  .card-content { flex: 1; min-width: 0; }
  .card-issue-link {
    font-size: 11px; color: var(--accent); text-decoration: none; font-weight: 600;
    display: block; margin-bottom: 2px;
  }
  .card-note { font-size: 13px; color: var(--text-primary); word-break: break-word; }
  .card-delete {
    flex: 0 0 auto; background: none; border: none; color: var(--text-muted);
    font-size: 12px; cursor: pointer; padding: 0; line-height: 1;
    opacity: 0; transition: opacity 0.1s;
  }
  .card:hover .card-delete { opacity: 1; }
  .card-delete:hover { color: var(--red); }

  /* ── Add card ── */
  .add-card-btn {
    display: block; width: 100%; padding: 7px; background: none; border: none;
    color: var(--text-muted); font-size: 13px; text-align: left; cursor: pointer;
    border-radius: var(--radius);
  }
  .add-card-btn:hover { background: var(--bg-hover); color: var(--text-primary); }

  .add-card-form { display: flex; flex-direction: column; gap: 6px; }
  .card-textarea {
    width: 100%; padding: 8px; border: 1px solid var(--border);
    border-radius: var(--radius); background: var(--bg-primary);
    color: var(--text-primary); font-size: 13px; resize: none; box-sizing: border-box;
  }
  .add-card-actions { display: flex; gap: 6px; }

  /* ── Buttons ── */
  .btn-primary {
    padding: 6px 14px; background: var(--accent); color: #fff; border: none;
    border-radius: var(--radius); font-size: 13px; font-weight: 600; cursor: pointer;
  }
  .btn-primary:hover { filter: brightness(1.1); }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-outline {
    padding: 5px 12px; background: none; border: 1px solid var(--border);
    border-radius: var(--radius); color: var(--text-primary); font-size: 13px; cursor: pointer;
  }
  .btn-outline:hover { background: var(--bg-secondary); }
  .btn-ghost {
    padding: 5px 10px; background: none; border: none;
    color: var(--text-secondary); font-size: 13px; cursor: pointer; border-radius: var(--radius);
  }
  .btn-ghost:hover { background: var(--bg-secondary); color: var(--text-primary); }
  .btn-sm { padding: 4px 10px; font-size: 12px; }
  .btn-xs { padding: 3px 8px; font-size: 12px; }
</style>
