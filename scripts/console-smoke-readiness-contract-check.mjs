#!/usr/bin/env node

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  consoleSmokeTimeouts,
  createPageReadinessWaiters,
} from './lib/browser-smoke-timing.mjs';
import { jsCodeView } from './lib/js-source.mjs';
import { tsFunctionBody } from './lib/ts-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const smokeSource = readFileSync(join(scriptsDir, 'console-smoke.mjs'), 'utf8');
const smokeCode = jsCodeView(smokeSource);
const createSessionBody = tsFunctionBody(smokeSource, 'createSession');
const checkRouteBody = tsFunctionBody(smokeSource, 'checkRoute');
assert.notEqual(createSessionBody, null, 'console-smoke.mjs must retain createSession()');
assert.notEqual(checkRouteBody, null, 'console-smoke.mjs must retain checkRoute()');

assert.match(smokeCode, /consoleSmokeTimeouts\s*\(\s*\)/);
assert.match(createSessionBody, /await send\('Page\.enable'\)/);
assert.match(
  createSessionBody,
  /await send\('Page\.setLifecycleEventsEnabled',\s*\{ enabled: true \}\)/,
);
assert.match(checkRouteBody, /openTab\('about:blank'\)/);
assert.match(checkRouteBody, /tab\.waitForLoad\(`route \$\{path\}`\)/);
assert.match(checkRouteBody, /tab\.send\('Page\.navigate',\s*\{ url: BASE \+ path \}\)/);
assert.match(checkRouteBody, /await Promise\.all\(\[loading\.promise, navigation\]\)/);
assert.doesNotMatch(
  smokeCode,
  /await sleep\(WAIT_MS\)/,
  'console-smoke must not accept a route after a blind fixed delay',
);

assert.deepEqual(consoleSmokeTimeouts({}), {
  cdpCommandMs: 10_000,
  pageLoadMs: 20_000,
});
assert.deepEqual(consoleSmokeTimeouts({ WAIT_MS: '1234' }), {
  cdpCommandMs: 10_000,
  pageLoadMs: 1234,
});
assert.deepEqual(consoleSmokeTimeouts({
  WAIT_MS: '1234',
  CDP_COMMAND_TIMEOUT_MS: '1500',
  PAGE_LOAD_TIMEOUT_MS: '2500',
}), {
  cdpCommandMs: 1500,
  pageLoadMs: 2500,
});
assert.throws(
  () => consoleSmokeTimeouts({ PAGE_LOAD_TIMEOUT_MS: '0' }),
  /PAGE_LOAD_TIMEOUT_MS must be a positive integer/,
);

const loaded = createPageReadinessWaiters({ timeoutMs: 100 });
const loadedRoute = loaded.wait('route /loaded');
// A fast document can complete before Page.navigate's response is dispatched.
// Keep the ready fact for that loader even if a later lifecycle event arrives.
loaded.observe({ frameId: 'frame-1', loaderId: 'loader-1', name: 'init' });
loaded.observe({ frameId: 'frame-1', loaderId: 'loader-1', name: 'DOMContentLoaded' });
loaded.observe({ frameId: 'frame-1', loaderId: 'loader-1', name: 'load' });
loaded.observe({ frameId: 'frame-1', loaderId: 'loader-1', name: 'networkIdle' });
loadedRoute.followNavigation({ frameId: 'frame-1', loaderId: 'loader-1' });
await loadedRoute.promise;

// A document fixture that starts and reaches DOMContentLoaded but never emits
// the load lifecycle event must be a deterministic red, even with no console or
// network error to provide an earlier failure.
const unfinished = createPageReadinessWaiters({ timeoutMs: 5 });
const unfinishedRoute = unfinished.wait('route /unfinished');
unfinishedRoute.followNavigation({ frameId: 'frame-2', loaderId: 'loader-2' });
unfinished.observe({ frameId: 'frame-2', loaderId: 'loader-2', name: 'init' });
unfinished.observe({ frameId: 'frame-2', loaderId: 'loader-2', name: 'DOMContentLoaded' });
await assert.rejects(
  unfinishedRoute.promise,
  /page readiness timed out after 5 ms while waiting for route \/unfinished; last state="dom-content-loaded"/,
);

const disconnected = createPageReadinessWaiters({ timeoutMs: 100 });
const disconnectedRoute = disconnected.wait('route /disconnected');
disconnectedRoute.followNavigation({ frameId: 'frame-3', loaderId: 'loader-3' });
disconnected.observe({ frameId: 'frame-3', loaderId: 'loader-3', name: 'init' });
disconnected.rejectAll(new Error('Chrome tab WebSocket closed'));
await assert.rejects(
  disconnectedRoute.promise,
  /page readiness aborted while waiting for route \/disconnected; last state="navigation-started"; Chrome tab WebSocket closed/,
);

console.log('✅ console smoke readiness: loaded, unfinished, and disconnected pages are distinct');
