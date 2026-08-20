#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource, rustFnBlock, rustStructBody } from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const files = {
  splitClient: path.join(root, 'web/src/lib/api/repos.ts'),
  orgPage: path.join(root, 'web/src/routes/orgs/[name]/+page.svelte'),
  backend: path.join(root, 'crates/rg-http/src/api/repos.rs'),
};

const splitClient = productionTsSource(readFileSync(files.splitClient, 'utf8'));
const orgPage = productionTsSource(readFileSync(files.orgPage, 'utf8'));
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

if (!/repos\.create\(\s*\{[\s\S]*name:\s*newRepoName[\s\S]*is_private:\s*newRepoPrivate[\s\S]*org:\s*page\.params\.name!/.test(orgPage)) {
  failures.push('Organization page must create repositories with an object payload including the org owner');
}

if (/repos\.create\(\s*newRepoName\s*,/.test(orgPage)) {
  failures.push('Organization page must not call the stale positional repos.create API');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Organization repository creation frontend/backend contract ok');
