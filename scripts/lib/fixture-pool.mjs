// Runs a regression stand's fixtures side by side.
//
// A stand proves a gate still bites by copying the tree, putting one defect
// back and running the real check against the copy — once per defect. Every
// fixture is its own directory and its own process, so nothing orders them but
// the loop that used to run them one after another: the UI inventory stand
// spent three and a half minutes of every push gate waiting on fifty-seven
// independent child processes in single file, on runners with four cores.
//
// The pool keeps what made the sequential loop trustworthy: every case runs to
// a verdict, a case that throws while building its fixture is a failure rather
// than a crash that hides the rest, and results come back in the order the
// cases were written, so the log reads the same however the scheduler ran them.

import { spawn } from 'node:child_process';
import { availableParallelism } from 'node:os';

import { adoptChild } from './scratch-dir.mjs';

/**
 * Run `node <args>` and collect its exit status and combined output.
 *
 * The asynchronous twin of the `spawnSync(process.execPath, …)` the stands
 * used. A child still running at `timeout` milliseconds is killed and reported
 * by its signal — never as a pass.
 */
export function runNode(args, { cwd, env, timeout = 120_000 } = {}) {
  return new Promise((resolve) => {
    // Adopted, so a stand interrupted mid-pool takes its fixtures' processes
    // with it instead of leaving them running against removed directories.
    const child = adoptChild(
      spawn(process.execPath, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] }),
    );
    let output = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });
    const timer = setTimeout(() => child.kill('SIGKILL'), timeout);
    child.on('error', (error) => {
      clearTimeout(timer);
      resolve({ status: null, signal: null, output: `${output}${error.message}` });
    });
    child.on('close', (status, signal) => {
      clearTimeout(timer);
      resolve({ status, signal, output });
    });
  });
}

/**
 * Map `run` over `cases` with at most one case per available CPU in flight.
 *
 * Resolves to one `{ value }` or `{ error }` per case, in input order. A case
 * that throws is reported, never swallowed, and never stops the others.
 */
export async function runFixtures(cases, run, { concurrency = availableParallelism() } = {}) {
  const results = new Array(cases.length);
  let next = 0;
  const worker = async () => {
    while (next < cases.length) {
      const index = next;
      next += 1;
      try {
        results[index] = { value: await run(cases[index], index) };
      } catch (error) {
        results[index] = { error };
      }
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, cases.length)) }, worker));
  return results;
}
