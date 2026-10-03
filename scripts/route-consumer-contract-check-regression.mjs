#!/usr/bin/env node

// Mutation stand for route-consumer-contract-check.mjs.
//
// That check answers "does anything call this mounted mutating route", and the
// two ways it can lie are opposite. It can go quiet — stop finding the client,
// stop reading the router, and report a tree it never looked at as clean. Or it
// can go blind the other way — let a client path made entirely of parameters
// stand for every route of that length, and report everything as called.
//
// Each fixture below breaks exactly one thing and names the sentence the check
// must produce. A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const checkName = 'route-consumer-contract-check.mjs';
const originalCheck = join(root, 'scripts', checkName);

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-route-consumer-'));
  mkdirSync(join(fixture, 'crates', 'rg-http', 'src'), { recursive: true });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(
    join(root, 'crates', 'rg-http', 'src', 'routes.rs'),
    join(fixture, 'crates', 'rg-http', 'src', 'routes.rs'),
  );
  cpSync(originalCheck, join(fixture, 'scripts', checkName));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  cpSync(join(root, 'web', 'src'), join(fixture, 'web', 'src'), { recursive: true });
  return fixture;
}

function edit(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runFixture(name, mutate, expectedStatus, expectedOutput) {
  const fixture = fixtureRoot();
  try {
    if (mutate) {
      mutate({
        router: join(fixture, 'crates', 'rg-http', 'src', 'routes.rs'),
        client: join(fixture, 'web', 'src'),
        auth: join(fixture, 'web', 'src', 'lib', 'api', 'auth.ts'),
        boards: join(fixture, 'web', 'src', 'lib', 'api', 'boards.ts'),
        packages: join(fixture, 'web', 'src', 'lib', 'api', 'packages.ts'),
        check: join(fixture, 'scripts', checkName),
      });
    }
    const result = spawnSync(process.execPath, [join(fixture, 'scripts', checkName)], {
      cwd: fixture,
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const expectedOutputs = Array.isArray(expectedOutput) ? expectedOutput : [expectedOutput];
    if (result.status !== expectedStatus || expectedOutputs.some((part) => !output.includes(part))) {
      throw new Error(
        `${name}: expected exit ${expectedStatus} and ${JSON.stringify(expectedOutputs)}, got exit ` +
          `${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
    return output;
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The copied tree is the shipped tree: if this one is not green, every red
// below is about the copy rather than about the mutation.
const unmutatedOutput = runFixture(
  'an unmutated copy of the tree passes',
  null,
  0,
  'route consumer contract ok',
);

// The defect the check exists for, in the shape it was last found in: a route
// mounted, gated and handled, whose only caller was never written
// (card_2cd2d40f27d2).
runFixture(
  'a mounted route whose client call is deleted is reported',
  ({ auth }) => {
    edit(
      auth,
      "    request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {",
      '    request<{ unlinked: boolean }>(`/auth/sso/__deleted__`, {',
    );
  },
  1,
  'DELETE /api/v1/auth/sso/{slug}/unlink',
);

// The same call, commented out rather than deleted. The client sources are read
// through `productionTsSource` precisely so that this is not a way to keep a
// gate green while the feature is gone.
runFixture(
  'a client call that is only commented out does not count',
  ({ auth }) => {
    edit(
      auth,
      '    request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {',
      '    // request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {\n' +
        '    request<{ unlinked: boolean }>(`/auth/sso/__commented__`, {',
    );
  },
  1,
  'DELETE /api/v1/auth/sso/{slug}/unlink',
);

const UPDATE_CARD =
  '  updateCard: (owner: string, repo: string, boardId: number, cardId: number, data: BoardCardUpdatePayload) =>\n' +
  "    request<BoardCard>(`/repos/${owner}/${repo}/boards/${boardId}/cards/${cardId}`, { method: 'PATCH', body: JSON.stringify(data) }),\n";

// Deleting updateCard leaves a DELETE call for the exact same route template
// and leaves unrelated PATCH methods in boards.ts. Combining method and path at
// file scope would therefore keep claiming a PATCH call that no member makes.
runFixture(
  'method and path from different board members cannot manufacture updateCard',
  ({ boards }) => edit(boards, UPDATE_CARD, ''),
  1,
  'PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}',
);

// Make the static sibling carry PATCH as well. Axum still selects its path
// registration first, so /cards/reorder cannot fall through to /cards/{id}.
const REORDER_CARD =
  '  reorderCards: (owner: string, repo: string, boardId: number, data: { column_id: number; positions: [number, number][] }) =>\n' +
  "    request<{ status: string }>(`/repos/${owner}/${repo}/boards/${boardId}/cards/reorder`, { method: 'POST', body: JSON.stringify(data) }),\n";
const PATCH_REORDER_CARD = REORDER_CARD
  .replace('  reorderCards:', '  reorderCardsPatchFixture:')
  .replace("method: 'POST'", "method: 'PATCH'");
const staticSiblingOnly = ({ boards }) => {
  edit(boards, UPDATE_CARD, '');
  edit(boards, REORDER_CARD, `${REORDER_CARD}${PATCH_REORDER_CARD}`);
};
runFixture(
  'a static sibling is not a client call to the placeholder route',
  staticSiblingOnly,
  1,
  'PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}',
);

// The independent package witness: a generic GET download tail lives beside
// the PATCH yank member. Removing the latter must not let the former donate its
// path to PATCH merely because both calls are in packages.ts.
runFixture(
  'a package download tail cannot replace the removed yank call',
  ({ packages }) => edit(packages, 'packageYankPath({ owner, repo, pkg_type, pkg_name, version })',
    '`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}/${encodeURIComponent(version)}/disabled-yank`'),
  1,
  'PATCH /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank',
);

const PROTOCOL_KEYS = [
  'POST /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems',
  'POST /{owner}/{repo}/git-upload-pack',
  'POST /{owner}/{repo}/git-receive-pack',
  'POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy/',
  'POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy',
];
const PROTOCOL_NOISE = [
  "const protocolNoiseOwner = 'owner';",
  "const protocolNoiseRepo = 'repo';",
  'export const protocolShapedPaths = [',
  '  `/repos/${protocolNoiseOwner}/${protocolNoiseRepo}/packages/rubygems/api/v1/gems`,',
  '  `/${protocolNoiseOwner}/${protocolNoiseRepo}/git-upload-pack`,',
  '  `/${protocolNoiseOwner}/${protocolNoiseRepo}/git-receive-pack`,',
  '  `/repos/${protocolNoiseOwner}/${protocolNoiseRepo}/packages/pypi/legacy/`,',
  '  `/repos/${protocolNoiseOwner}/${protocolNoiseRepo}/packages/pypi/legacy`,',
  '];',
  "export const unrelatedPost = () => fetch('/__unrelated__', { method: 'POST' });",
  '',
].join('\n');
const addProtocolNoise = ({ packages }) => {
  edit(packages, 'export const packages = {', `${PROTOCOL_NOISE}\nexport const packages = {`);
};
const protocolNoiseOutput = runFixture(
  'protocol-shaped file-level noise does not turn external endpoints into SPA calls',
  addProtocolNoise,
  0,
  'route consumer contract ok',
);
if (protocolNoiseOutput !== unmutatedOutput) {
  throw new Error(
    'adding or removing accidental dynamic protocol paths changed the route classification:\n' +
      `baseline: ${unmutatedOutput}\nwith noise: ${protocolNoiseOutput}`,
  );
}

// Re-introduce the deleted defect against the adversarial file above. Its five
// paths and its POST belong to different members, so a file-level join makes
// every exact protocol exemption look as though it gained an SPA caller.
runFixture(
  'returning the file-level join makes all five protocol false-greens visible',
  (paths) => {
    addProtocolNoise(paths);
    const { check } = paths;
    edit(check, '    return precise;', '    return precise ?? fileLevelConsumer(method, shape);');
  },
  1,
  [...PROTOCOL_KEYS, 'Delete the entry'],
);

// Mutation proof for the route-specificity half. The adversarial tree above is
// red with the shipped check; deleting only the rival rejection resurrects the
// exact false green this stand is meant to make visible.
runFixture(
  'removing route ownership resurrects the static-sibling false green',
  (paths) => {
    staticSiblingOnly(paths);
    edit(
      paths.check,
      '    && !rivalsOf(routeUrl).some((rival) => shapeCoveredBy(rival, clientShape));',
      ';',
    );
  },
  0,
  'route consumer contract ok',
);

// The blind direction. A client path of nothing but parameters matches any
// route of the same length, so the index would call every route consumed — and
// the check's own vacuity probe is what has to notice.
runFixture(
  'a client path made only of parameters cannot stand for every route',
  ({ client }) => {
    writeFileSync(
      join(client, 'lib', 'api', 'zz_wildcard_fixture.ts'),
      'export const anything = (a: string, b: string, c: string) =>\n' +
        '  fetch(`/${a}/${b}/${c}`, { method: \'DELETE\' });\n',
    );
  },
  1,
  'no longer discriminates between routes',
);

// The client is gone. Every route would read as an orphan, which is a loud
// failure — but it must be loud about the *cause*, not print a hundred
// accusations against a tree it never read.
runFixture(
  'a client tree it cannot read is reported as such, not as a hundred orphans',
  ({ client }) => {
    rmSync(join(client, 'lib'), { recursive: true, force: true });
    rmSync(join(client, 'routes'), { recursive: true, force: true });
  },
  1,
  'fix the path, not the code',
);

// The allowlist is a ratchet. An entry naming a route the router stopped
// serving must fail rather than sit there describing a server that is gone.
runFixture(
  'an allowlist entry for a route the router no longer mounts fails',
  ({ router }) => {
    edit(router, '"/runners/{id}/heartbeat"', '"/runners/{id}/heartbeat-renamed"');
  },
  1,
  'the router does not mount it any more',
);

// The other end of the same ratchet: an exemption that stopped applying,
// because somebody wired the route up after all.
runFixture(
  'an allowlist entry for a route that gained a caller fails',
  ({ client }) => {
    writeFileSync(
      join(client, 'lib', 'api', 'zz_runner_fixture.ts'),
      "export const heartbeat = (id: string) =>\n" +
        "  fetch(`/runners/${id}/heartbeat`, { method: 'POST' });\n",
    );
  },
  1,
  'Delete the entry',
);

console.log('route consumer contract regression stand green');
