#!/usr/bin/env node

import { readFileSync } from 'node:fs';

import { rustFnBlock, rustStructBody, stripRustComments } from './lib/rust-source.mjs';

const files = {
  client: 'web/src/lib/api/boards.ts',
  boardsPage: 'web/src/routes/[owner]/[repo]/boards/+page.svelte',
  issueBoardPage: 'web/src/routes/[owner]/[repo]/issues/board/+page.svelte',
  backend: 'crates/rg-http/src/api/boards.rs',
  backendService: 'crates/rg-core/src/board/service.rs',
};

const source = Object.fromEntries(
  Object.entries(files).map(([key, file]) => [key, readFileSync(file, 'utf8')]),
);

// A status elsewhere in boards.rs must not stand in for the handler whose
// response contract is being asserted. Strip comments before extracting each
// function so a commented-out status cannot satisfy the check either.
const backendCode = stripRustComments(source.backend);
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
      /async function selectBoard\(board: Board\)[\s\S]*boards\.get\(owner, repo, board\.id\)/.test(source.boardsPage) &&
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
      /createCard\(owner, repo, activeBoard\.id, colId, \{\s*note: newCardTitle\.trim\(\),\s*\}\)/.test(source.boardsPage) &&
      /createCard\(owner, repo, activeBoardId!, colId, \{ note \}\)/.test(source.issueBoardPage),
  },
  {
    name: 'both board pages publish complete same-column card orders',
    ok:
      /publishBoardCardOrder\(\{[\s\S]*boards\.reorderCards\(owner, repo, activeBoard!\.id, \{ positions \}\)/.test(source.boardsPage) &&
      /publishBoardCardOrder\(\{[\s\S]*boards\.reorderCards\(owner, repo, activeBoardId!, \{ positions \}\)/.test(source.issueBoardPage),
  },
  {
    name: 'issue board routes same-column drops through the reorder path',
    ok:
      /if \(draggingFromColId === colId\) \{[\s\S]*await reorderCard\(colId, draggingCardId, position\)/.test(source.issueBoardPage) &&
      !/if \(draggingFromColId === colId\) \{\s*draggingCardId = null;\s*return;\s*\}/.test(source.issueBoardPage) &&
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
