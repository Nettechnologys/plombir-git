#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource, requireBlock } from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/pulls/[number]/+page.svelte');
const clientPath = path.join(root, 'web/src/lib/api/pulls.ts');
const backendPath = path.join(root, 'crates/rg-core/src/review/service.rs');
const i18nPath = path.join(root, 'web/src/lib/i18n/translations/en.json');

const page = productionTsSource(readFileSync(pagePath, 'utf8'));
const client = productionTsSource(readFileSync(clientPath, 'utf8'));
const failures = [];
// Every assertion below reads the executable Rust, not the file's bytes: the
// action names are string literals, so a commented-out match arm satisfied the
// raw greps exactly like a live one — the parser stops accepting `approve`,
// the timeline label set shrinks, and this gate stayed green (card_64b6ede78939).
const backend = productionRustSource(readFileSync(backendPath, 'utf8'));

if (!/"approve"\s*=>\s*Ok\(Self::Approve\)/.test(backend)) {
  failures.push('Backend review action parser must accept the canonical approve action.');
}

if (/value="approved"/.test(page)) {
  failures.push('PR review approve radio must submit backend action "approve", not display label "approved".');
}

if (!/value="approve"/.test(page)) {
  failures.push('PR review approve radio must use value="approve".');
}

if (!/body:\s*JSON\.stringify\(\{\s*body,\s*action:\s*verdict\s*\}\)/.test(client)) {
  failures.push('API client must send PR review verdict as the backend action field.');
}

// The dedicated review list was folded into the unified PR timeline: review
// activity now renders from the backend-derived event kind (`review_<action>`),
// never a client-side verdict field. Assert the page keeps rendering it that way.
if (!/t\(`pulls\.timeline\.\$\{event\.kind\}`/.test(page)) {
  failures.push('PR timeline must render review activity from the backend event kind (review_<action>), not a client-side verdict.');
}

if (/class:approved=\{review\.verdict/.test(page) || /pulls\.verdict\.\$\{review\.verdict/.test(page)) {
  failures.push('PR review activity must not render directly from the absent backend verdict field.');
}

// Every backend ReviewAction variant surfaces on the timeline as `review_<action>`
// (service.rs records `format!("review_{}", review.action)`). Tie the timeline
// labels to the enum so a new/renamed action can't silently lose its rendering.
const asStrBody = requireBlock(
  backend,
  /pub fn as_str\(&self\)[\s\S]*?\n\s*\}/,
  'Could not extract backend ReviewAction variants from service.rs as_str().',
  failures,
);
const actions = asStrBody
  ? [...asStrBody.matchAll(/Self::\w+\s*=>\s*"([a-z_]+)"/g)].map((m) => m[1])
  : [];
if (actions.length === 0) {
  if (asStrBody) {
    failures.push('Could not extract backend ReviewAction variants from service.rs as_str().');
  }
} else {
  let timelineLabels = {};
  try {
    timelineLabels = JSON.parse(readFileSync(i18nPath, 'utf8'))?.pulls?.timeline ?? {};
  } catch (err) {
    failures.push(`Could not parse timeline labels from en.json: ${err.message}`);
  }
  for (const action of actions) {
    if (!Object.prototype.hasOwnProperty.call(timelineLabels, `review_${action}`)) {
      failures.push(`Timeline label "pulls.timeline.review_${action}" is missing for backend review action "${action}".`);
    }
  }
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('PR review frontend/backend contract ok');
