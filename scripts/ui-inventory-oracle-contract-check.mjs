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

// card_e0145cf4574d: a placeholder match must consume its closing brace. The
// old `[^/]+` could backtrack before `}`, after which the right-boundary check
// accepted that brace and credited this child request to `/boards/{id}` too.
const boardChildOnly = testSourceView('fixture.rs', String.raw`
let response = client
    .patch(format!("{base}/api/v1/repos/acme/demo/boards/{board_id}/columns/{col_id}"))
    .send()
    .await?;
`);
const boardChildCorpora = { rust: [{ file: 'fixture.rs', source: boardChildOnly }] };
expect(
  JSON.stringify(touchedBy(
    boardChildCorpora,
    'PATCH',
    '/api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}',
  )) === JSON.stringify(['rust']),
  'a templated child URL must cover its complete route',
);
expect(
  touchedBy(
    boardChildCorpora,
    'PATCH',
    '/api/v1/repos/{owner}/{name}/boards/{id}',
  ).length === 0,
  'a templated child URL must not end inside `}` and cover its parent route',
);

// Rust's positional `format!` placeholder is the empty pair `{}`. It still
// represents exactly one path segment and must not disappear merely because
// the source does not name the argument inside the braces.
const rustPositionalFormat = testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/{}/{}/pulls/{}/reviews/{}", owner, repo, pr, review))
    .send()
    .await?;
`);
expect(
  JSON.stringify(touchedBy(
    { rust: [{ file: 'fixture.rs', source: rustPositionalFormat }] },
    'GET',
    '/api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}',
  )) === JSON.stringify(['rust']),
  'an empty Rust format placeholder must cover one complete route segment',
);

// card_da18427b4ee6: an Axum `{*path}` consumes a tail, not one segment. A
// concrete file path is the evidence real tests contain; merely finding the
// route template itself elsewhere in the corpus is not a substitute.
const wildcardConcrete = testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/acme/demo/blob/src/lib.rs"))
    .send()
    .await?;
`);
expect(
  JSON.stringify(touchedBy(
    { rust: [{ file: 'fixture.rs', source: wildcardConcrete }] },
    'GET',
    '/api/v1/repos/{owner}/{name}/blob/{*path}',
  )) === JSON.stringify(['rust']),
  'a concrete multi-segment tail must cover an Axum catch-all route',
);
const wildcardWithoutTail = testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/acme/demo/blob"))
    .send()
    .await?;
`);
expect(
  touchedBy(
    { rust: [{ file: 'fixture.rs', source: wildcardWithoutTail }] },
    'GET',
    '/api/v1/repos/{owner}/{name}/blob/{*path}',
  ).length === 0,
  'a catch-all route must not be covered by its prefix with no tail',
);

// A trailing slash is part of a path, not decoration. Six registrations carry
// one on purpose — `/v2/`, the OCI upload start, `/api-docs/` and the two pypi
// indexes — because that is what docker, pip and a browser send, and each is
// mounted beside a slashless alias. Folding the pair into one row hid every
// test that writes the real client URL.
const slashSpelling = testSourceView('fixture.rs', String.raw`
let response = client
    .post(format!("{base}/v2/acme/demo/blobs/uploads/"))
    .send()
    .await?;
`);
const slashCorpora = { rust: [{ file: 'fixture.rs', source: slashSpelling }] };
expect(
  JSON.stringify(touchedBy(slashCorpora, 'POST', '/v2/{owner}/{repo}/blobs/uploads/'))
    === JSON.stringify(['rust']),
  'the spelling a docker push sends must cover the route mounted for it',
);
expect(
  touchedBy(slashCorpora, 'POST', '/v2/{owner}/{repo}/blobs/uploads').length === 0,
  'the trailing-slash route must not cover the bare alias mounted beside it',
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
  ['GET', '/v2/'],
  ['POST', '/v2/{owner}/{repo}/blobs/uploads/'],
  ['GET', '/api-docs/'],
  ['POST', '/api/v1/repos/{owner}/{name}/packages/pypi/legacy/'],
  ['GET', '/api/v1/repos/{owner}/{name}/packages/pypi/simple/'],
  ['GET', '/api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}/'],
]) {
  const rows = inventory.routes.filter((row) => row.method === method && row.url === url);
  expect(
    rows.length === 1,
    `${method} ${url} is not one row of the artefact — its trailing slash was folded away`,
  );
}

for (const [method, url] of [
  ['GET', '/git/{owner}/{repo}/info/refs'],
  ['GET', '/api/v1/repos/{owner}/{name}/starred'],
  ['GET', '/api/v1/repos/{owner}/{name}/issues/{number}/labels'],
  ['GET', '/api/v1/repos/{owner}/{name}/issues/{number}/time'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/issues'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/prs'],
  ['GET', '/api/v1/ai/repos/{owner}/{name}/tree'],
  // card_d482cf7e098e: the second pass over the same class. Each of these is
  // driven by a real request whose URL the test used to assemble from parts.
  ['POST', '/v2/{owner}/{repo}/blobs/uploads'],
  ['POST', '/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems'],
  ['GET', '/api/v1/repos/{owner}/{name}/packages/pypi/simple'],
  ['GET', '/api/v1/users/mfa/backup'],
  ['DELETE', '/api/v1/repos/{owner}/{name}/hooks/{id}'],
  ['GET', '/api/v1/repos/{owner}/{name}/hooks/{id}/deliveries'],
  ['POST', '/api/v1/runners/{id}/jobs/{job_id}/log'],
  ['PUT', '/api/v1/runners/{id}/jobs/{job_id}/artifacts/staging'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    row?.testedIn.includes('rust'),
    `${method} ${url} is still hidden by a dynamically assembled test URL`,
  );
}

// card_b8608f60b29d and follow-ups: all fourteen requests below have live Rust coverage,
// but their tests assembled URLs from a root, ids or a loop. The source oracle
// could not join those fragments, so each route looked wholly untested. The
// full spellings now drive those same tests (not comments or inventory-only
// constants), and the copied-tree regression harness mutates every matching
// registration in `routes.rs` to prove the join is independent.
for (const [method, url] of [
  ['POST', '/git/{owner}/{repo}/git-upload-pack'],
  ['POST', '/git/{owner}/{repo}/git-receive-pack'],
  ['POST', '/{owner}/{repo}/git-upload-pack'],
  ['POST', '/{owner}/{repo}/git-receive-pack'],
  ['GET', '/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets'],
  ['DELETE', '/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}'],
  ['GET', '/api/v1/repos/{owner}/{name}/pulls/{number}/assets'],
  ['DELETE', '/api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}'],
  ['GET', '/api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets'],
  ['PATCH', '/api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}'],
  ['GET', '/api-docs'],
  ['GET', '/api/v1/repos/{owner}/{name}/releases/assets/{asset_id}'],
  ['POST', '/api/v1/repos/{owner}/{name}/mirror/sync'],
  ['HEAD', '/v2/{owner}/{repo}/manifests/{reference}'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    JSON.stringify(row?.testedIn) === JSON.stringify(['rust']),
    `${method} ${url} must be credited only to the live Rust request, got ${JSON.stringify(row?.testedIn)}`,
  );
}

// card_d8bf49e9dc95: these protocol writes and the NuGet availability probe
// were already executed by content-aware integration tests, but their request
// URLs were assembled through helpers, advertised resources or variables. The
// complete spellings now sit at the live HTTP verbs, and the copied-tree
// harness below mutates every matching registration independently.
for (const [method, url] of [
  ['PUT', '/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new'],
  ['DELETE', '/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/yank'],
  ['PUT', '/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/unyank'],
  ['POST', '/api/v1/repos/{owner}/{name}/packages/npm/publish'],
  ['PUT', '/api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}'],
  ['PUT', '/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}'],
  ['DELETE', '/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}'],
  ['POST', '/api/v1/repos/{owner}/{name}/packages/pypi/legacy'],
  ['HEAD', '/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}'],
  ['POST', '/api/v1/repos/{owner}/{name}/packages/nuget/publish'],
  ['PUT', '/api/v1/repos/{owner}/{name}/packages/nuget/publish'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    JSON.stringify(row?.testedIn) === JSON.stringify(['rust']),
    `${method} ${url} must be credited only to the live Rust request, got ${JSON.stringify(row?.testedIn)}`,
  );
}

if (failures.length > 0) {
  console.error('❌ UI inventory oracle fixtures failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log('✅ UI inventory oracle distinguishes methods/comments and preserves nested API calls');
