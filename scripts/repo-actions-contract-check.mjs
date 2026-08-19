#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import {
  loadRouteTable,
  parseUtoipaPaths,
  requireBlock,
  routeFailures,
  rustFnBlock,
  utoipaRowFor,
} from './lib/rust-source.mjs';

const root = process.cwd();
const clientPaths = [
  path.join(root, 'web/src/lib/api/repos.ts'),
];
const headerPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/repos.rs');
const routerPath = path.join(root, 'crates/rg-http/src/routes.rs');

const backend = readFileSync(backendPath, 'utf8');
const routes = loadRouteTable(routerPath);
const header = readFileSync(headerPath, 'utf8');
const repoPage = readFileSync(repoPagePath, 'utf8');
const basePath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const base = readFileSync(basePath, 'utf8');
const failures = [];

// The annotations are read through the parser rather than searched for as text
// in the raw file: `backend.includes('path = "…"')` is satisfied by a
// commented-out annotation, by a doc-comment example, and by a `#[cfg(test)]`
// fixture — none of which reach the published spec (card_64b6ede78939).
const annotations = parseUtoipaPaths(backend, 'api::repos', path.relative(root, backendPath));

for (const route of [
  '/repos/{owner}/{name}/star',
  '/repos/{owner}/{name}/starred',
  '/repos/{owner}/{name}/watch',
  '/repos/{owner}/{name}',
]) {
  if (!annotations.some((row) => row.path === route)) {
    failures.push(`Backend repo action OpenAPI annotation missing: path = "${route}"`);
  }
}

// Starring and watching are per-user state on a repository the caller may read,
// hence `RepoAuthRead` — anonymous reads of a public repo must not be able to
// star it. Deleting the repository is `RepoOwner`, a level above admin.
failures.push(
  ...routeFailures(routes, [
    {
      method: 'PUT',
      path: '/repos/{owner}/{name}/star',
      handler: 'api::repos::star_repo',
      access: 'RepoAuthRead',
    },
    {
      method: 'GET',
      path: '/repos/{owner}/{name}/starred',
      handler: 'api::repos::get_starred_status',
      access: 'RepoAuthRead',
    },
    {
      method: 'GET',
      path: '/repos/{owner}/{name}/watch',
      handler: 'api::repos::get_watch_status',
      access: 'RepoAuthRead',
    },
    {
      method: 'PUT',
      path: '/repos/{owner}/{name}/watch',
      handler: 'api::repos::watch_repo',
      access: 'RepoAuthRead',
    },
    {
      method: 'DELETE',
      path: '/repos/{owner}/{name}/watch',
      handler: 'api::repos::unwatch_repo',
      access: 'RepoAuthRead',
    },
    {
      method: 'DELETE',
      path: '/repos/{owner}/{name}',
      handler: 'api::repos::delete_repo_handler',
      access: 'RepoOwner',
    },
  ]),
);

const deleteHandler = rustFnBlock(backend, 'delete_repo_handler');
if (deleteHandler === null) {
  failures.push('Backend repo delete handler missing');
} else {
  if (!/StatusCode::OK[\s\S]*"deleted": true/.test(deleteHandler.body)) {
    failures.push('Backend repo delete handler must return the JSON deleted envelope used by the frontend');
  }
  if (/StatusCode::NO_CONTENT/.test(deleteHandler.body)) {
    failures.push('Backend repo delete handler must not return 204 while the frontend expects JSON');
  }
}

// The annotation is read through the parser that attributes each
// `#[utoipa::path(...)]` to the function *below* it. The previous spelling
// anchored on `pub async fn delete_repo_handler` and reached forward with
// `[\s\S]*?` — but the annotation stands *above* its handler, so the 82-line
// match ran into the `responses(...)` of the next handler, the fork. The claim
// about the delete contract was reading the fork's annotation, and it failed
// both ways: a `204` returning to delete went unnoticed, while a `204` on
// whichever handler happens to follow reddened the gate under delete's name
// (card_9808ff5aec29).
const deleteAnnotation = utoipaRowFor(
  annotations,
  'api::repos::delete_repo_handler',
);
if (!deleteAnnotation) {
  failures.push('Backend repo delete handler must carry a #[utoipa::path] annotation this check can attribute to it');
} else if (deleteAnnotation.responsesBody === null) {
  failures.push('Backend repo delete OpenAPI annotation must declare its responses(...)');
} else if (/status\s*=\s*204/.test(deleteAnnotation.responsesBody)) {
  failures.push('Backend repo delete OpenAPI annotation must not advertise an unused 204 response');
}

for (const clientPath of clientPaths) {
  const source = readFileSync(clientPath, 'utf8');
  const name = path.relative(root, clientPath);

  if (!/starred:\s*\([^)]*\)\s*=>\s*\n?\s*request<\{\s*starred:\s*boolean\s*\}>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/starred`,\s*\{\s*method:\s*'GET'\s*\}/.test(source)) {
    failures.push(`${name} must expose repos.starred using GET /starred`);
  }

  if (!/watchStatus:\s*\([^)]*\)\s*=>\s*\n?\s*request<\{\s*watch_state:\s*'not_watching'\s*\|\s*'watching'\s*\|\s*'ignoring'\s*\}>/.test(source)) {
    failures.push(`${name} must expose repos.watchStatus with the backend watch-state union`);
  }

  if (!/delete:\s*\([^)]*\)\s*=>\s*\n?\s*request<\{\s*deleted:\s*boolean\s*\}>\(`\/repos\/\$\{owner\}\/\$\{repo\}`,\s*\{\s*method:\s*'DELETE'\s*\}/.test(source)) {
    failures.push(`${name} must model repo delete as the backend JSON deleted envelope`);
  }

  const unstarBlock = source.match(/unstar:\s*async\s*\([^)]*\)\s*=>\s*\{[\s\S]*?\n\s*\},/);
  if (!unstarBlock) {
    failures.push(`${name} must expose async repos.unstar`);
    continue;
  }

  if (!/repos\.starred\(owner,\s*repo\)/.test(unstarBlock[0])) {
    failures.push(`${name} repos.unstar must check GET /starred before toggling`);
  }

  if (!/if\s*\(!status\.starred\)\s*return\s*\{\s*starred:\s*false\s*\}/.test(unstarBlock[0])) {
    failures.push(`${name} repos.unstar must be idempotent when already unstarred`);
  }
}

if (!/repos\.starred\(owner,\s*repo\)/.test(header)) {
  failures.push('RepoHeader must load starred status from the backend before rendering the star action');
}

if (!/repos\.watchStatus\(owner,\s*repo\)/.test(header)) {
  failures.push('RepoHeader must load watch status from the backend before rendering the watch action');
}

if (!/import\s+\{[^}]*withBackendBase[^}]*\}\s+from '\$lib\/api\/_base'/.test(header)) {
  failures.push('RepoHeader must import withBackendBase so clone URLs use the configured backend origin');
}

if (!/httpCloneUrl\s*=\s*\$derived\(withBackendBase\(`\/git\/\$\{encodeURIComponent\(owner\)\}\/\$\{encodeURIComponent\(repo\)\}`\)\)/.test(header)) {
  failures.push('RepoHeader HTTP clone URL must target backend /git/{owner}/{repo}, not the frontend origin');
}

// The two assertions below are negative, so they must never run against a
// block that simply failed to match — an empty string satisfies both.
const httpCloneLine = requireBlock(
  header,
  /httpCloneUrl\s*=\s*\$derived\([^\n]+\)/,
  'RepoHeader httpCloneUrl $derived expression could not be located',
  failures,
);

if (httpCloneLine && /location\.(protocol|host)/.test(httpCloneLine)) {
  failures.push('RepoHeader HTTP clone URL must not use the frontend location origin');
}

if (httpCloneLine && /\.git/.test(httpCloneLine)) {
  failures.push('RepoHeader HTTP clone URL must not append .git to the backend /git/{owner}/{repo} path');
}

if (!/import\s+\{[^}]*buildSshCloneUrl[^}]*\}\s+from '\$lib\/api\/_base'/.test(header)) {
  failures.push('RepoHeader must use the shared SSH clone URL helper');
}

if (!/sshCloneUrl\s*=\s*\$derived\(browser\s*\?\s*buildSshCloneUrl\(owner,\s*repo,\s*location\.hostname\)\s*:\s*''\)/.test(header)) {
  failures.push('RepoHeader SSH clone URL must use ssh://git@host:port/{owner}/{repo}');
}

const sshCloneLine = requireBlock(
  header,
  /sshCloneUrl\s*=\s*\$derived\([^\n]+\)/,
  'RepoHeader sshCloneUrl $derived expression could not be located',
  failures,
);

if (sshCloneLine && /git@[^`]*:\$\{owner\}\/\$\{repo\}\.git/.test(sshCloneLine)) {
  failures.push('RepoHeader SSH clone URL must not use scp-like default-port syntax with .git suffix');
}

if (!/import\s+\{[^}]*withBackendBase[^}]*\}\s+from '\$lib\/api\/_base'/.test(repoPage)) {
  failures.push('Repository page must import withBackendBase for empty-repo HTTP clone instructions');
}

if (!/httpCloneUrl\s*=\s*\$derived\(withBackendBase\(`\/git\/\$\{encodeURIComponent\(owner\)\}\/\$\{encodeURIComponent\(repo\)\}`\)\)/.test(repoPage)) {
  failures.push('Repository empty state HTTP clone URL must target backend /git/{owner}/{repo}, not the frontend origin');
}

const repoPageHttpCloneLine = requireBlock(
  repoPage,
  /httpCloneUrl\s*=\s*\$derived\([^\n]+\)/,
  'Repository page httpCloneUrl $derived expression could not be located',
  failures,
);

if (repoPageHttpCloneLine && /location\.(protocol|host)/.test(repoPageHttpCloneLine)) {
  failures.push('Repository empty state HTTP clone URL must not use the frontend location origin');
}

if (repoPageHttpCloneLine && /\.git/.test(repoPageHttpCloneLine)) {
  failures.push('Repository empty state HTTP clone URL must not append .git to the backend /git/{owner}/{repo} path');
}

if (!/import\s+\{[^}]*buildSshCloneUrl[^}]*\}\s+from '\$lib\/api\/_base'/.test(repoPage)) {
  failures.push('Repository page must use the shared SSH clone URL helper');
}

if (!/sshCloneUrl\s*=\s*\$derived\(browser\s*\?\s*buildSshCloneUrl\(owner,\s*repo,\s*location\.hostname\)\s*:\s*''\)/.test(repoPage)) {
  failures.push('Repository empty state SSH clone URL must use ssh://git@host:port/{owner}/{repo}');
}

const repoPageSshCloneLine = requireBlock(
  repoPage,
  /sshCloneUrl\s*=\s*\$derived\([^\n]+\)/,
  'Repository page sshCloneUrl $derived expression could not be located',
  failures,
);

if (repoPageSshCloneLine && /git@[^`]*:\$\{owner\}\/\$\{repo\}\.git/.test(repoPageSshCloneLine)) {
  failures.push('Repository empty state SSH clone URL must not use scp-like default-port syntax with .git suffix');
}

if (!/VITE_SSH_HOST/.test(base) || !/VITE_SSH_PORT/.test(base)) {
  failures.push('Shared API base must expose configurable SSH clone host and port');
}

if (!/configuredSshPort\s*\|\|\s*'2222'/.test(base)) {
  failures.push('Shared SSH clone URL helper must default to ForgeKeep SSH port 2222');
}

const buildSshCloneUrlBody = requireBlock(
  base,
  /buildSshCloneUrl[\s\S]*?\n\}/,
  'Shared API base must define buildSshCloneUrl',
  failures,
);

if (!/ssh:\/\/git@/.test(base) || (buildSshCloneUrlBody && /\.git/.test(buildSshCloneUrlBody))) {
  failures.push('Shared SSH clone URL helper must emit ssh:// URLs without appending .git');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Repo action frontend/backend contract ok');
