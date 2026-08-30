#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource, rustFnBlock, rustStructBody } from './lib/rust-source.mjs';
import { productionTsSource, tsFunctionBody } from './lib/ts-source.mjs';

const root = process.cwd();
const files = {
  splitClient: path.join(root, 'web/src/lib/api/repos.ts'),
  orgPage: path.join(root, 'web/src/routes/orgs/[name]/+page.svelte'),
  ownerPage: path.join(root, 'web/src/routes/[owner]/+page.svelte'),
  dashboard: path.join(root, 'web/src/routes/dashboard/+page.svelte'),
  backend: path.join(root, 'crates/rg-http/src/api/repos.rs'),
};

const splitClient = productionTsSource(readFileSync(files.splitClient, 'utf8'));
const orgPageSource = readFileSync(files.orgPage, 'utf8');
const orgPage = productionTsSource(orgPageSource);
const ownerPage = productionTsSource(readFileSync(files.ownerPage, 'utf8'));
const dashboard = productionTsSource(readFileSync(files.dashboard, 'utf8'));
// The production view: comments and `#[cfg(test)]` items alike are blanked, so
// a commented-out handler reads as a deleted one and a test double cannot
// stand in for the handler the server ships.
const backend = productionRustSource(productionTsSource(readFileSync(files.backend, 'utf8')));
const failures = [];

function expectCreateObjectContract(label, source) {
  const createBlock = source.match(/create:\s*\([^)]*\)\s*=>\s*\n?\s*request<[\s\S]*?\/repos[\s\S]*?\n\s*\}\),/);
  if (!createBlock) {
    failures.push(`${label} must expose repos.create for POST /repos`);
    return;
  }

  if (!/create:\s*\(\s*opts\s*:/.test(createBlock[0])) {
    failures.push(`${label} repos.create must accept the backend CreateRepoRequest object, not positional args`);
  }

  if (!/body:\s*JSON\.stringify\(opts\)/.test(createBlock[0])) {
    failures.push(`${label} repos.create must serialize the full options object so org/template fields reach the backend`);
  }
}

expectCreateObjectContract('web/src/lib/api/repos.ts', splitClient);

// Read inside the struct rather than `/pub struct CreateRepoRequest[\s\S]*pub org:/`,
// which any `pub org:` further down the file satisfies — see `rustStructBody`.
const createRepoRequest = rustStructBody(backend, 'CreateRepoRequest');
if (createRepoRequest === null) {
  failures.push('api/repos.rs no longer defines a `struct CreateRepoRequest` this check can read');
} else if (!/\bpub org:\s*Option<String>/.test(createRepoRequest)) {
  failures.push('Backend CreateRepoRequest must keep org as an optional repository owner field');
}

// ── `org` must reach the repository row, through the gate that resolved it ──
//
// The assertion here used to be `/body\.org[\s\S]*get_org_by_name/` over the
// whole module, for a message naming `create_repo`. The resolution moved out of
// the handler and into the `NamespaceCreate` body extractor (card_1e1ed1ee06f1),
// so `create_repo` has not called `get_org_by_name` for some time — the only
// occurrence left in the file is inside `list_repos`, and `[\s\S]*` bridged the
// two. The gate was green on a handler it no longer described, and red on a
// rename inside a handler it never mentioned (card_c7aef378ad3d).
//
// So the subject is named explicitly, and it is the pair the contract actually
// rests on now:
//
//   1. `CreateRepoRequest` declares `org` as the namespace the gate reads —
//      `TargetOwnerOrSelf::target_owner_or_self` returning it is what makes
//      `NamespaceCreate` resolve and authorize that namespace at all. A body
//      that answers `None` there is created under the caller's own account no
//      matter what `org` says.
//   2. `create_repo` takes `NamespaceCreate<CreateRepoRequest>` and stores the
//      `org_id` *the gate resolved* on the new repository. Looking the name up
//      a second time is what let the handler's copy of the rule drift.
const targetOwner = /impl\s+TargetOwnerOrSelf\s+for\s+CreateRepoRequest\s*\{([\s\S]*?)\n\}/.exec(backend);
if (targetOwner === null) {
  failures.push(
    'api/repos.rs no longer implements TargetOwnerOrSelf for CreateRepoRequest, so nothing tells the ' +
      'NamespaceCreate gate which namespace the `org` field asks for',
  );
} else if (!/self\.org\b/.test(targetOwner[1])) {
  failures.push(
    'CreateRepoRequest::target_owner_or_self must return the `org` field — it is the only thing that ' +
      'points the create gate at the organization, and a body that answers None is silently created ' +
      "under the caller's own account",
  );
}

const createRepo = rustFnBlock(backend, 'create_repo');
if (createRepo === null) {
  failures.push('api/repos.rs no longer defines a `pub async fn create_repo` this check can read');
} else {
  if (!/NamespaceCreate\s*<\s*CreateRepoRequest\s*>/.test(createRepo.params)) {
    failures.push(
      'Backend create_repo must take NamespaceCreate<CreateRepoRequest>: the namespace is named by the ' +
        'body, so the gate over it is the body extractor and a handler asking the question itself keeps ' +
        'a second copy of the membership rule',
    );
  }
  const orgIdBinding = /NamespaceCreate\s*\{[^}]*\borg_id(?:\s*:\s*(\w+))?[^}]*\}/.exec(createRepo.params);
  if (orgIdBinding === null) {
    failures.push('Backend create_repo must destructure `org_id` out of NamespaceCreate — the namespace the gate resolved');
  } else {
    const orgId = orgIdBinding[1] ?? 'org_id';
    if (!new RegExp(`\\borg_id\\s*:\\s*${orgId}\\b|\\borg_id\\b(?=\\s*,)`).test(createRepo.body)) {
      failures.push(
        `Backend create_repo must pass the gate-resolved \`${orgId}\` into CreateRepoOptions.org_id — ` +
          'otherwise the repository lands outside the organization the caller was authorized for',
      );
    }
  }
}

const createOrgRepo = tsFunctionBody(orgPageSource, 'createOrgRepo');
if (!/let\s+name\s*=\s*\$derived\(\$page\.params\.name!\)/.test(orgPage)) {
  failures.push('Organization page repository owner snapshot must originate from the reactive route name');
} else if (createOrgRepo === null) {
  failures.push('Organization page no longer defines a readable `createOrgRepo` handler');
} else if (
  !/const\s+expectedName\s*=\s*name\b/.test(createOrgRepo)
  || !/repos\.create\(\s*\{[\s\S]*name:\s*newRepoName[\s\S]*is_private:\s*newRepoPrivate[\s\S]*org:\s*expectedName\b/.test(createOrgRepo)
) {
  failures.push(
    'Organization page must snapshot its route owner and include that snapshot in the repository create payload',
  );
}

if (/repos\.create\(\s*newRepoName\s*,/.test(orgPage)) {
  failures.push('Organization page must not call the stale positional repos.create API');
}

// ── The way in has to exist, not just the endpoint behind it ─────────────────
//
// Everything above proves a member *can* create a repository under an
// organization. None of it proves anyone can find out how. The whole feature
// was reported as missing while every assertion above was green: the only entry
// point was the small form on `/orgs/{name}`, the dashboard's create form had
// no owner field at all, and `/{owner}` — where a repository's own breadcrumb
// lands — offered neither the action nor a link to the page that does. A
// namespace you can POST to and cannot reach is not shipped.

if (!/org:\s*createOwner\s*\|\|\s*undefined/.test(dashboard)) {
  failures.push(
    'Dashboard create form must send the selected owner as `org` — without it the primary "new ' +
      'repository" button can only ever create under the personal account',
  );
}

if (!/bind:value=\{createOwner\}/.test(dashboard)) {
  failures.push(
    'Dashboard create form must bind an owner selector to `createOwner`, or the org field it sends ' +
      'can never be anything but the personal account',
  );
}

if (!/searchParams\.get\('owner'\)/.test(dashboard)) {
  failures.push(
    "Dashboard must honour `?owner=<name>`: it is how the organization page and an owner profile hand " +
      'this form a namespace instead of carrying a second, poorer create form of their own',
  );
}

if (!/href=\{`\/orgs\/\$\{org\.name\}`\}/.test(ownerPage)) {
  failures.push(
    'Owner page must link an organization to `/orgs/{name}` — it is the only page that manages the ' +
      "organization, and a repository's breadcrumb lands here, not there",
  );
}

if (!/href=\{`\/dashboard\?owner=\$\{encodeURIComponent\(owner\)\}`\}/.test(ownerPage)) {
  failures.push('Owner page must offer repository creation for a namespace the viewer may create in');
}

if (!/canCreate\s*=\s*mine\.some/.test(ownerPage)) {
  failures.push(
    "Owner page must decide the create action by the viewer's own organization membership — the API " +
      'admits any member, and a button shown to a stranger only promises a 403',
  );
}

if (!/\{#if canCreateRepo\}/.test(orgPage)) {
  failures.push(
    'Organization page must gate its create form on membership: it renders for every reader of a ' +
      'public organization otherwise, and a stranger learns of the refusal only on submit',
  );
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Organization repository creation frontend/backend contract ok');
