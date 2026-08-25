#!/usr/bin/env node

// The UI inventory must describe the tree it is committed next to.
//
// `docs/ui-inventory.json` and `docs/UI_INVENTORY.md` name every control a
// person can act on, the route behind it and the `Access` the router holds that
// route to. That artefact is only worth anything while it is true, and the way
// an inventory stops being true is not malice — it is a page gaining a button
// and nobody remembering the generator exists. `docs/FEATURE_INVENTORY.md` is
// the worked example: it records "последняя сверка с кодом: 2026-07-23" and 962
// commits have landed since, 64 of them in the router.
//
// So the artefact is a ratchet, not a snapshot: this check rebuilds it from the
// sources and fails when the committed copy disagrees. Adding a button then
// costs one regenerate, and *not* regenerating costs a red gate rather than a
// document that quietly lies.
//
// Static: reads repository sources only, no server, so it belongs in the
// contract-check runner rather than the runtime regression harness.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { buildInventory, renderMarkdown } from './ui-inventory.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const JSON_ARTEFACT = join(root, 'docs/ui-inventory.json');
const MD_ARTEFACT = join(root, 'docs/UI_INVENTORY.md');
const REGENERATE = 'node scripts/ui-inventory.mjs';

// Floors, not targets. They exist so that a parser that has stopped
// understanding the sources fails loudly instead of agreeing with an equally
// empty committed artefact — the failure mode `loadRouteTable` guards against
// one layer down. Raise them when the real numbers move well past.
const MIN_ROUTES = 250;
const MIN_PAGES = 40;
const MIN_CONTROLS = 400;
const MIN_CONTROLS_WITH_CALLS = 120;

const failures = [];

let inventory;
try {
  inventory = buildInventory();
} catch (error) {
  console.error(`❌ UI inventory could not be derived: ${error?.message || String(error)}`);
  process.exit(1);
}

const controls = inventory.pages.flatMap((page) => page.controls);
const withCalls = controls.filter((control) => control.calls.length);

const floor = (actual, minimum, what) => {
  if (actual < minimum) {
    failures.push(
      `the inventory understood only ${actual} ${what} (floor ${minimum}). The source form probably changed — ` +
        'fix the parsers in scripts/lib/ui-surface.mjs rather than lowering this floor.',
    );
  }
};

floor(inventory.routes.length, MIN_ROUTES, 'router routes');
floor(inventory.pages.length, MIN_PAGES, 'pages');
floor(controls.length, MIN_CONTROLS, 'interactive controls');
floor(withCalls.length, MIN_CONTROLS_WITH_CALLS, 'controls resolved to an API call');

// Every route the browser reaches must resolve to a declared access level. A
// null here means the client calls a URL the router does not serve, or that the
// join stopped working — both are worth a red run.
const unresolved = withCalls
  .flatMap((control) => control.calls)
  .filter((call) => !call.matched && !call.opaque);
if (unresolved.length) {
  const sample = [...new Set(unresolved.map((call) => `${call.method} ${call.url}`))].slice(0, 5);
  failures.push(
    `${unresolved.length} UI call(s) resolve to no route in the router: ${sample.join(', ')}. ` +
      'Either the client calls a URL nothing serves, or the path join in scripts/lib/ui-surface.mjs drifted.',
  );
}

const compare = (artefactPath, expected, label) => {
  if (!existsSync(artefactPath)) {
    failures.push(`${label} is missing — generate it with \`${REGENERATE}\`.`);
    return;
  }
  if (readFileSync(artefactPath, 'utf8') !== expected) {
    failures.push(`${label} does not match the sources — regenerate it with \`${REGENERATE}\`.`);
  }
};

compare(JSON_ARTEFACT, `${JSON.stringify(inventory, null, 2)}\n`, 'docs/ui-inventory.json');
compare(MD_ARTEFACT, renderMarkdown(inventory), 'docs/UI_INVENTORY.md');

if (failures.length) {
  console.error('❌ UI inventory contract failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

const uiRoutes = inventory.routes.filter((route) => route.reachedFromUi);
const untested = uiRoutes.filter(
  (route) => !['web', 'smoke', 'browser'].some((suite) => route.testedIn.includes(suite)),
);
console.log(
  `✅ UI inventory in sync: ${inventory.routes.length} routes, ${inventory.pages.length} pages, ` +
    `${controls.length} controls (${withCalls.length} reaching an API); ` +
    `${uiRoutes.length} routes reachable from the browser, ${untested.length} of them with no frontend test.`,
);
