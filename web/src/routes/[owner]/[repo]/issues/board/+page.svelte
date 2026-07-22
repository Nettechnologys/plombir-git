<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { boards } from '$lib/api/client.svelte';

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let boardList = $state<any[]>([]);
  let activeBoardId = $state<number | null>(null);
  let activeBoard = $state<any | null>(null);
  let loading = $state(true);
  let error = $state('');

  // Board creation
  let showCreateBoard = $state(false);
  let newBoardName = $state('');
  let creatingBoard = $state(false);

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

  $effect(() => { loadBoards(); });

  async function loadBoards() {
    loading = true;
    error = '';
    try {
      boardList = await boards.list(owner, repo);
      if (boardList.length > 0) {
        activeBoardId = activeBoardId ?? boardList[0].id;
        await loadBoard(activeBoardId!);
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadBoard(id: number) {
    const board = await boards.get(owner, repo, id);
    activeBoard = board;
  }

  async function handleCreateBoard() {
    if (!newBoardName.trim()) return;
    creatingBoard = true;
    try {
      const board = await boards.create(owner, repo, { name: newBoardName.trim() });
      newBoardName = '';
      showCreateBoard = false;
      activeBoardId = board.id;
      await loadBoards();
    } catch (e: any) {
      error = e.message;
    } finally {
      creatingBoard = false;
    }
  }

  async function handleAddColumn() {
    if (!newColumnName.trim()) return;
    try {
      await boards.createColumn(owner, repo, activeBoardId!, { name: newColumnName.trim() });
      newColumnName = '';
      showAddColumn = false;
      await loadBoard(activeBoardId!);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleDeleteColumn(colId: number) {
    if (!confirm('Delete this column and all its cards?')) return;
    try {
      await boards.deleteColumn(owner, repo, activeBoardId!, colId);
      await loadBoard(activeBoardId!);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleAddCard(colId: number) {
    const note = (newCardNote[colId] ?? '').trim();
    if (!note) return;
    try {
      await boards.createCard(owner, repo, activeBoardId!, colId, { note });
      newCardNote = { ...newCardNote, [colId]: '' };
      showAddCard = { ...showAddCard, [colId]: false };
      await loadBoard(activeBoardId!);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleDeleteCard(cardId: number) {
    try {
      await boards.deleteCard(owner, repo, activeBoardId!, cardId);
      await loadBoard(activeBoardId!);
    } catch (e: any) {
      error = e.message;
    }
  }

  // ── Drag & Drop ──────────────────────────────────
  function onDragStart(e: DragEvent, cardId: number, colId: number) {
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

  async function onDrop(e: DragEvent, colId: number) {
    e.preventDefault();
    dragOverColId = null;
    if (draggingCardId === null || draggingFromColId === null) return;
    if (draggingFromColId === colId) { draggingCardId = null; return; }

    const targetCol = activeBoard?.columns?.find((c: any) => c.column.id === colId);
    const position = targetCol ? targetCol.cards.length : 0;

    try {
      await boards.moveCard(owner, repo, activeBoardId!, draggingCardId, { column_id: colId, position });
      await loadBoard(activeBoardId!);
    } catch (e: any) {
      error = e.message;
    } finally {
      draggingCardId = null;
      draggingFromColId = null;
    }
  }
</script>

<svelte:head>
  <title>Board · {owner}/{repo} · IronForge</title>
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
      <button class="btn-primary" onclick={() => showCreateBoard = true}>Create Board</button>
    </div>
  {:else}
    <!-- Board selector + controls -->
    <div class="board-toolbar">
      <div class="board-tabs">
        {#each boardList as b}
          <button
            class="board-tab"
            class:active={b.id === activeBoardId}
            onclick={async () => { activeBoardId = b.id; await loadBoard(b.id); }}
          >{b.name}</button>
        {/each}
        <button class="btn-ghost btn-sm" onclick={() => showCreateBoard = !showCreateBoard}>+ Board</button>
      </div>
      <button class="btn-outline btn-sm" onclick={() => showAddColumn = !showAddColumn}>+ Column</button>
    </div>

    {#if showCreateBoard}
      <div class="inline-form">
        <input
          class="form-input"
          placeholder="Board name"
          bind:value={newBoardName}
          onkeydown={(e) => e.key === 'Enter' && handleCreateBoard()}
        />
        <button class="btn-primary btn-sm" onclick={handleCreateBoard} disabled={creatingBoard}>
          {creatingBoard ? '…' : 'Create'}
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
          onkeydown={(e) => e.key === 'Enter' && handleAddColumn()}
        />
        <button class="btn-primary btn-sm" onclick={handleAddColumn}>Add</button>
        <button class="btn-ghost btn-sm" onclick={() => { showAddColumn = false; newColumnName = ''; }}>Cancel</button>
      </div>
    {/if}

    <!-- Board columns -->
    {#if activeBoard?.columns}
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
                <button class="btn-ghost btn-xs" onclick={() => handleDeleteColumn(column.id)} title="Delete column">✕</button>
              </div>
            </div>

            <div class="column-body" role="listitem">
              {#each cards as card (card.id)}
                <div
                  class="card"
                  class:dragging={draggingCardId === card.id}
                  draggable="true"
                  ondragstart={(e) => onDragStart(e, card.id, column.id)}
                  role="button"
                  tabindex="0"
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
                    onkeydown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleAddCard(column.id); } }}
                  ></textarea>
                  <div class="add-card-actions">
                    <button class="btn-primary btn-xs" onclick={() => handleAddCard(column.id)}>Add</button>
                    <button class="btn-ghost btn-xs" onclick={() => showAddCard = { ...showAddCard, [column.id]: false }}>Cancel</button>
                  </div>
                </div>
              {:else}
                <button class="add-card-btn" onclick={() => showAddCard = { ...showAddCard, [column.id]: true }}>
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
