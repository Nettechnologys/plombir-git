#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPaths = [
  path.join(root, 'web/src/lib/api/client.svelte.ts'),
  path.join(root, 'web/src/lib/api/repos.ts'),
];
const headerPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/repos.rs');
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');

const backend = readFileSync(backendPath, 'utf8');
const router = readFileSync(routerPath, 'utf8');
const header = readFileSync(headerPath, 'utf8');
const repoPage = readFileSync(repoPagePath, 'utf8');
const basePath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const base = readFileSync(basePath, 'utf8');
const failures = [];

for (const route of [
  'path = "/repos/{owner}/{name}/star"',
  'path = "/repos/{owner}/{name}/starred"',
  'path = "/repos/{owner}/{name}/watch"',
  'path = "/repos/{owner}/{name}"',
]) {
  if (!backend.includes(route)) {
    failures.push(`Backend repo action OpenAPI annotation missing: ${route}`);
  }
}

for (const [label, pattern] of [
  ['PUT /star', /\.route\(\s*"\/repos\/\{owner\}\/\{name\}\/star",\s*put\(api::repos::star_repo\)\s*\)/],
  ['GET /starred', /\.route\(\s*"\/repos\/\{owner\}\/\{name\}\/starred",\s*get\(api::repos::get_starred_status\),?\s*\)/],
  ['GET /watch', /"\/repos\/\{owner\}\/\{name\}\/watch"[\s\S]*get\(api::repos::get_watch_status\)/],
  ['PUT /watch', /"\/repos\/\{owner\}\/\{name\}\/watch"[\s\S]*\.put\(api::repos::watch_repo\)/],
  ['DELETE /watch', /"\/repos\/\{owner\}\/\{name\}\/watch"[\s\S]*\.delete\(api::repos::unwatch_repo\)/],
  ['DELETE /repos/{owner}/{name}', /"\/repos\/\{owner\}\/\{name\}"[\s\S]*delete\(api::repos::delete_repo_handler\)/],
]) {
  if (!pattern.test(router)) {
    failures.push(`Backend repo action router binding missing: ${label}`);
  }
}

const deleteHandlerBlock = backend.match(/pub async fn delete_repo_handler[\s\S]*?\n\}/);
if (!deleteHandlerBlock) {
  failures.push('Backend repo delete handler missing');
} else {
  if (!/StatusCode::OK[\s\S]*"deleted": true/.test(deleteHandlerBlock[0])) {
    failures.push('Backend repo delete handler must return the JSON deleted envelope used by the frontend');
  }
  if (/StatusCode::NO_CONTENT/.test(deleteHandlerBlock[0])) {
    failures.push('Backend repo delete handler must not return 204 while the frontend expects JSON');
  }
}

const deleteAnnotationBlock = backend.match(/pub async fn delete_repo_handler[\s\S]*?responses\([\s\S]*?\)\s*,\s*\)\]/)
  || backend.match(/\/\/\/ DELETE \/api\/v1\/repos\/:owner\/:name[\s\S]*?pub async fn delete_repo_handler/);
if (deleteAnnotationBlock && /status = 204/.test(deleteAnnotationBlock[0])) {
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

const httpCloneLine = header.match(/httpCloneUrl\s*=\s*\$derived\([^\n]+\)/)?.[0] || '';

if (/location\.(protocol|host)/.test(httpCloneLine)) {
  failures.push('RepoHeader HTTP clone URL must not use the frontend location origin');
}

if (/\.git/.test(httpCloneLine)) {
  failures.push('RepoHeader HTTP clone URL must not append .git to the backend /git/{owner}/{repo} path');
}

if (!/import\s+\{[^}]*buildSshCloneUrl[^}]*\}\s+from '\$lib\/api\/_base'/.test(header)) {
  failures.push('RepoHeader must use the shared SSH clone URL helper');
}

if (!/sshCloneUrl\s*=\s*\$derived\(browser\s*\?\s*buildSshCloneUrl\(owner,\s*repo,\s*location\.hostname\)\s*:\s*''\)/.test(header)) {
  failures.push('RepoHeader SSH clone URL must use ssh://git@host:port/{owner}/{repo}');
}

const sshCloneLine = header.match(/sshCloneUrl\s*=\s*\$derived\([^\n]+\)/)?.[0] || '';

if (/git@[^`]*:\$\{owner\}\/\$\{repo\}\.git/.test(sshCloneLine)) {
  failures.push('RepoHeader SSH clone URL must not use scp-like default-port syntax with .git suffix');
}

if (!/import\s+\{[^}]*withBackendBase[^}]*\}\s+from '\$lib\/api\/_base'/.test(repoPage)) {
  failures.push('Repository page must import withBackendBase for empty-repo HTTP clone instructions');
}

if (!/httpCloneUrl\s*=\s*\$derived\(withBackendBase\(`\/git\/\$\{encodeURIComponent\(owner\)\}\/\$\{encodeURIComponent\(repo\)\}`\)\)/.test(repoPage)) {
  failures.push('Repository empty state HTTP clone URL must target backend /git/{owner}/{repo}, not the frontend origin');
}

const repoPageHttpCloneLine = repoPage.match(/httpCloneUrl\s*=\s*\$derived\([^\n]+\)/)?.[0] || '';

if (/location\.(protocol|host)/.test(repoPageHttpCloneLine)) {
  failures.push('Repository empty state HTTP clone URL must not use the frontend location origin');
}

if (/\.git/.test(repoPageHttpCloneLine)) {
  failures.push('Repository empty state HTTP clone URL must not append .git to the backend /git/{owner}/{repo} path');
}

if (!/import\s+\{[^}]*buildSshCloneUrl[^}]*\}\s+from '\$lib\/api\/_base'/.test(repoPage)) {
  failures.push('Repository page must use the shared SSH clone URL helper');
}

if (!/sshCloneUrl\s*=\s*\$derived\(browser\s*\?\s*buildSshCloneUrl\(owner,\s*repo,\s*location\.hostname\)\s*:\s*''\)/.test(repoPage)) {
  failures.push('Repository empty state SSH clone URL must use ssh://git@host:port/{owner}/{repo}');
}

const repoPageSshCloneLine = repoPage.match(/sshCloneUrl\s*=\s*\$derived\([^\n]+\)/)?.[0] || '';

if (/git@[^`]*:\$\{owner\}\/\$\{repo\}\.git/.test(repoPageSshCloneLine)) {
  failures.push('Repository empty state SSH clone URL must not use scp-like default-port syntax with .git suffix');
}

if (!/VITE_SSH_HOST/.test(base) || !/VITE_SSH_PORT/.test(base)) {
  failures.push('Shared API base must expose configurable SSH clone host and port');
}

if (!/configuredSshPort\s*\|\|\s*'2222'/.test(base)) {
  failures.push('Shared SSH clone URL helper must default to IronForge SSH port 2222');
}

if (!/ssh:\/\/git@/.test(base) || /\.git/.test(base.match(/buildSshCloneUrl[\s\S]*?\n\}/)?.[0] || '')) {
  failures.push('Shared SSH clone URL helper must emit ssh:// URLs without appending .git');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Repo action frontend/backend contract ok');
