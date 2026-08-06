#!/usr/bin/env node

import { readFileSync } from 'node:fs';

import { rustFnBlock, stripRustComments } from './lib/rust-source.mjs';

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

const checks = [
  {
    name: 'backend create-card request accepts note, not title',
    ok:
      /pub struct CreateCardRequest[\s\S]*pub note: Option<String>/.test(source.backend) &&
      !/pub struct CreateCardRequest[\s\S]*pub title:/.test(source.backend),
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
      /async function selectBoard\(board: any\)[\s\S]*boards\.get\(owner, repo, board\.id\)/.test(source.boardsPage) &&
      /function normalizeColumns\(board: any\)/.test(source.boardsPage),
  },
  {
    name: 'standalone board page renders card note instead of absent title',
    ok: /card\.note \|\| card\.issue\?\.title/.test(source.boardsPage) && !/<span>\{card\.title\}<\/span>/.test(source.boardsPage),
  },
  {
    name: 'backend board response enriches cards with issue metadata',
    ok:
      /pub struct CardFull\s*\{[^}]*#\[serde\(flatten\)\][^}]*pub card: Card,[^}]*pub issue: Option<crate::issue::IssueWithLabels>/.test(source.backendService) &&
      /pub struct ColumnFull\s*\{[^}]*pub cards: Vec<CardFull>/.test(source.backendService),
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
