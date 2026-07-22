#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const splitClientPath = path.join(root, 'web/src/lib/api/repos.ts');
const repoPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/+page.svelte');
const blobPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/blob/[...path]/+page.svelte');
const editPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/edit/[...path]/+page.svelte');
const newPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/new/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/repo_content.rs');

const client = readFileSync(clientPath, 'utf8');
const splitClient = readFileSync(splitClientPath, 'utf8');
const repoPage = readFileSync(repoPagePath, 'utf8');
const blobPage = readFileSync(blobPagePath, 'utf8');
const editPage = readFileSync(editPagePath, 'utf8');
const newPage = readFileSync(newPagePath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');

const failures = [];

if (!/path\s*=\s*"\/repos\/\{owner\}\/\{name\}\/contents\/\{\*path\}"/.test(backend)) {
  failures.push('Backend content save route must remain a splat path endpoint.');
}

if (!/function\s+encodeRepoPath\s*\([^)]*\)[\s\S]*split\('\/'\)\.map\(encodeURIComponent\)\.join\('\/'\)/.test(client)) {
  failures.push('API client must encode file path segments while preserving repository subdirectories.');
}

if (!/blob\/\$\{encodeRepoPath\(path\)\}/.test(client)) {
  failures.push('repos.blob must encode file paths before calling the backend blob route.');
}

if (!/contents\/\$\{encodeRepoPath\(path\)\}/.test(client)) {
  failures.push('repos.saveContent must encode file paths before calling the backend contents route.');
}

if (!/deleteContent[\s\S]*contents\/\$\{encodeRepoPath\(path\)\}[\s\S]*method:\s*'DELETE'/.test(client)) {
  failures.push('repos.deleteContent must encode file paths before calling the backend contents route.');
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

if (!/currentRefLabel[\s\S]*repoInfo\?\.default_branch[\s\S]*branches\.find\(\(b: any\) => b\.is_default\)\?\.name/.test(repoPage)) {
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
  !/goto\(blobHref\(path,\s*payload\.branch\)\)/.test(editPage)
) {
  failures.push('Edit page must redirect back to the saved branch after saving.');
}

if (!/let\s+branch\s*=\s*\$derived\(\$page\.url\.searchParams\.get\('ref'\)\s*\|\|\s*'main'\)/.test(newPage)) {
  failures.push('New file page must initialize branch from the ref query parameter.');
}

if (
  !/window\.location\.href\s*=\s*blobHref\(filePath,\s*targetBranch\)/.test(newPage) &&
  !/goto\(blobHref\(payload\.path,\s*payload\.branch\)\)/.test(newPage)
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
