#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { loadProductionRust } from './lib/rust-consumer-contract.mjs';
import { checkPrivateForkSources, checkDirectForkFetches } from './private-fork-source-contract-check.mjs';

const production = loadProductionRust(path.resolve('crates'));
assert.deepEqual(checkPrivateForkSources(production), [], 'baseline must be green');
const service = path.resolve('crates/rg-core/src/pull_request/service.rs');
const queue = path.resolve('crates/rg-core/src/pull_request/merge_queue.rs');
const policy = path.resolve('crates/rg-core/src/pull_request/fork_source.rs');
const mutations = [
  [service, 'fork_source::may_publish(', 'fork_source::unchecked('],
  [service, 'if frozen', 'if false'],
  [queue, 'fork_source::resolve(', 'fork_source::unchecked('],
  [policy, 'can_write_repo(', 'can_read_repo('],
  [policy, 'is_usable()', 'is_active'],
  [policy, 'let verdict = is_live(', 'let verdict = unchecked('],
];
for (const [file, oldText, newText] of mutations) {
  const changed = new Map(production);
  assert(changed.get(file).includes(oldText), `mutation anchor ${oldText}`);
  changed.set(file, changed.get(file).replace(oldText, newText));
  assert(checkPrivateForkSources(changed).length, `gate survived removing ${oldText}`);
}
for (const sink of ['run_fork_fetch', 'compare_and_swap_open_head_sha', 'update_open_head_sha', 'advance_open_head_sha']) {
  const changed = new Map(production);
  changed.set(path.resolve('crates/rg-core/src/pull_request/unreviewed.rs'), `fn bypass() { ${sink}(); }`);
  assert(checkPrivateForkSources(changed).length, `unreviewed ${sink} bypass survived`);
}
const bypass = path.resolve('crates/rg-core/src/pull_request/unreviewed.rs');
assert(checkDirectForkFetches(
  new Map([[bypass, 'fn bypass() { git.run(&[       ]); }']]),
  new Map([[bypass, 'fn bypass() { git.run(&["fetch"]); }']]),
).length, 'a new direct Git fetch bypass survived');
console.log('Private fork source regression: 11 mutations rejected');
