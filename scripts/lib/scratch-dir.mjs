// Scratch directories a script must not leave behind.
//
// A regression stand copies the tree into $TMPDIR once per fixture and removes
// the copy in `finally`. Node runs no `finally` on `process.exit()` and nothing
// at all on a signal, so every red run — the stand exits from inside its `try`
// — and every Ctrl-C left a full copy behind, `crates/` and `web/src`
// included (card_40a2c3c4254b). A stand is rerun exactly while it is red, so
// the copies piled up.
//
// Registering the directory here moves its removal to the one place Node does
// run on the way out: the `exit` event, which `process.exit()` reaches and the
// signal handlers below route to. A stand keeps its own `finally` for the
// ordinary path; this is the floor under it.
//
// Children are the second half. A stand that runs its fixtures side by side
// (`fixture-pool.mjs`) has one `node` per fixture in flight; a SIGTERM aimed
// at the stand alone used to leave them running against directories that no
// longer existed. `adoptChild` hands them to the same exit path.

import { mkdtempSync, rmSync } from 'node:fs';

const directories = new Set();
const children = new Set();
let armed = false;

// The conventional `128 + n` status, so a caller can tell an interrupted run
// from a red one.
const SIGNAL_EXIT_CODES = { SIGHUP: 129, SIGINT: 130, SIGTERM: 143 };

function arm() {
  if (armed) return;
  armed = true;
  process.on('exit', () => {
    for (const child of children) child.kill('SIGKILL');
    for (const directory of directories) rmSync(directory, { recursive: true, force: true });
  });
  for (const [signal, code] of Object.entries(SIGNAL_EXIT_CODES)) {
    process.on(signal, () => process.exit(code));
  }
}

/** `mkdtempSync(prefix)`, removed again however the process ends. */
export function scratchDir(prefix) {
  arm();
  const directory = mkdtempSync(prefix);
  directories.add(directory);
  return directory;
}

/** Remove a scratch directory now; the exit path then has nothing left to do. */
export function removeScratchDir(directory) {
  rmSync(directory, { recursive: true, force: true });
  directories.delete(directory);
}

/** Kill `child` on the way out if it is still running then. */
export function adoptChild(child) {
  arm();
  children.add(child);
  child.once('exit', () => children.delete(child));
  return child;
}
