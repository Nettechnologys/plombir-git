#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { parseUtoipaPaths, utoipaRowFor } from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const splitClientPath = path.join(root, 'web/src/lib/api/repos.ts');
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const blobPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/blob/[...path]/+page.svelte');
const editPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/edit/[...path]/+page.svelte');
const newPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/new/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/repo_content.rs');

const splitClient = productionTsSource(readFileSync(splitClientPath, 'utf8'));
const repoPage = productionTsSource(readFileSync(repoPagePath, 'utf8'));
const blobPage = productionTsSource(readFileSync(blobPagePath, 'utf8'));
const editPage = productionTsSource(readFileSync(editPagePath, 'utf8'));
const newPage = productionTsSource(readFileSync(newPagePath, 'utf8'));
const backend = readFileSync(backendPath, 'utf8');

const failures = [];

// Anchored on the handler, not on the file. Grepping the raw source for the
// path string asserted only that the characters appear *somewhere* — a
// commented-out annotation satisfied it exactly like a live one, and it never
// said which handler serves the splat (card_64b6ede78939).
const contentAnnotations = parseUtoipaPaths(
  backend,
  'api::repo_content',
  path.relative(root, backendPath).split(path.sep).join('/'),
);
for (const handler of ['create_or_update_file', 'delete_file']) {
  const row = utoipaRowFor(contentAnnotations, `api::repo_content::${handler}`);
  if (row?.path !== '/repos/{owner}/{name}/contents/{*path}') {
    failures.push(
      `Backend content route \`${handler}\` must remain a splat path endpoint (declared: ${row?.path ?? 'no annotation'}).`,
    );
  }
}

if (!/function\s+encodeRepoPath\s*\([^)]*\)[\s\S]*split\('\/'\)\.map\(encodeURIComponent\)\.join\('\/'\)/.test(splitClient)) {
  failures.push('Split repos API client must encode file path segments while preserving repository subdirectories.');
}

if (!/blob\/\$\{encodeRepoPath\(path\)\}/.test(splitClient)) {
  failures.push('Split repos.blob must encode file paths before calling the backend blob route.');
}

if (!/contents\/\$\{encodeRepoPath\(path\)\}/.test(splitClient)) {
  failures.push('Split repos.saveContent must encode file paths before calling the backend contents route.');
}

if (!/deleteContent[\s\S]*contents\/\$\{encodeRepoPath\(path\)\}[\s\S]*method:\s*'DELETE'/.test(splitClient)) {
  failures.push('Split repos.deleteContent must encode file paths before calling the backend contents route.');
}

if (!/function\s+encodeRepoPath\s*\([^)]*\)[\s\S]*split\('\/'\)\.map\(encodeURIComponent\)\.join\('\/'\)/.test(repoPage)) {
  failures.push('Repository browser must encode file path segments while preserving repository subdirectories.');
}

if (!/blob\/\$\{encodeRepoPath\(filePath\)\}/.test(repoPage)) {
  failures.push('Repository browser blob links must encode file paths before navigating to the blob route.');
}

if (/ref\s*\|\|\s*['"]main['"]/.test(repoPage)) {
  failures.push('Repository browser branch selector must not hardcode main when no ref query is selected.');
}

// The branch list is the primary source (it carries Git's own default marker,
// so the label cannot disagree with the highlighted dropdown entry); the
// repository row remains the fallback for a repo whose HEAD is still unborn.
if (!/currentRefLabel[\s\S]*branches\.find\(\(b: any\) => b\.is_default\)\?\.name[\s\S]*repoInfo\?\.default_branch/.test(repoPage)) {
  failures.push('Repository browser must display the backend default branch when no ref query is selected.');
}

if (/href="\/\{owner\}\/\{repo\}\/edit\/\{filePath\}/.test(blobPage)) {
  failures.push('Blob page must not render a literal owner/repo/filePath edit href.');
}

if (!/function\s+buildEditHref\s*\([\s\S]*blobData\?\.sha[\s\S]*params\.set\('ref',\s*ref\)[\s\S]*encodeRepoPath\(filePath\)/.test(blobPage)) {
  failures.push('Blob page edit link must include blob sha, active ref, and encoded file path.');
}

if (/href="\/\{owner\}\/\{repo\}"/.test(editPage + newPage)) {
  failures.push('Content editor cancel links must not render literal owner/repo placeholders.');
}

if (
  !/window\.location\.href\s*=\s*blobHref\(path,\s*targetBranch\)/.test(editPage) &&
  !/goto\(blobHref\(path,\s*targetBranch\)\)/.test(editPage) &&
  !/goto\(blobHref\(path,\s*payload\.branch\)\)/.test(editPage) &&
  !/goto\(blobHref\(expectedPath,\s*next\.branch,\s*expectedOwner,\s*expectedRepo\)\)/.test(editPage)
) {
  failures.push('Edit page must redirect back to the saved branch after saving.');
}

// Without `?ref=` both editor pages commit to the repository's own default
// branch (read from the repository row), never to a literal `main` — which on
// a `master` repository made "New file" commit to a branch that did not exist
// and the edit page read the file at a ref that did not exist
// (card_2e320f5287d7).
for (const [label, source] of [['New file page', newPage], ['Edit page', editPage]]) {
  if (!/let\s+branch\s*=\s*\$derived\(\$page\.url\.searchParams\.get\('ref'\)\s*\|\|\s*''\)/.test(source)) {
    failures.push(`${label} must initialize branch from the ref query parameter.`);
  }
  if (/searchParams\.get\('ref'\)\s*\|\|\s*['"]main['"]/.test(source)) {
    failures.push(`${label} must not fall back to a hardcoded main when no ref query is selected.`);
  }
  if (!/repos\.get\([\s\S]*default_branch/.test(source) || !/branch=\{targetBranch\}/.test(source)) {
    failures.push(`${label} must hand the editor the repository default branch when no ref query is selected.`);
  }
}

if (
  !/window\.location\.href\s*=\s*blobHref\(filePath,\s*targetBranch\)/.test(newPage) &&
  !/goto\(blobHref\(payload\.path,\s*payload\.branch\)\)/.test(newPage) &&
  !/goto\(blobHref\(next\.path,\s*next\.branch,\s*expectedOwner,\s*expectedRepo\)\)/.test(newPage)
) {
  failures.push('New file page must redirect back to the saved branch after creating.');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Repo content editor frontend/backend contract ok');
