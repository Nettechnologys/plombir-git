#!/usr/bin/env node

// Mutation stand for parked-task-assertion-contract-check.mjs. A green
// repository only proves the inverted assertion is absent today; this fixture
// proves the sweep still goes red when it is written again — and that it stays
// silent about the two spellings that are not the defect: the same line quoted
// in a doc comment (the helpers document the trap they replace, and prose is
// not a program) and the honest three-outcome match that replaced it.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'parked-task-assertion-contract-check.mjs');

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    cwd: fixture,
    env: { ...process.env, PLOMBIR_GIT_PARKED_TASK_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-parked-task-contract.'));
const empty = mkdtempSync(join(tmpdir(), 'plombir-git-parked-task-empty.'));

try {
  mkdirSync(join(fixture, 'crates/demo/src'), { recursive: true });

  // The spelling this gate exists to keep out, in both shapes it had: a
  // `JoinHandle` and a `oneshot::Receiver`.
  writeFileSync(
    join(fixture, 'crates/demo/src/inverted.rs'),
    `#[tokio::test]
async fn a_writer_stays_behind_the_lock() {
    let mut writer = tokio::spawn(async { write_it().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut writer)
            .await
            .is_err(),
        "the writer crossed the held lock"
    );
    let mut finished = finished_rx;
    assert!(tokio::time::timeout(WINDOW, &mut finished).await.is_err(), "it crossed");
}
`,
  );

  // A helper that replaces the defect has to be able to describe it. Prose is
  // not a program, and neither is a fixture somebody commented out.
  writeFileSync(
    join(fixture, 'crates/demo/src/documented.rs'),
    `/// \`assert!(timeout(window, &mut task).await.is_err())\` reads \`Err\` as
/// "still parked", which is wrong for a task that failed or panicked.
pub async fn assert_task_stays_blocked(task: &mut JoinHandle<()>, crossed: &str) {
    // assert!(tokio::time::timeout(WINDOW, &mut task).await.is_err(), "{crossed}");
    match tokio::time::timeout(WINDOW, task).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => panic!("{crossed} — finished with {outcome:?} instead of waiting"),
        Ok(Err(panic)) => panic!("{crossed} — panicked instead of waiting: {panic}"),
    }
}
`,
  );

  // The honest call site: a bare `timeout` on a borrowed handle whose outcome
  // is matched, not read through `is_err()`.
  writeFileSync(
    join(fixture, 'crates/demo/src/honest.rs'),
    `#[tokio::test]
async fn a_writer_stays_behind_the_lock() {
    let mut writer = tokio::spawn(async { write_it().await });
    assert_task_stays_blocked(&mut writer, "the writer crossed the held lock").await;
    let early = match tokio::time::timeout(WINDOW, &mut reader).await {
        Ok(result) => Some(result.expect("the reader task panicked")),
        Err(_) => None,
    };
    assert!(early.is_none() || early == Some(old), "a reader saw an uncommitted batch");
}
`,
  );

  const mutated = run(fixture);
  if (mutated.status === 0) {
    console.error(
      `❌ parked-task mutation passed: the contract check no longer rejects the defect:\n${mutated.output}`,
    );
    process.exit(1);
  }
  for (const expected of ['crates/demo/src/inverted.rs:4', 'crates/demo/src/inverted.rs:11']) {
    if (!mutated.output.includes(expected)) {
      console.error(`❌ parked-task mutation did not name ${expected}:\n${mutated.output}`);
      process.exit(1);
    }
  }
  for (const silent of ['crates/demo/src/documented.rs', 'crates/demo/src/honest.rs']) {
    if (mutated.output.includes(silent)) {
      console.error(
        `❌ parked-task swept ${silent}, which carries no assertion this gate is about:\n${mutated.output}`,
      );
      process.exit(1);
    }
  }

  // A sweep that reads nothing is not green, it is uninformed — the failure
  // mode every absence-based gate has.
  mkdirSync(join(empty, 'crates'), { recursive: true });
  const vacuous = run(empty);
  if (vacuous.status === 0) {
    console.error(`❌ parked-task check reported green over an empty tree:\n${vacuous.output}`);
    process.exit(1);
  }

  console.log(
    '✅ parked-task mutation: both inverted spellings are rejected, a documented and an honest '
      + 'one are not, and an empty tree cannot pass',
  );
} finally {
  rmSync(fixture, { recursive: true, force: true });
  rmSync(empty, { recursive: true, force: true });
}
