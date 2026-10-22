#!/usr/bin/env node

// Keep the public capability table reviewable: every row names a tracked
// implementation entry point, and the claims that have drifted before retain
// a small source-level tripwire. This cannot replace behaviour tests.
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const readme = readFileSync(process.env.FEATURE_README_OVERRIDE || path.join(root, 'README.md'), 'utf8');
const section = readme.split(/^## Features\s*$/m)[1]?.split(/^## /m)[0];
const failures = [];
if (!section) failures.push('README Features section is missing');

const expected = [
  'Git transport', 'Repositories', 'Auth', 'Issues', 'Pull requests', 'Wiki',
  'Git LFS', 'CI/CD', 'Webhooks', 'Notifications', 'Registries', 'Search',
  'Import', 'Operations', 'AI agents', 'Releases', 'Web UI',
];
const rows = new Map();
for (const line of section?.split('\n') || []) {
  if (!line.startsWith('|')) continue;
  const cells = line.split('|').slice(1, -1).map((cell) => cell.trim());
  if (cells[0] === 'Area' || /^-+$/.test(cells[0])) continue;
  if (cells.length !== 3) {
    failures.push(`Feature row needs Area, Capability and Implementation: ${cells[0]}`);
    continue;
  }
  const [area, capability, evidence] = cells;
  if (rows.has(area)) failures.push(`Duplicate feature row: ${area}`);
  rows.set(area, capability);
  const linkedPath = /^\[[^\]]+\]\(([^)]+)\)$/.exec(evidence)?.[1];
  if (!linkedPath || !/^(crates\/|web\/).+\.(rs|svelte)$/.test(linkedPath)) {
    failures.push(`${area}: Implementation must link to a Rust route/service or Svelte page`);
  } else if (!existsSync(path.join(root, linkedPath))) {
    failures.push(`${area}: implementation link does not exist: ${linkedPath}`);
  }
}
for (const area of expected) if (!rows.has(area)) failures.push(`Feature row is missing: ${area}`);
for (const area of rows.keys()) if (!expected.includes(area)) failures.push(`Unreviewed feature row: ${area}`);

function requireClaim(area, pattern, message) {
  if (!pattern.test(rows.get(area) || '')) failures.push(`${area}: ${message}`);
}
function requireSource(file, pattern, message) {
  const source = readFileSync(path.join(root, file), 'utf8');
  if (!pattern.test(source)) failures.push(`${file}: ${message}`);
}

requireClaim('Wiki', /revision history stored in the database.*no `\.wiki\.git`/i, 'describe database revisions and the absent Git mirror');
requireClaim('Import', /release brings its files.*GitLab release's links are not fetched.*authorship belongs to the importing account/i, 'state which releases bring their files, and both release import limits');
requireClaim('Search', /PRs, users and indexed code.*build a code index explicitly/i, 'describe browser search and the index prerequisite');
requireSource('crates/rg-core/src/wiki/service.rs', /wiki_revision_ops/, 'wiki revision writer is missing');
requireSource('crates/rg-core/src/import/service.rs', /\.import\(release_id, &release\.tag_name, &release\.assets, stats\)/, 'GitHub/Gitea release import no longer hands its files to the asset fetcher');
requireSource('crates/rg-core/src/search/service.rs', /"pulls"\s*=>[\s\S]*"users"\s*=>[\s\S]*"code"\s*=>/, 'global search needs PR, user and code branches');
requireSource('web/src/routes/search/+page.svelte', /key: 'code'/, 'browser code tab is missing');
requireSource('crates/rg-http/src/routes.rs', /api::repo_content::get_blame/, 'file blame route is missing');
requireSource('crates/rg-http/src/api/issues.rs', /pub async fn delete_issue/, 'issue deletion is missing');
requireSource('crates/rg-http/src/api/issues.rs', /pub async fn edit_comment/, 'comment editing is missing');
requireSource('crates/rg-http/src/api/issues.rs', /pub async fn delete_comment/, 'comment deletion is missing');

if (failures.length) {
  console.error('README feature contract failed:\n' + failures.map((item) => `- ${item}`).join('\n'));
  process.exit(1);
}
console.log(`${rows.size}/${expected.length} README feature rows have implementation links and drift checks passed`);
