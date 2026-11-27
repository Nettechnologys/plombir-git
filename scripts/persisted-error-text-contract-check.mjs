#!/usr/bin/env node

// Every column that stores a failure's text and shows it again later is
// accounted for, with the decision about what that text may say.
//
// H-05 holds at the `AppError` funnel: a 5xx body is replaced, and the typed
// `NotFound` / `InvalidRequest` / `Conflict` / `Forbidden` document that their
// `message` reaches the client verbatim. A column is a route *past* that
// funnel. The server writes a reason into a row, and a page renders the row
// days later — the pull request timeline, the import page, the mirror
// settings. Whatever `format!("{error:#}")` put there arrives whole: server
// paths, git command lines, a subprocess's stderr, `db: …` chains. Three such
// fields were found one sweep at a time (ee9360a, f2209ce, 0ee3e12), and the
// fourth would have been found the same way (card_c456fc9fc9e8).
//
// WHAT THIS CHECK ASKS. Not "does every producer of this column sanitise" —
// that crosses functions and crates, and a gate that needs data-flow analysis
// is a gate that gets deleted the first time it misreads a refactor. It asks
// what has an exact answer:
//
//   1. every entity text column whose name says it holds a failure or a reason
//      (`error`, `fail`, `reason`, `stderr`, `stdout`, `diagnos`) is in
//      PERSISTED_TEXT below, with a decision and the reason for it;
//   2. every entry names a text column that exists — an entry pointing at
//      nothing is how a registry stops being one;
//   3. the columns that carry failure text under another name are listed by
//      hand (`also_named: true`), and so are kept honest by rule 2.
//
// A new column of that kind therefore cannot land without somebody writing
// down what it may say and who sees it. The decision is then the reviewer's to
// check against the producer — which is the part a person does better.

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { loadProductionRust } from './lib/rust-consumer-contract.mjs';
import { productionRustSource } from './lib/rust-source.mjs';

const root = process.cwd();
const cratesDir = path.join(root, 'crates');
const ENTITIES = 'crates/rg-db/src/entities/';

// Floors, not counts: below them the parse has stopped reading the schema and
// every verdict is uninformed rather than clean.
const MIN_ENTITIES = 80;
const MIN_NAMED_COLUMNS = 6;

const NAME = /(?:error|fail|reason|stderr|stdout|diagnos)/i;

/**
 * What each decision means:
 *
 * - `fixed_text`            — only literals or a closed vocabulary of codes are
 *                              written; nothing of an error's own text.
 * - `client_facing_message` — fixed text, or the `message` of a typed error as
 *                              `rg_core::error::client_facing_message` selects
 *                              it; the full chain goes to the log only.
 * - `owner_detail`          — the failure's own text, deliberately, to the
 *                              resource's administrator only; credentials masked.
 *                              Needs `audience`.
 * - `not_failure_text`      — the name matches, the content is not a failure
 *                              (a subscription reason, say).
 */
const DECISIONS = new Set(['fixed_text', 'client_facing_message', 'owner_detail', 'not_failure_text']);

const PERSISTED_TEXT = [
  {
    column: 'import_tasks.error',
    decision: 'client_facing_message',
    why: 'import::service::failure_reason writes client_facing_message (source credentials masked) or a fixed "the import failed" line; the chain is logged with the task id (ee9360a)',
  },
  {
    column: 'merge_queue_entries.failure_reason',
    decision: 'client_facing_message',
    why: 'merge_queue::merge_failure_reason is client_facing_message; the queue catches merge_pr past the AppError funnel (card_ff19d8130b49)',
  },
  {
    column: 'pr_events.body',
    also_named: true,
    decision: 'client_facing_message',
    why: 'the merge queue writes its failure reason into the pull request timeline as this body — the same text as merge_queue_entries.failure_reason; other events carry user-written text (card_3b467917ce10)',
  },
  {
    column: 'login_logs.failure_reason',
    decision: 'fixed_text',
    why: 'auth::lockout and api::mfa pass literal codes (`invalid_password`, `mfa_required`, `invalid_mfa_code`, …); read by instance administrators in the audit view',
  },
  {
    column: 'notification_deliveries.last_error',
    decision: 'fixed_text',
    why: 'delivery outcome codes only (`coalesced`, `delivery_expired`, `channel_disabled`, `telegram_not_configured`, …); a transport error is logged, never stored',
  },
  {
    column: 'mirrors.last_sync_error',
    decision: 'owner_detail',
    audience: 'repository administrators, in the mirror settings',
    why: 'the git failure is what the administrator needs to repair the mirror; mask_credential runs before the write and the credential never enters argv or the URL (card_a997f30c142c)',
  },
  {
    column: 'push_mirrors.last_sync_error',
    decision: 'owner_detail',
    audience: 'repository administrators, in the push-mirror settings',
    why: 'same reasoning and the same mask_credential as pull mirrors (mirror::push)',
  },
  {
    column: 'webhook_deliveries.response_body',
    also_named: true,
    decision: 'owner_detail',
    audience: 'repository administrators, in the hook\'s own delivery list',
    why: 'a failed delivery stores `delivery error: {e:#}` so the administrator can see why their endpoint refused; delivery_transport_error strips the URL and mask_url_credentials is the last net; a successful delivery stores nothing',
  },
  {
    column: 'notifications.reason',
    decision: 'not_failure_text',
    why: 'why the person was notified (mention, review request, subscription) — a closed vocabulary',
  },
  {
    column: 'thread_subscriptions.reason',
    decision: 'not_failure_text',
    why: 'why the person follows the thread — the same closed vocabulary as notifications.reason',
  },
];

const failures = [];

const production = loadProductionRust(cratesDir);
const textColumns = new Set();
const named = new Set();
let entities = 0;
for (const file of production.keys()) {
  const name = path.relative(root, file).split(path.sep).join('/');
  if (!name.startsWith(ENTITIES)) continue;
  // The view that keeps string literals (the table name is one) while still
  // removing comments and test-only items.
  const source = productionRustSource(readFileSync(file, 'utf8'));
  if (!/\bpub\s+struct\s+Model\b/.test(source)) continue;
  const table = source.match(/\btable_name\s*=\s*"([^"]+)"/);
  const model = source.match(/\bpub\s+struct\s+Model\s*\{([\s\S]*?)\n\}/);
  if (!table || !model) {
    failures.push(`${name}: cannot parse the entity table/Model; persisted-text coverage is unknown`);
    continue;
  }
  entities += 1;
  for (const field of model[1].matchAll(/\bpub\s+(\w+)\s*:\s*([^,\n]+)/g)) {
    if (!/\bString\b/.test(field[2])) continue;
    const label = `${table[1]}.${field[1]}`;
    textColumns.add(label);
    if (NAME.test(field[1])) named.add(label);
  }
}

if (entities < MIN_ENTITIES) {
  failures.push(
    `Only ${entities} entity Model(s) parsed under ${ENTITIES}, and at least ${MIN_ENTITIES} ` +
      'are expected. The schema walk is reading nothing; fix the parsing, not the code.',
  );
}
if (named.size < MIN_NAMED_COLUMNS) {
  failures.push(
    `Only ${named.size} failure-text column(s) found by name, and at least ${MIN_NAMED_COLUMNS} ` +
      'are expected. The name rule or the field parse has stopped matching; fix it before ' +
      'trusting a clean result.',
  );
}

const registered = new Map();
for (const entry of PERSISTED_TEXT) {
  if (registered.has(entry.column)) {
    failures.push(`PERSISTED_TEXT lists \`${entry.column}\` twice`);
  }
  registered.set(entry.column, entry);
  if (!DECISIONS.has(entry.decision)) {
    failures.push(
      `\`${entry.column}\` has decision \`${entry.decision}\`, which is none of ` +
        `${[...DECISIONS].join(', ')}`,
    );
  }
  if (entry.decision === 'owner_detail' && !entry.audience) {
    failures.push(
      `\`${entry.column}\` shows a failure's own text and names no audience. Say who reads it ` +
        '— the owner-only exception is only an exception while it says to whom.',
    );
  }
  if (!entry.why) failures.push(`\`${entry.column}\` carries no reason for its decision`);
  if (!textColumns.has(entry.column)) {
    failures.push(
      `PERSISTED_TEXT names \`${entry.column}\`, which is not a text column of any entity. ` +
        'Drop the entry, or follow the column to wherever it moved.',
    );
  } else if (!entry.also_named && !named.has(entry.column)) {
    failures.push(
      `\`${entry.column}\` is listed as found by its name, and its name no longer says ` +
        'failure. Mark it `also_named: true` if it still carries failure text.',
    );
  }
}

for (const label of [...named].sort()) {
  if (!registered.has(label)) {
    failures.push(
      `Entity column \`${label}\` looks like it stores a failure's text, and PERSISTED_TEXT ` +
        'does not say what it may hold. A stored reason is rendered later, past the AppError ' +
        'funnel that keeps paths, git command lines and `db: …` chains away from clients. ' +
        'Write `rg_core::error::client_facing_message(&error)` (or a fixed code) into it, keep ' +
        'the full chain in `tracing`, and add an entry here with the decision and the reason.',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ Persisted failure text:');
  for (const failure of failures) console.error(`   - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ Persisted failure text: ${registered.size} column(s) registered with a decision ` +
    `across ${entities} entities; every failure-text column found by name is accounted for.`,
);
