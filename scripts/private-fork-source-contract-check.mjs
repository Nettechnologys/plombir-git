#!/usr/bin/env node
// Publishing a private source must keep checking the PR author's authority.
// This ratchet complements request/ODB tests; it is not a Rust type checker.
import path from 'node:path';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadProductionRust, blankRustComments, stripCfgTestItems } from './lib/rust-consumer-contract.mjs';

function functions(source) {
  const result = [];
  for (const match of source.matchAll(/^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)\b/gm)) {
    const open = source.indexOf('{', match.index);
    if (open < 0) continue;
    let end = open + 1, depth = 1;
    while (end < source.length && depth) {
      if (source[end] === '{') depth++;
      if (source[end] === '}') depth--;
      end++;
    }
    result.push({ name: match[1], open, end, body: source.slice(open, end) });
  }
  return result;
}

const SERVICE = 'crates/rg-core/src/pull_request/service.rs';
const SOURCE = 'crates/rg-core/src/pull_request/fork_source.rs';
const QUEUE = 'crates/rg-core/src/pull_request/merge_queue.rs';

export function checkPrivateForkSources(production) {
  const failures = [], entries = new Map();
  for (const [file, source] of production) {
    const relative = path.relative(process.cwd(), path.resolve(file)).split(path.sep).join('/');
    const fns = functions(source);
    for (const fn of fns) entries.set(`${relative}:${fn.name}`, fn.body);
    for (const call of source.matchAll(/\b(run_fork_fetch|compare_and_swap_open_head_sha|update_open_head_sha|advance_open_head_sha)\s*\(/g)) {
      if (/\bfn\s+$/.test(source.slice(0, call.index))) continue;
      const fn = fns.find(f => f.open < call.index && f.end > call.index);
      const key = `${relative}:${fn?.name ?? '<unclassified>'}`;
      const allowed = call[1] === 'run_fork_fetch'
        ? [`${SERVICE}:compute_diff`, `${SERVICE}:compare_branches`, `${SERVICE}:merge_claimed_pr`]
        : call[1] === 'compare_and_swap_open_head_sha'
          ? [`${SOURCE}:refresh_open_heads`, `${SOURCE}:advance_open_heads`] : [];
      if (!allowed.includes(key)) failures.push(`${key}: unreviewed ${call[1]} source-publication path`);
    }
  }
  const requireBefore = (key, guard, sink) => {
    const body = entries.get(key);
    const at = body?.indexOf(guard) ?? -1;
    const until = sink ? (body?.indexOf(sink) ?? -1) : body?.length;
    if (at < 0 || until === undefined || until < 0 || at >= until) failures.push(`${key}: ${guard} must precede ${sink ?? 'completion'}`);
  };
  requireBefore(`${SERVICE}:create_pr`, 'fork_source::may_publish(', 'try_get_branch_sha(');
  requireBefore(`${SERVICE}:compute_diff`, 'fork_source::resolve(', 'run_blocking_git(');
  requireBefore(`${SERVICE}:compute_diff`, 'if frozen', 'require_pull_request_branch(');
  requireBefore(`${SERVICE}:compute_diff`, 'recorded_diff_revs(', 'require_pull_request_branch(');
  requireBefore(`${SERVICE}:merge_pr`, 'fork_source::is_live(', 'claim_merge(');
  requireBefore(`${SERVICE}:merge_claimed_pr`, 'fork_source::resolve(', 'require_pull_request_branch(');
  requireBefore(`${SERVICE}:try_auto_merge`, 'fork_source::is_live(', 'claim_auto_merge(');
  requireBefore(`${QUEUE}:ensure_merge_group_ci`, 'fork_source::resolve(', 'fork_head_ci_is_approved(');
  requireBefore('crates/rg-core/src/pull_request/ci.rs:fork_head_ci_is_approved', 'fork_source::is_live(', 'ci_approved_sha');
  requireBefore('crates/rg-core/src/push_hooks.rs:synchronize_open_pr_heads', 'fork_source::refresh_open_heads(');
  requireBefore('crates/rg-core/src/review/service.rs:apply_suggestions', 'fork_source::advance_open_heads(');
  for (const name of ['refresh_open_heads', 'advance_open_heads']) {
    requireBefore(`${SOURCE}:${name}`, 'cached_is_live(', 'compare_and_swap_open_head_sha(');
  }
  requireBefore(`${SOURCE}:cached_is_live`, 'is_live(', 'verdicts.insert(');
  for (const check of ['deleted_at.is_some()', 'is_usable()', 'can_write_repo(']) {
    requireBefore(`${SOURCE}:may_publish`, check);
  }
  return failures;
}

export function checkDirectForkFetches(production, originals) {
  const failures = [];
  // A new direct fetch must not silently sidestep the shared helper's caller
  // allowlist. Only the existing helper and the separately gated queue own one.
  for (const [file, source] of production) {
    if (!file.includes('/pull_request/')) continue;
    const relative = path.relative(process.cwd(), file).split(path.sep).join('/');
    const fns = functions(source);
    const strings = blankRustComments(stripCfgTestItems(originals.get(file)));
    for (const match of strings.matchAll(/"fetch"/g)) {
      const fn = fns.find(f => f.open < match.index && f.end > match.index);
      const key = `${relative}:${fn?.name ?? '<unclassified>'}`;
      // Rebase rehearsal fetches from its own temporary worktree, built solely
      // from already-held base objects; it never reads a foreign repository.
      if (![`${SERVICE}:run_fork_fetch`, `${QUEUE}:ensure_merge_group_ci`, `${SERVICE}:rebase_group_tree`].includes(key)) failures.push(`${key}: unreviewed direct fork fetch`);
    }
  }
  return failures;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const production = loadProductionRust(path.resolve('crates'));
  const originals = new Map([...production.keys()].filter(file => file.includes('/pull_request/')).map(file => [file, readFileSync(file, 'utf8')]));
  const failures = [...checkPrivateForkSources(production), ...checkDirectForkFetches(production, originals)];
  if (failures.length) {
    console.error(failures.join('\n')); process.exitCode = 1;
  } else console.log('Private fork source: creation, refresh, diff, CI and merge gates verified');
}
