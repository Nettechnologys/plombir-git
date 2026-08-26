#!/usr/bin/env node

// Behavioural fixtures for the two parsers that feed docs/ui-inventory.json.
// A green generated artefact is not proof that either parser still
// discriminates: the artefact and a broken parser can shrink together. These
// fixtures pin the false-positive and false-negative shapes from
// card_df6ac5d1165c, then assert the real tree still exposes the four dashboard
// template calls the old two-segment parser lost.

import {
  buildInventory,
  testSourceView,
  touchedBy,
} from './ui-inventory.mjs';
import { parseApiSurface, parsePageInventory } from './lib/ui-surface.mjs';

const failures = [];
const expect = (condition, message) => {
  if (!condition) failures.push(message);
};

const route = '/api/v1/repos/{owner}/{name}/watch';
const foreignVerb = testSourceView('fixture.rs', String.raw`
// client.delete(format!("{base}/api/v1/repos/acme/demo/watch"));
let response = client
    .get(format!("{base}/api/v1/repos/acme/demo/watch"))
    .send()
    .await?;
`);
const corpora = { rust: [{ file: 'fixture.rs', source: foreignVerb }] };
expect(
  touchedBy(corpora, 'DELETE', route).length === 0,
  'a GET plus a commented-out DELETE must not cover DELETE on the same URL',
);
expect(
  JSON.stringify(touchedBy(corpora, 'GET', route)) === JSON.stringify(['rust']),
  'the live GET request in the fixture must still cover GET',
);

const genericRequest = testSourceView('fixture.rs', String.raw`
let response = client
    .request(reqwest::Method::DELETE, format!("{base}/api/v1/repos/acme/demo/watch"))
    .send()
    .await?;
`);
expect(
  JSON.stringify(touchedBy(
    { rust: [{ file: 'fixture.rs', source: genericRequest }] },
    'DELETE',
    route,
  )) === JSON.stringify(['rust']),
  'reqwest::Method must bind a generic request call to its actual verb',
);

const statusOnly = testSourceView('fixture.ts', `
expect(request).toHaveBeenCalledWith('/api/v1/repos/acme/demo/commits/abc/statuses');
`);
expect(
  touchedBy({ web: [{ file: 'fixture.ts', source: statusOnly }] }, 'GET',
    '/api/v1/repos/{owner}/{name}/commits/{sha}/status').length === 0,
  '`/statuses` must not cover its `/status` prefix',
);
expect(
  touchedBy({ web: [{ file: 'fixture.ts', source: statusOnly }] }, 'GET',
    '/api/v1/repos/{owner}/{name}/commits/{sha}').length === 0,
  'a child URL must not cover its parent route',
);

const api = parseApiSurface(`
import { request } from './base';
export const a = {
  b: {
    c: () => request('/nested'),
  },
};
`, 'fixture-api.ts');
expect(api.length === 1 && api[0].symbol === 'a.b.c',
  `nested API leaf parsed as ${JSON.stringify(api.map((row) => row.symbol))}, expected a.b.c`);

const page = parsePageInventory(`
<script lang="ts">
  import { a } from '$lib/api/client.svelte';
  async function load() { await a.b.c(); }
  load();
</script>
`, 'fixture/+page.svelte');
expect(
  page.passiveCalls.includes('a.b.c') && api.some((row) => page.passiveCalls.includes(row.symbol)),
  `a.b.c() did not join to its API leaf: passive=${JSON.stringify(page.passiveCalls)}`,
);

const inventory = buildInventory();
for (const suffix of ['gitignores', 'licenses', 'readmes', 'labels']) {
  const expected = `/api/v1/repos/templates/${suffix}`;
  const row = inventory.routes.find(({ method, url }) => method === 'GET' && url === expected);
  expect(row?.reachedFromUi === true, `${expected} is still not reached from the dashboard`);
}

for (const [method, url] of [
  ['GET', '/git/{owner}/{repo}/info/refs'],
  ['GET', '/api/v1/repos/{owner}/{name}/starred'],
  ['GET', '/api/v1/repos/{owner}/{name}/issues/{number}/labels'],
  ['GET', '/api/v1/repos/{owner}/{name}/issues/{number}/time'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/issues'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/prs'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/tree'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    row?.testedIn.includes('rust'),
    `${method} ${url} is still hidden by a dynamically assembled test URL`,
  );
}

if (failures.length > 0) {
  console.error('❌ UI inventory oracle fixtures failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log('✅ UI inventory oracle distinguishes methods/comments and preserves nested API calls');
