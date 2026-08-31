#!/usr/bin/env node

// Behavioural fixtures for the two parsers that feed docs/ui-inventory.json.
// A green generated artefact is not proof that either parser still
// discriminates: the artefact and a broken parser can shrink together. These
// fixtures pin the false-positive and false-negative shapes from
// card_df6ac5d1165c, then assert the real tree still exposes the four dashboard
// template calls the old two-segment parser lost.

import {
  buildInventory,
  outrankingRoutes,
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

// card_e73d4d017693: module paths name frontend routes but execute no HTTP
// request. Static imports, re-exports and dynamic imports must therefore be
// invisible to the deliberately weak GET corpus scanner.
//
// Each statement below also names `request`, and that is deliberate: since
// card_de4bdc55196c a bare literal in the web corpus buys nothing anyway, so a
// specifier standing alone would pass with the blanking removed. A specifier
// that shares its statement with the transport — a barrel re-exporting
// `request`, the shape `web/src/lib/api/_base.ts` already has — is what the
// blanking is still the only defence against.
for (const [kind, moduleOnly] of [
  ['static import', `import { request } from '../../routes/admin/runners/+page.svelte';`],
  ['re-export', `export { request } from '../../routes/admin/runners/+page.svelte';`],
  ['dynamic import', `const get = async () => (await import('../../routes/admin/runners/+page.svelte')).request;`],
]) {
  const view = testSourceView('fixture.ts', moduleOnly);
  expect(
    touchedBy({ web: [{ file: 'fixture.ts', source: view }] }, 'GET',
      '/api/v1/admin/runners/{id}').length === 0,
    `${kind} module specifier must not count as routed GET coverage`,
  );
}

const navigationOnly = testSourceView('fixture.ts', `
setTestPage('/search?q=old&type=repos&page=1', {});
rendered = await renderComponent(SearchPage);
`);
expect(
  touchedBy(
    { web: [{ file: 'fixture.ts', source: navigationOnly }] },
    'GET',
    '/api/v1/search',
    ['search.search'],
  ).length === 0,
  'a browser navigation literal must not count as a GET on the route behind the page',
);

const clientSymbolDriven = testSourceView('fixture.ts', `
import { search } from '$lib/api/client.svelte';
search.search.mockResolvedValueOnce(page);
expect(search.search).toHaveBeenNthCalledWith(3, 'current', 'wiki', 3, 20);
`);
expect(
  JSON.stringify(touchedBy(
    { web: [{ file: 'fixture.ts', source: clientSymbolDriven }] },
    'GET',
    '/api/v1/search',
    ['search.search'],
  )) === JSON.stringify(['web']),
  'an executed client member must cover its route without any endpoint literal',
);

const symbolInProse = testSourceView('fixture.ts', `
import { search } from '$lib/api/client.svelte';
const described = 'search.search(query) is the call behind this page';
`);
expect(
  touchedBy(
    { web: [{ file: 'fixture.ts', source: symbolInProse }] },
    'GET',
    '/api/v1/search',
    ['search.search'],
  ).length === 0,
  'a client member named inside a string must not count as an executed call',
);

const shadowedNamespace = testSourceView('fixture.ts', `
const boards = new Map();
boards.get(7);
`);
expect(
  touchedBy(
    { web: [{ file: 'fixture.ts', source: shadowedNamespace }] },
    'GET',
    '/api/v1/repos/{owner}/{name}/boards/{id}',
    ['boards.get'],
  ).length === 0,
  'a local binding shadowing a client namespace must not prove the route behind it',
);

const realRunnerGet = testSourceView('fixture.ts', `
await client.get('/admin/runners/42');
`);
expect(
  JSON.stringify(touchedBy(
    { web: [{ file: 'fixture.ts', source: realRunnerGet }] },
    'GET',
    '/api/v1/admin/runners/{id}',
  )) === JSON.stringify(['web']),
  'a real GET to the same route must remain visible after module specifiers are blanked',
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

// card_146d32d61ec2: the router ranks path patterns — static, then placeholder,
// then catch-all — and only the winner's method table is consulted. A literal a
// static sibling owns is therefore no evidence at all for the placeholder route
// registered beside it.
const dispatchRoutes = [
  '/api/v1/repos/{owner}/{name}/pipelines',
  '/api/v1/repos/{owner}/{name}/pipelines/workflow-dispatch',
  '/api/v1/repos/{owner}/{name}/pipelines/{id}',
];
const dispatchLiteral = String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/alice/demo/pipelines/workflow-dispatch?ref=main"))
    .send()
    .await?;
`;
const dispatchCorpora = {
  rust: [{ file: 'fixture.rs', source: testSourceView('fixture.rs', dispatchLiteral) }],
  web: [{
    file: 'fixture.ts',
    source: testSourceView('fixture.ts', `
await request('GET', '/repos/alice/demo/pipelines/workflow-dispatch?ref=main');
`),
  }],
};
const dispatchOwner = dispatchRoutes[1];
const dispatchRival = dispatchRoutes[2];
expect(
  JSON.stringify(touchedBy(
    dispatchCorpora,
    'GET',
    dispatchOwner,
    [],
    outrankingRoutes(dispatchOwner, dispatchRoutes),
  )) === JSON.stringify(['rust', 'web']),
  'the static sibling must keep the literal the router delivers to it',
);
expect(
  touchedBy(
    dispatchCorpora,
    'GET',
    dispatchRival,
    [],
    outrankingRoutes(dispatchRival, dispatchRoutes),
  ).length === 0,
  'a placeholder route must not take credit for a literal its static sibling owns',
);

// The same ranking across methods: `…/{version}/yank` is registered for PATCH
// only, so a GET spelled that way answers 405 — it never falls through to the
// catch-all that would have taken the verb.
const yankRoutes = [
  '/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank',
  '/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}',
];
const yankCorpora = {
  rust: [{
    file: 'fixture.rs',
    source: testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/acme/demo/packages/npm/widget/1.2.3/yank"))
    .send()
    .await?;
`),
  }],
};
expect(
  touchedBy(
    yankCorpora,
    'GET',
    yankRoutes[1],
    [],
    outrankingRoutes(yankRoutes[1], yankRoutes),
  ).length === 0,
  'a catch-all must not take credit for a path a static sibling answers under another method',
);
const downloadLiteral = {
  rust: [{
    file: 'fixture.rs',
    source: testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api/v1/repos/acme/demo/packages/npm/widget/1.2.3/widget-1.2.3.tgz"))
    .send()
    .await?;
`),
  }],
};
expect(
  JSON.stringify(touchedBy(
    downloadLiteral,
    'GET',
    yankRoutes[1],
    [],
    outrankingRoutes(yankRoutes[1], yankRoutes),
  )) === JSON.stringify(['rust']),
  'a real download tail must still cover the catch-all route that answers it',
);

// A rival owns a literal only when it answers the whole of it. `/api-docs/` is
// registered beside the catch-all and its bytes open every documentation path
// there is, so a prefix match would take the whole tree away from the route
// that really serves it.
const apiDocsRoutes = ['/api-docs', '/api-docs/', '/api-docs/openapi.json', '/api-docs/{*tail}'];
const apiDocsCorpora = {
  rust: [{
    file: 'fixture.rs',
    source: testSourceView('fixture.rs', String.raw`
let response = client
    .get(format!("{base}/api-docs/swagger-ui.css"))
    .send()
    .await?;
`),
  }],
};
expect(
  JSON.stringify(touchedBy(
    apiDocsCorpora,
    'GET',
    '/api-docs/{*tail}',
    [],
    outrankingRoutes('/api-docs/{*tail}', apiDocsRoutes),
  )) === JSON.stringify(['rust']),
  'a catch-all must keep a tail no sibling answers',
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

// card_17fa92f7a5f7: the browser client has real transports outside request().
// The upload deliberately keeps XHR in a local helper so progress stays in one
// place; the exported member owns the literal route passed into that helper.
const transportApi = parseApiSurface([
  "import { downloadApiFile, withApiBase } from './base';",
  'function uploadWithProgress(path: string, file: File) {',
  '  const xhr = new XMLHttpRequest();',
  "  xhr.open('POST', withApiBase(path));",
  '  xhr.send(file);',
  '}',
  'export const transfers = {',
  "  download: (id: number) => downloadApiFile(`/artifacts/${id}/download`, 'artifact'),",
  '  upload: (owner: string, repo: string, id: number, file: File) =>',
  '    uploadWithProgress(`/repos/${owner}/${repo}/releases/${id}/assets`, file),',
  '  packageFile: (owner: string, repo: string, kind: string, name: string, version: string, file: string) =>',
  '    withApiBase(`/repos/${owner}/${repo}/packages/${kind}/${name}/${version}/${file}`),',
  '};',
].join('\n'), 'fixture-transports.ts');
const transportRows = new Map(transportApi.map((row) => [
  row.symbol,
  `${row.method} ${row.path}`,
]));
expect(
  transportRows.get('transfers.download') === 'GET /artifacts/{id}/download',
  `downloadApiFile must bind transfers.download to GET, got ${transportRows.get('transfers.download')}`,
);
expect(
  transportRows.get('transfers.upload') === 'POST /repos/{owner}/{repo}/releases/{id}/assets',
  `XHR helper must bind transfers.upload to POST, got ${transportRows.get('transfers.upload')}`,
);
expect(
  transportRows.get('transfers.packageFile')
    === 'GET /repos/{owner}/{repo}/packages/{kind}/{name}/{version}/{file}',
  `withApiBase URL factory must bind transfers.packageFile to GET, got ${transportRows.get('transfers.packageFile')}`,
);

// card_a5d1ee3a396f: a WebSocket constructor is a GET handshake. The first
// function deliberately has an object return type: its braces are not the
// function body and must not hide the constructor that follows.
const websocketApi = parseApiSurface([
  "import { withWebSocketApiBase } from './base';",
  'export function connectNotifications(): { disconnect(): void } {',
  "  const socket = new WebSocket(withWebSocketApiBase('/ws/notifications'));",
  "  const urlOnly = withWebSocketApiBase('/ws/not-a-handshake');",
  "  const label = '/ws/string-only';",
  '  return { disconnect: () => socket.close() };',
  '}',
  'export function connectJob(jobId: number): WebSocket | null {',
  '  return new WebSocket(withWebSocketApiBase(`/ws/job/${jobId}`));',
  '}',
].join('\n'), 'fixture-websockets.ts');
const websocketRows = new Map(websocketApi.map((row) => [
  row.symbol,
  `${row.method} ${row.path} via ${row.transport}`,
]));
expect(
  websocketRows.get('connectNotifications') === 'GET /ws/notifications via websocket',
  `notification WebSocket must be a GET handshake, got ${websocketRows.get('connectNotifications')}`,
);
expect(
  websocketRows.get('connectJob') === 'GET /ws/job/{jobId} via websocket',
  `job-log WebSocket must be a GET handshake, got ${websocketRows.get('connectJob')}`,
);
expect(
  websocketApi.length === 2,
  `URL factories and strings must not become WebSocket handshakes, got ${JSON.stringify(websocketApi)}`,
);

const directTransportPage = parsePageInventory(`
<script lang="ts">
  import { withApiBase, withBackendBase } from '$lib/api/_base';
  async function checkBackendReadiness() {
    await fetch(withBackendBase('/health'), { cache: 'no-store' });
    const navigationOnly = withApiBase('/not-fetched');
    window.location.href = '/string-only';
  }
  checkBackendReadiness();
</script>
`, 'fixture/+layout.svelte');
expect(
  directTransportPage.passiveTransports.length === 1
    && directTransportPage.passiveTransports[0].symbol
      === 'fixture/+layout.svelte#checkBackendReadiness'
    && directTransportPage.passiveTransports[0].method === 'GET'
    && directTransportPage.passiveTransports[0].path === '/health'
    && directTransportPage.passiveTransports[0].base === 'root'
    && directTransportPage.passiveTransports[0].transport === 'fetch',
  `direct transport parser must keep only the executable health fetch with exact owner, got ${JSON.stringify(directTransportPage.passiveTransports)}`,
);

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
  ['GET', '/health'],
  ['GET', '/api/v1/ws/notifications'],
  ['GET', '/api/v1/ws/job/{job_id}'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(row?.reachedFromUi === true, `${method} ${url} is still hidden from the UI surface`);
}
const healthLayoutCall = inventory.layouts
  .find((layout) => layout.file === 'web/src/routes/+layout.svelte')
  ?.passive.find((call) => call.routeUrl === '/health');
expect(
  healthLayoutCall?.symbol === 'web/src/routes/+layout.svelte#checkBackendReadiness'
    && healthLayoutCall.kind === 'transport'
    && healthLayoutCall.transport === 'fetch',
  `health layout provenance is not exact: ${JSON.stringify(healthLayoutCall)}`,
);

for (const [method, url] of [
  ['POST', '/api/v1/repos/{owner}/{name}/releases/{release_id}/assets'],
  ['GET', '/api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/download'],
  ['GET', '/api/v1/artifacts/{id}/download'],
  ['GET', '/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(row?.reachedFromUi === true, `${method} ${url} is still hidden from the UI surface`);
}
const releaseUpload = inventory.routes.find((row) => (
  row.method === 'POST'
    && row.url === '/api/v1/repos/{owner}/{name}/releases/{release_id}/assets'
));
expect(
  releaseUpload?.testedIn.includes('web'),
  'releaseAssets.test.ts must prove POST release asset upload through releases.uploadAsset',
);

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
  // Also carries `web` since card_de4bdc55196c: the mirror settings component
  // tests really drive `mirrors.update`, so this row is no longer rust-only.
  ['PATCH', '/api/v1/repos/{owner}/{name}/mirror'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    row?.testedIn.includes('rust'),
    `${method} ${url} is still hidden by a dynamically assembled test URL`,
  );
}

// card_b8608f60b29d and follow-ups: all fifteen requests below have live Rust coverage,
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

// card_146d32d61ec2, against the real tree. Every literal that used to buy these
// rows a `smoke` credit is the spelling of a *statically registered sibling* —
// `POST /v2/{owner}/{repo}/blobs/uploads` in a route inventory,
// `/api-docs/openapi.json` in the OpenAPI smoke, `/api/v1/repos/explore` in the
// frontend smoke, the package protocol paths in the route consumer check. The
// router delivers each of those to the sibling, so none of them is evidence
// about the placeholder route mounted beside it.
for (const [method, url, expected] of [
  ['GET', '/v2/{owner}/{repo}/blobs/{digest}', ['rust']],
  ['GET', '/api-docs/{*tail}', ['rust']],
  ['GET', '/api/v1/repos/{owner}', ['rust', 'web', 'browser']],
  ['GET', '/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}', ['rust', 'web', 'browser']],
  // `web` is the production `packages.downloadUrl` assertion added with the
  // transport reader, not the static sibling's `/yank` literal.
  ['GET', '/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}', ['rust', 'web']],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    JSON.stringify(row?.testedIn) === JSON.stringify(expected),
    `${method} ${url} must not be credited to a literal its static sibling owns, `
      + `got ${JSON.stringify(row?.testedIn)}`,
  );
}

// card_de4bdc55196c, against the real tree: `searchExploreAuditStateOwnership`
// mounts three pages and drives all four of these through the mocked client,
// spelling none of their URLs. They are the routes the literal oracle could not
// see — and `/api/v1/search`, the one it did see, it saw only because the test
// navigates to a page whose address happens to match.
for (const [method, url] of [
  ['GET', '/api/v1/search'],
  ['GET', '/api/v1/repos/explore'],
  ['GET', '/api/v1/admin/audit/logs'],
  ['GET', '/api/v1/admin/audit/logs/{id}'],
]) {
  const row = inventory.routes.find((candidate) => (
    candidate.method === method && candidate.url === url
  ));
  expect(
    row?.testedIn.includes('web'),
    `${method} ${url} lost the component test that really drives its client member`,
  );
}

if (failures.length > 0) {
  console.error('❌ UI inventory oracle fixtures failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log('✅ UI inventory oracle distinguishes methods/comments and preserves nested API calls');
