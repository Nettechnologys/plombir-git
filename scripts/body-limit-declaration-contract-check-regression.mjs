#!/usr/bin/env node

// Mutation stand for body-limit-declaration-contract-check.mjs.
//
// That check answers "does every route buffering an opaque body declare the
// ceiling it buffers up to", and a green answer is worth exactly as much as the
// red ones it is still capable of. The two ways it could lie are opposite: it
// can stop recognising buffered extractors — at which point every route is
// trivially compliant — or it can accept a wrapper that declares nothing, which
// is the trap the check's own subject documents one level down.
//
// So each fixture below puts back one shape of the defect, in the spellings it
// really arrived in: a route mounted plainly (the inbound CI webhook,
// card_f82dd9f820e1), a route whose wrapper was taken away (the job log,
// card_f0958a52d286; the OCI manifest, card_6cbde71c452d), a wrapper swapped
// for one that gates but does not bound, and the one line inside
// `apply_body_limit` that would return every declared route to Axum's 2 MiB
// while every wrapper still reads as present.
//
// The floors and the truth boundary get the same treatment, because a check
// that stopped reading is indistinguishable from a clean tree by its exit code
// alone: a `BUFFERING` list that matches nothing must trip the floor rather
// than report success, and a declaration that exists only inside a comment must
// read as absent.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const checkName = 'body-limit-declaration-contract-check.mjs';

/** A throwaway copy of the tree the check reads: the router, the handlers, itself. */
function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-body-limit-'));
  mkdirSync(join(fixture, 'crates', 'rg-http'), { recursive: true });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, 'crates', 'rg-http', 'src'), join(fixture, 'crates', 'rg-http', 'src'), {
    recursive: true,
  });
  cpSync(join(root, 'scripts', checkName), join(fixture, 'scripts', checkName));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  return fixture;
}

function edit(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

const failures = [];

function expect(name, mutate, { red, mentions = [] }) {
  const fixture = fixtureRoot();
  try {
    mutate({
      router: join(fixture, 'crates', 'rg-http', 'src', 'routes.rs'),
      routeTable: join(fixture, 'crates', 'rg-http', 'src', 'route_table.rs'),
      check: join(fixture, 'scripts', checkName),
    });
    const result = spawnSync(process.execPath, [join(fixture, 'scripts', checkName)], {
      cwd: fixture,
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (red && result.status === 0) {
      failures.push(`${name}: the check stayed green on a mutated fixture — it asserts nothing`);
      return;
    }
    if (!red && result.status !== 0) {
      failures.push(`${name}: the check went red on a fixture it should accept:\n${output}`);
      return;
    }
    for (const fragment of mentions) {
      if (!output.includes(fragment)) {
        failures.push(`${name}: the verdict never mentions ${JSON.stringify(fragment)}:\n${output}`);
        return;
      }
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The copied tree is the shipped tree. If this is not green, every red below is
// about the copy rather than about the mutation it claims to prove.
expect('an unmutated copy of the tree passes', () => {}, {
  red: false,
});

// ── The defect, in the shapes it actually arrived in ───────────────────────

expect(
  'a buffering route mounted with no wrapper at all is caught',
  ({ router }) =>
    edit(
      router,
      `        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/webhooks/external/ci",
            api::webhooks_external::external_ci_webhook,
            &external_ci_webhook_envelope,
        )`,
      `        .post(
            RepoWrite,
            "/repos/{owner}/{name}/webhooks/external/ci",
            api::webhooks_external::external_ci_webhook,
        )`,
    ),
  {
    red: true,
    mentions: [
      'POST /api/v1/repos/{owner}/{name}/webhooks/external/ci',
      'mounted with no wrapper at all',
    ],
  },
);

expect(
  'the job log losing its wrapper is caught',
  ({ router }) =>
    edit(
      router,
      `        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/log",
            api::runners::upload_log,
            &runner_auth_job_log,
        )`,
      `        .post(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/log",
            api::runners::upload_log,
        )`,
    ),
  { red: true, mentions: ['POST /api/v1/runners/{id}/jobs/{job_id}/log', 'String'] },
);

expect(
  'the OCI manifest push losing its wrapper is caught',
  ({ router }) =>
    edit(
      router,
      `        .put_with(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/manifests/{reference}",
            oci::put_manifest,
            &manifest_limit,
        )`,
      `        .put(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/manifests/{reference}",
            oci::put_manifest,
        )`,
    ),
  { red: true, mentions: ['/v2/{owner}/{repo}/manifests/{reference}'] },
);

// A wrapper is still present, and it is a real one — it just bounds nothing.
// This is the half a check reading "is there a fourth argument" would miss.
expect(
  'a wrapper that gates but declares no ceiling is caught',
  ({ router }) => edit(router, '            &runner_auth_job_log,', '            &runner_auth,'),
  {
    red: true,
    mentions: ['`Wrap::runner_auth` — not one of the constructors that carry a body limit'],
  },
);

expect(
  'a wrapper naming a binding that does not exist is caught',
  ({ router }) =>
    edit(router, '            &runner_auth_job_log,', '            &job_log_limit_todo,'),
  { red: true, mentions: ['not a `Wrap` bound in this router'] },
);

// ── The one line under every declaration on the list ───────────────────────

expect(
  'apply_body_limit dropping the extractor half is caught',
  ({ routeTable }) =>
    edit(
      routeTable,
      'mr.layer::<_, std::convert::Infallible>(DefaultBodyLimit::max(body_limit))\n        .layer',
      'mr.layer',
    ),
  {
    red: true,
    mentions: ['`apply_body_limit` no longer applies `DefaultBodyLimit::max`'],
  },
);

expect(
  'a limit constructor that stops applying the limit is caught',
  ({ routeTable }) =>
    edit(
      routeTable,
      'Self::plain(move |mr| apply_body_limit(mr, body_limit))',
      'Self::plain(move |mr| mr)',
    ),
  { red: true, mentions: ['`Wrap::body_limit` does not reach'] },
);

// ── The check going quiet ──────────────────────────────────────────────────

expect(
  'a BUFFERING list that recognises nothing trips the floor',
  ({ check }) => edit(check, "{ kind: 'String', matches: (type) => type === 'String' },", ''),
  { red: true, mentions: ['buffering route(s) were recognised'] },
);

// A handler the check cannot place is unknown, not compliant: it must be
// reported rather than skipped, or moving a handler to a module the resolver
// does not find is a way out of the rule.
expect(
  'a handler that cannot be resolved to a file is reported, not skipped',
  ({ router }) =>
    edit(router, 'api::runners::upload_log,', 'api::runners_moved::upload_log,'),
  { red: true, mentions: ['cannot be resolved to a file'] },
);

// ── The truth boundary ─────────────────────────────────────────────────────

// A declaration that exists only inside a comment is not a declaration. The
// check reads the production view, so this must read exactly like the plain
// mount above rather than like a route that declares 64 KiB.
expect(
  'a wrapper commented out at the mount reads as absent',
  ({ router }) =>
    edit(
      router,
      '            &external_ci_webhook_envelope,\n        )',
      '            // &external_ci_webhook_envelope,\n        )',
    ),
  { red: true, mentions: ['/repos/{owner}/{name}/webhooks/external/ci'] },
);

// ── Exemptions ─────────────────────────────────────────────────────────────

expect(
  'an exemption with a reason lets a deliberate decision through',
  ({ router, check }) => {
    edit(
      router,
      `        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/webhooks/external/ci",
            api::webhooks_external::external_ci_webhook,
            &external_ci_webhook_envelope,
        )`,
      `        .post(
            RepoWrite,
            "/repos/{owner}/{name}/webhooks/external/ci",
            api::webhooks_external::external_ci_webhook,
        )`,
    );
    edit(
      check,
      'const EXEMPT = [];',
      `const EXEMPT = [
  {
    route: 'POST /api/v1/repos/{owner}/{name}/webhooks/external/ci',
    handler: 'api::webhooks_external::external_ci_webhook',
    reason: 'fixture',
  },
];`,
    );
  },
  { red: false },
);

expect(
  'an exemption for a route that does declare a ceiling is stale, and says so',
  ({ check }) =>
    edit(
      check,
      'const EXEMPT = [];',
      `const EXEMPT = [
  {
    route: 'POST /api/v1/repos/{owner}/{name}/webhooks/external/ci',
    handler: 'api::webhooks_external::external_ci_webhook',
    reason: 'fixture',
  },
];`,
    ),
  { red: true, mentions: ['declares a ceiling now, so its exemption is stale'] },
);

expect(
  'an exemption naming no mounted route is caught',
  ({ check }) =>
    edit(
      check,
      'const EXEMPT = [];',
      `const EXEMPT = [
  {
    route: 'POST /api/v1/repos/{owner}/{name}/webhooks/removed',
    handler: 'api::webhooks_external::gone',
    reason: 'fixture',
  },
];`,
    ),
  { red: true, mentions: ['matches no buffering route in the router'] },
);

expect(
  'an exemption with no reason is caught',
  ({ router, check }) => {
    edit(
      router,
      `            api::webhooks_external::external_ci_webhook,
            &external_ci_webhook_envelope,`,
      '            api::webhooks_external::external_ci_webhook,',
    );
    edit(
      check,
      'const EXEMPT = [];',
      `const EXEMPT = [
  {
    route: 'POST /api/v1/repos/{owner}/{name}/webhooks/external/ci',
    handler: 'api::webhooks_external::external_ci_webhook',
    reason: '',
  },
];`,
    );
  },
  { red: true, mentions: ['carries no reason, which is what it is for'] },
);

if (failures.length > 0) {
  console.error('❌ body limit declaration stand failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  '✅ body limit declaration stand: the check goes red on every shape of the defect, and quiet ' +
    'is a failure rather than a pass',
);
