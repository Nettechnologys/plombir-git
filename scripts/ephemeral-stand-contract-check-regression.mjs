#!/usr/bin/env node

// Mutation stand for ephemeral-stand-contract-check.mjs. A green repository
// only proves the wiring is present today; this fixture proves the check still
// goes red for each way the stand rots — a second boot sequence, a preview
// server without the proxy, a preview proxy that has drifted from the dev one,
// a teardown that stops removing its workspace — and that required wiring which
// survives only in a whole-line or inline comment reads as absent. Valid `#`
// shell data and `//` inside JavaScript strings remain live source.

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const CHECK = join(scriptsDir, 'ephemeral-stand-contract-check.mjs');

function baseline(fixture) {
  mkdirSync(join(fixture, 'scripts/lib'), { recursive: true });
  mkdirSync(join(fixture, 'web'), { recursive: true });
  for (const file of [
    'scripts/lib/stand.sh',
    'scripts/ephemeral-stand.sh',
    'scripts/git-protocol-e2e.sh',
    // The hourly wrapper is a named regression target: it must keep delegating
    // instead of quietly regrowing the third boot sequence fixed by
    // card_91a376a406db.
    'scripts/codex-hourly-automation.mjs',
    'web/vite.config.ts',
  ]) {
    cpSync(join(root, file), join(fixture, file));
  }
}

function runCheck(fixture) {
  const result = spawnSync(process.execPath, [CHECK], {
    cwd: fixture,
    env: { ...process.env, FORGEKEEP_EPHEMERAL_STAND_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

function patch(fixture, file, from, to) {
  const path = join(fixture, file);
  const source = readFileSync(path, 'utf8');
  if (!source.includes(from)) {
    console.error(`❌ ephemeral-stand mutation could not find its anchor in ${file}: ${from}`);
    process.exit(1);
  }
  writeFileSync(path, source.replace(from, to));
}

const MUTATIONS = [
  {
    name: 'the preview server loses its proxy',
    apply: (fixture) => patch(fixture, 'web/vite.config.ts', '\tpreview: { proxy }\n', ''),
    expect: 'declares no preview proxy',
  },
  {
    name: 'the preview proxy drifts away from the dev proxy',
    apply: (fixture) =>
      patch(
        fixture,
        'web/vite.config.ts',
        '\tpreview: { proxy }\n',
        "\tpreview: { proxy: { '/api/v1': { target: backendOrigin, ws: true } } }\n",
      ),
    expect: 'the dev proxy forwards',
  },
  {
    name: 'the backend origin is hard-coded instead of read from the environment',
    apply: (fixture) =>
      patch(
        fixture,
        'web/vite.config.ts',
        "process.env.FORGEKEEP_BACKEND_ORIGIN || 'http://127.0.0.1:8080'",
        "'http://127.0.0.1:8080'",
      ),
    expect: 'does not read FORGEKEEP_BACKEND_ORIGIN',
  },
  {
    name: 'a second script boots its own server',
    apply: (fixture) =>
      writeFileSync(
        join(fixture, 'scripts/browser-e2e.sh'),
        '#!/usr/bin/env bash\n"${FORGEKEEP_BIN}" serve --listen-address-file "${WORK_DIR}/addrs" &\n',
      ),
    expect: 'scripts/browser-e2e.sh starts its own ForgeKeep',
  },
  {
    name: 'the hourly wrapper regrows its own server boot',
    apply: (fixture) => {
      const file = join(fixture, 'scripts/codex-hourly-automation.mjs');
      writeFileSync(
        file,
        `${readFileSync(file, 'utf8')}\nspawn('forgekeep', ['serve', '--listen-address-file', 'fixture']);\n`,
      );
    },
    expect: 'scripts/codex-hourly-automation.mjs starts its own ForgeKeep',
  },
  {
    name: 'teardown stops removing the temporary workspace',
    apply: (fixture) =>
      patch(fixture, 'scripts/lib/stand.sh', 'rm -rf "${STAND_WORK_DIR}"', ': "${STAND_WORK_DIR}"'),
    expect: 'remove the temporary workspace',
  },
  {
    name: 'the teardown trap is only commented out',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        "  trap 'stand_trap' EXIT INT TERM",
        "  # trap 'stand_trap' EXIT INT TERM",
      ),
    expect: 'does not install a teardown trap',
  },
  {
    name: 'the teardown trap survives only in an inline comment',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        "  trap 'stand_trap' EXIT INT TERM",
        "  true # trap 'stand_trap' EXIT INT TERM",
      ),
    expect: 'does not install a teardown trap',
  },
  {
    name: 'the spawned PID registration survives only in an inline comment',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        '  STAND_PIDS+=("${STAND_LAST_PID}")',
        '  true # STAND_PIDS+=("${STAND_LAST_PID}")',
      ),
    expect: 'register the child it started for teardown',
  },
  {
    name: 'the entry point names the shared loader only in an inline comment',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/ephemeral-stand.sh',
        'source "${ROOT_DIR}/scripts/lib/stand.sh"',
        'true # source "${ROOT_DIR}/scripts/lib/stand.sh"',
      ),
    expect: 'does not source scripts/lib/stand.sh',
  },
  {
    name: 'the websocket upgrade is dropped from the proxy',
    apply: (fixture) => patch(fixture, 'web/vite.config.ts', ', ws: true', ''),
    expect: 'does not enable websocket proxying',
  },
];

const VALID_VARIANTS = [
  {
    name: 'a quoted shell hash before live PID registration',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        '  STAND_PIDS+=("${STAND_LAST_PID}")',
        "  printf '%s' '# pid follows' >/dev/null; STAND_PIDS+=(\"${STAND_LAST_PID}\")",
      ),
  },
  {
    name: 'an assignment hash before live PID registration',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        '  STAND_PIDS+=("${STAND_LAST_PID}")',
        '  STAND_NOTE=pid#follows; STAND_PIDS+=("${STAND_LAST_PID}")',
      ),
  },
  {
    name: 'a parameter-expansion hash before live PID registration',
    apply: (fixture) =>
      patch(
        fixture,
        'scripts/lib/stand.sh',
        '  STAND_PIDS+=("${STAND_LAST_PID}")',
        '  : "${STAND_NOTE:-#pid-follows}"; STAND_PIDS+=("${STAND_LAST_PID}")',
      ),
  },
  {
    name: 'a JavaScript URL string followed by an inline boot decoy',
    apply: (fixture) =>
      writeFileSync(
        join(fixture, 'scripts/source-view-positive.mjs'),
        "const healthUrl = 'https://stand.invalid/health';\nvoid healthUrl; // spawn('forgekeep', ['serve', '--listen-address-file']);\n",
      ),
  },
];

let fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ephemeral-stand-contract.'));
try {
  baseline(fixture);
  const clean = runCheck(fixture);
  if (clean.status !== 0) {
    console.error(`❌ ephemeral-stand baseline fixture is already red, so no mutation below proves anything:\n${clean.output}`);
    process.exit(1);
  }

  for (const mutation of MUTATIONS) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ephemeral-stand-contract.'));
    baseline(fixture);
    mutation.apply(fixture);

    const result = runCheck(fixture);
    if (result.status === 0) {
      console.error(`❌ ephemeral-stand mutation passed — the check no longer notices when ${mutation.name}`);
      process.exit(1);
    }
    if (!result.output.includes(mutation.expect)) {
      console.error(
        `❌ ephemeral-stand mutation "${mutation.name}" went red for the wrong reason ` +
          `(expected the message to mention "${mutation.expect}"):\n${result.output}`,
      );
      process.exit(1);
    }
  }

  for (const variant of VALID_VARIANTS) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ephemeral-stand-contract.'));
    baseline(fixture);
    variant.apply(fixture);

    const result = runCheck(fixture);
    if (result.status !== 0) {
      console.error(
        `❌ ephemeral-stand valid variant "${variant.name}" went red even though its wiring remains live:\n${result.output}`,
      );
      process.exit(1);
    }
  }

  console.log(
    `✅ ephemeral stand mutation: all ${MUTATIONS.length} rots are rejected, and ` +
      `${VALID_VARIANTS.length} syntax-preserving variants stay green`,
  );
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
