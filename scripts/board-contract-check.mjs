#!/usr/bin/env node

import { readFileSync } from 'node:fs';

import { productionRustSource, rustFnBlock, rustStructBody } from './lib/rust-source.mjs';
import { productionTsSource, tsFunctionBody } from './lib/ts-source.mjs';

const files = {
  client: 'web/src/lib/api/boards.ts',
  boardsPage: 'web/src/routes/[owner]/[repo]/boards/+page.svelte',
  issueBoardPage: 'web/src/routes/[owner]/[repo]/issues/board/+page.svelte',
  asyncStateOwnership: 'web/src/lib/asyncStateOwnership.ts',
  backend: 'crates/rg-http/src/api/boards.rs',
  backendService: 'crates/rg-core/src/board/service.rs',
  backendDb: 'crates/rg-db/src/ops/board_ops.rs',
};

// Each half of the map is normalized by the language it is written in: the
// `.rs` entries reach `productionRustSource` at the readers below, the `.ts`
// and `.svelte` ones are put into their production view here. A page asserted
// against raw bytes answers out of its own comments — see
// `scripts/lib/ts-source.mjs`.
const source = Object.fromEntries(
  Object.entries(files).map(([key, file]) => [
    key,
    file.endsWith('.rs') ? readFileSync(file, 'utf8') : productionTsSource(readFileSync(file, 'utf8')),
  ]),
);

// A status elsewhere in boards.rs must not stand in for the handler whose
// response contract is being asserted. Extract each function out of the
// production view, so neither a commented-out status nor one written inside a
// `#[cfg(test)]` double can satisfy the check.
const backendCode = productionRustSource(source.backend);
const boardDeleteChecks = ['delete_board', 'delete_column', 'delete_card'].map((handler) => {
  const fn = rustFnBlock(backendCode, handler);
  return {
    name: `backend ${handler} returns 204 No Content`,
    ok: fn !== null && /StatusCode::NO_CONTENT\.into_response\(\)/.test(fn.body),
  };
});

// Both halves of this one used to bridge out of the struct: the positive was
// satisfied by any later `pub note:` in boards.rs, and the negative — the more
// dangerous direction — asserted only that *no struct anywhere in the file* has
// a `pub title:`, so re-adding one to `CreateCardRequest` would have gone red
// for the wrong reason and dropping the guard entirely would not have been
// noticed. Read the body once, assert inside it (see `rustStructBody`).
const createCardRequest = rustStructBody(backendCode, 'CreateCardRequest');

// `service.rs` was the last file in this repository still asserted against as
// raw bytes: the two struct regexes below used to run over `source.backendService`
// straight from disk, so a `#[cfg(test)]` fixture or a commented-out field
// satisfied a claim about the response the server actually serialises. Read the
// two bodies through the struct reader, which anchors in the production view.
const cardFull = rustStructBody(source.backendService, 'CardFull');
const columnFull = rustStructBody(source.backendService, 'ColumnFull');
const reorderRequest = rustStructBody(backendCode, 'ReorderCardsRequest');
const reorderService = rustFnBlock(productionRustSource(source.backendService), 'reorder_cards');
const reorderDb = rustFnBlock(
  productionRustSource(source.backendDb),
  'update_card_position_if_still_in_column',
);
const boardMutationOwner = tsFunctionBody(source.boardsPage, 'runBoardMutation');
const issueBoardMutationOwner = tsFunctionBody(source.issueBoardPage, 'runBoardMutation');
const standaloneBoardControlsBusy =
  /let boardControlsBusy = \$derived\(boardMutationBusy \|\| boardSelectionBusy\)/.test(
    source.boardsPage,
  );
const issueBoardControlsBusy =
  /let boardControlsBusy = \$derived\(boardMutationBusy \|\| boardSelectionBusy\)/.test(
    source.issueBoardPage,
  );
const standaloneReorder = tsFunctionBody(source.boardsPage, 'reorderCard');
const issueReorder = tsFunctionBody(source.issueBoardPage, 'reorderCard');
const issueDrop = tsFunctionBody(source.issueBoardPage, 'onDrop');
const standaloneBoardLoad = tsFunctionBody(source.boardsPage, 'loadBoards');
const issueBoardLoad = tsFunctionBody(source.issueBoardPage, 'loadBoards');
const standaloneSelection = tsFunctionBody(source.boardsPage, 'selectBoard');
const issueBoardSelection = tsFunctionBody(source.issueBoardPage, 'loadBoard');
const standaloneRefresh = tsFunctionBody(source.boardsPage, 'refreshBoard');
const standaloneRouteMutations = [
  'createBoard',
  'deleteBoard',
  'saveBoard',
  'addColumn',
  'deleteColumn',
  'saveColumn',
  'addCard',
  'deleteCard',
  'saveCard',
  'moveCard',
  'reorderCard',
]
  .map((name) => ({ name, body: tsFunctionBody(source.boardsPage, name) }));
const standaloneCardMutations = ['addCard', 'deleteCard', 'saveCard', 'moveCard', 'reorderCard']
  .map((name) => ({ name, body: tsFunctionBody(source.boardsPage, name) }));
const issueBoardMutations = [
  'handleCreateBoard',
  'handleAddColumn',
  'handleDeleteColumn',
  'handleAddCard',
  'handleDeleteCard',
  'onDrop',
]
  .map((name) => ({ name, body: tsFunctionBody(source.issueBoardPage, name) }));

const checks = [
  {
    name: 'backend boards.rs still defines a readable struct CreateCardRequest',
    ok: createCardRequest !== null,
  },
  {
    name: 'backend create-card request accepts note, not title',
    ok:
      createCardRequest !== null &&
      /\bpub note: Option<String>/.test(createCardRequest) &&
      !/\bpub title:/.test(createCardRequest),
  },
  {
    name: 'API client createCard payload does not expose title',
    ok:
      /createCard:[\s\S]*data: \{ note\?: string; issue_id\?: number \}/.test(source.client) &&
      !/createCard:[\s\S]*data: \{ title:/.test(source.client),
  },
  {
    name: 'API client moveCard requires position',
    ok: /moveCard:[\s\S]*data: \{ column_id: number; position: number \}/.test(source.client),
  },
  {
    name: 'reorder contract carries the source column from both board pages to the backend',
    ok:
      /reorderCards:[\s\S]*data: \{ column_id: number; positions: \[number, number\]\[\] \}/.test(source.client) &&
      reorderRequest !== null &&
      /pub column_id: Option<i64>/.test(reorderRequest) &&
      standaloneReorder !== null &&
      /boards\.reorderCards\(route\.owner, route\.repo, boardId, \{[\s\S]*column_id: column\.id,[\s\S]*positions,/.test(standaloneReorder) &&
      issueReorder !== null &&
      /boards\.reorderCards\(route\.owner, route\.repo, boardId, \{[\s\S]*column_id: colId,[\s\S]*positions,/.test(issueReorder),
  },
  {
    name: 'API client board deletes model backend 204 responses as void',
    ok:
      /delete:\s*\(owner: string, repo: string, id: number\)\s*=>\s*\n?\s*request<void>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/boards\/\$\{id\}`,\s*\{ method: 'DELETE' \}\)/.test(source.client) &&
      /deleteColumn:\s*\(owner: string, repo: string, boardId: number, colId: number\)\s*=>\s*\n?\s*request<void>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/boards\/\$\{boardId\}\/columns\/\$\{colId\}`,\s*\{ method: 'DELETE' \}\)/.test(source.client) &&
      /deleteCard:\s*\(owner: string, repo: string, boardId: number, cardId: number\)\s*=>\s*\n?\s*request<void>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/boards\/\$\{boardId\}\/cards\/\$\{cardId\}`,\s*\{ method: 'DELETE' \}\)/.test(source.client) &&
      !/request<\{\s*deleted:\s*boolean\s*\}>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/boards\//.test(source.client),
  },
  ...boardDeleteChecks,
  {
    name: 'standalone board page fetches full board before rendering columns',
    ok:
      standaloneSelection !== null &&
      /boards\.get\(expectedOwner, expectedRepo, board\.id\)/.test(standaloneSelection) &&
      /function normalizeColumns\(board: BoardFullResponse\)/.test(source.boardsPage),
  },
  {
    name: 'standalone board page renders card note instead of absent title',
    ok: /card\.note \|\| card\.issue\?\.title/.test(source.boardsPage) && !/<span>\{card\.title\}<\/span>/.test(source.boardsPage),
  },
  {
    name: 'backend board response enriches cards with issue metadata',
    ok:
      cardFull !== null &&
      columnFull !== null &&
      /#\[serde\(flatten\)\][\s\S]*?pub card: Card,/.test(cardFull) &&
      /pub issue: Option<crate::issue::IssueWithLabels>/.test(cardFull) &&
      /pub cards: Vec<CardFull>/.test(columnFull),
  },
  {
    name: 'issue board links cards by issue number, not database issue_id',
    ok:
      /href=\{`\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{card\.issue\.number\}`\}/.test(source.issueBoardPage) &&
      !/href=\{`\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{card\.issue_id\}`\}/.test(source.issueBoardPage),
  },
  {
    name: 'board card creation pages send note payloads',
    ok:
      /createCard\(route\.owner, route\.repo, boardId, colId, \{\s*note,\s*\}\)/.test(source.boardsPage) &&
      /createCard\(route\.owner, route\.repo, boardId, colId, \{ note \}\)/.test(source.issueBoardPage),
  },
  {
    name: 'both board pages publish complete same-column card orders',
    ok:
      standaloneReorder !== null &&
      /publishBoardCardOrder\(\{[\s\S]*boards\.reorderCards\(route\.owner, route\.repo, boardId, \{[\s\S]*column_id: column\.id,[\s\S]*positions,/.test(standaloneReorder) &&
      issueReorder !== null &&
      /publishBoardCardOrder\(\{[\s\S]*boards\.reorderCards\(route\.owner, route\.repo, boardId, \{[\s\S]*column_id: colId,[\s\S]*positions,/.test(issueReorder),
  },
  {
    name: 'standalone board owns parent load and mutations by repository-route generation',
    ok:
      /let routeGeneration = 0/.test(source.boardsPage) &&
      /\$effect\(\(\) => \{[\s\S]*routeGeneration \+= 1;[\s\S]*boardMutationBusy = false;[\s\S]*void loadBoards\(expectedOwner, expectedRepo, routeGeneration\)/.test(source.boardsPage) &&
      standaloneBoardLoad !== null &&
      /boardListRequests\.begin\(expectedOwner, expectedRepo\)/.test(standaloneBoardLoad) &&
      /boards\.list\(expectedOwner, expectedRepo\)/.test(standaloneBoardLoad) &&
      /issues\.list\(expectedOwner, expectedRepo/.test(standaloneBoardLoad) &&
      /isCurrentRoute\(route\)/.test(standaloneBoardLoad) &&
      boardMutationOwner !== null &&
      /const route = currentRoute\(\)/.test(boardMutationOwner) &&
      /await operation\(route\)/.test(boardMutationOwner) &&
      /catch[\s\S]*if \(isCurrentRoute\(route\)\) error/.test(boardMutationOwner) &&
      /finally[\s\S]*if \(isCurrentRoute\(route\)\) boardMutationBusy = false/.test(boardMutationOwner) &&
      standaloneRouteMutations.every(({ body }) =>
        body !== null &&
        /runBoardMutation\(async \(route\)/.test(body) &&
        /route\.owner/.test(body) &&
        /route\.repo/.test(body) &&
        /isCurrentRoute\(route\)/.test(body)
      ) &&
      standaloneRefresh !== null &&
      /boards\.get\(route\.owner, route\.repo, boardId\)/.test(standaloneRefresh) &&
      /isCurrentRoute\(route\)/.test(standaloneRefresh),
  },
  {
    name: 'issue board owns parent load and mutations by repository-route generation',
    ok:
      /let routeGeneration = 0/.test(source.issueBoardPage) &&
      /\$effect\(\(\) => \{[\s\S]*routeGeneration \+= 1;[\s\S]*boardMutationBusy = false;[\s\S]*void loadBoards\(expectedOwner, expectedRepo, routeGeneration\)/.test(source.issueBoardPage) &&
      issueBoardLoad !== null &&
      /boardListRequests\.begin\(expectedOwner, expectedRepo\)/.test(issueBoardLoad) &&
      /boards\.list\(expectedOwner, expectedRepo\)/.test(issueBoardLoad) &&
      /isCurrentRoute\(route\)/.test(issueBoardLoad) &&
      issueBoardMutationOwner !== null &&
      /const route = currentRoute\(\)/.test(issueBoardMutationOwner) &&
      /await operation\(route\)/.test(issueBoardMutationOwner) &&
      /catch[\s\S]*if \(isCurrentRoute\(route\)\) error/.test(issueBoardMutationOwner) &&
      /finally[\s\S]*if \(isCurrentRoute\(route\)\) boardMutationBusy = false/.test(issueBoardMutationOwner) &&
      issueBoardMutations.every(({ body }) =>
        body !== null &&
        /runBoardMutation\(async \(route\)/.test(body) &&
        /route\.owner/.test(body) &&
        /route\.repo/.test(body) &&
        /isCurrentRoute\(route\)/.test(body)
      ) &&
      issueReorder !== null &&
      /async function reorderCard\(\s*route: BoardRoute,/.test(source.issueBoardPage) &&
      /if \(isCurrentRoute\(route\)\) setColumnCards\(colId, cards\)/.test(issueReorder) &&
      /reload: \(\) => loadBoard\(boardId, route\)/.test(issueReorder),
  },
  {
    name: 'both board pages use one fail-closed owner for conflicting mutations',
    ok:
      standaloneBoardControlsBusy &&
      boardMutationOwner !== null &&
      /if \(boardControlsBusy\) return false/.test(boardMutationOwner) &&
      /boardMutationBusy = true/.test(boardMutationOwner) &&
      /finally[\s\S]*boardMutationBusy = false/.test(boardMutationOwner) &&
      issueBoardControlsBusy &&
      issueBoardMutationOwner !== null &&
      /if \(boardControlsBusy\) return false/.test(issueBoardMutationOwner) &&
      standaloneCardMutations.every(({ body }) => body !== null && /runBoardMutation\(/.test(body)) &&
      issueBoardMutations.every(({ body }) => body !== null && /runBoardMutation\(/.test(body)),
  },
  {
    name: 'both board pages fence detail publication by repository and board identity',
    ok:
      /class LatestRepositoryResourceRequestFence/.test(source.asyncStateOwnership) &&
      /claim\.resource === resource/.test(source.asyncStateOwnership) &&
      standaloneSelection !== null &&
      /async function selectBoard\(board: Board, route = currentRoute\(\)\)/.test(source.boardsPage) &&
      /boardSelectionRequests\.begin\(expectedOwner, expectedRepo, board\.id\)/.test(standaloneSelection) &&
      /activeBoard = null;[\s\S]*await boards\.get\(expectedOwner, expectedRepo, board\.id\)/.test(standaloneSelection) &&
      /isCurrentRoute\(route\)/.test(standaloneSelection) &&
      /boardSelectionRequests\.owns\(claim, owner, repo, board\.id\)/.test(standaloneSelection) &&
      issueBoardSelection !== null &&
      /async function loadBoard\(id: number, route = currentRoute\(\)\)/.test(source.issueBoardPage) &&
      /boardSelectionRequests\.begin\(expectedOwner, expectedRepo, id\)/.test(issueBoardSelection) &&
      /activeBoard = null;[\s\S]*await boards\.get\(expectedOwner, expectedRepo, id\)/.test(issueBoardSelection) &&
      /isCurrentRoute\(route\)/.test(issueBoardSelection) &&
      /boardSelectionRequests\.owns\(claim, owner, repo, id\)/.test(issueBoardSelection),
  },
  {
    name: 'both board mutation owners reject controls during an unconfirmed selection',
    ok:
      standaloneBoardControlsBusy &&
      boardMutationOwner !== null &&
      /if \(boardControlsBusy\) return false/.test(boardMutationOwner) &&
      issueBoardControlsBusy &&
      issueBoardMutationOwner !== null &&
      /if \(boardControlsBusy\) return false/.test(issueBoardMutationOwner),
  },
  {
    name: 'backend rejects a reorder that loses to a card move instead of overwriting it',
    ok:
      reorderDb !== null &&
      /Column::ColumnId\.eq\(column_id\)/.test(reorderDb.body) &&
      /rows_affected == 1/.test(reorderDb.body) &&
      reorderService !== null &&
      /ReorderOutcome::Moved[\s\S]*crate::error::conflict/.test(reorderService.body),
  },
  {
    name: 'issue board routes same-column drops through the reorder path',
    ok:
      issueDrop !== null &&
      /if \(fromColId === colId\) \{[\s\S]*await reorderCard\(route, boardId, colId, cardId, position\)/.test(issueDrop) &&
      !/if \(fromColId === colId\) \{\s*draggingCardId = null;\s*return;\s*\}/.test(issueDrop) &&
      /draggingFromColId === column\.id \? cardIndex : undefined/.test(source.issueBoardPage),
  },
];

let failed = 0;
for (const check of checks) {
  if (check.ok) {
    console.log(`✅ ${check.name}`);
  } else {
    console.log(`❌ ${check.name}`);
    failed += 1;
  }
}

if (failed > 0) {
  console.error(`\nBoard contract check failed: ${failed} issue(s)`);
  process.exit(1);
}

console.log('\nBoard contract check passed');
