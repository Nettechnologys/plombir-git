#!/usr/bin/env node

// Exercise the real shared loader when the group leader exits before its
// stubborn helper. The OpenAPI fixture separately keeps BOTH alive until KILL.
// Process disappearance is checked with kill(pid, 0), including zombies.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { scratchDir, removeScratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sleep = ms => new Promise(done => setTimeout(done, ms));
function exists(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}
function require(condition, message) { if (!condition) throw new Error(message); }
async function gone(pid) {
  const deadline = Date.now() + 2_000;
  while (exists(pid) && Date.now() < deadline) await sleep(25);
  require(!exists(pid), `fixture process ${pid} survived teardown`);
}

for (const mode of ['leader-exits-on-term', 'leader-already-reaped']) {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-stand-process-'));
  const parent = join(fixture, 'parent.cjs');
  const helperPid = join(fixture, 'helper.pid');
  const leaderPid = join(fixture, 'leader.pid');
  const workspace = join(fixture, 'workspace');
  const helper = `
    process.on('SIGTERM', () => {});
    process.on('SIGINT', () => {});
    require('node:fs').writeFileSync(process.env.HELPER_PID, String(process.pid));
    setInterval(() => {}, 1000);
  `;
  writeFileSync(parent, `
    const child = require('node:child_process').spawn(process.execPath,
      ['-e', ${JSON.stringify(helper)}], { stdio: 'ignore' });
    process.on('SIGTERM', () => process.exit(0));
    setInterval(() => {}, 1000);
  `);
  const script = `
    set -euo pipefail
    source "$STAND_LIBRARY"
    stand_open
    printf '%s' "$STAND_WORK_DIR" > "$WORKSPACE_FILE"
    stand_spawn "$STAND_WORK_DIR/server.log" "$STAND_WORK_DIR" "$NODE_BIN" "$PARENT_FILE"
    printf '%s' "$STAND_LAST_PID" > "$LEADER_PID"
    for _ in $(seq 1 200); do
      [[ -s "$HELPER_PID" ]] && break
      sleep 0.025
    done
    [[ -s "$HELPER_PID" ]]
    own_group="$(ps -o pgid= -p $$ | tr -d ' ')"
    child_group="$(ps -o pgid= -p "$STAND_LAST_PID" | tr -d ' ')"
    [[ "$child_group" == "$STAND_LAST_PID" && "$child_group" != "$own_group" ]]
    if [[ "$MODE" == "leader-already-reaped" ]]; then
      kill -s TERM "$STAND_LAST_PID"
      wait "$STAND_LAST_PID"
      ! kill -0 "$STAND_LAST_PID" 2>/dev/null
      kill -0 -- "-$STAND_LAST_PID"
      printf '%s\\n' 'leader reaped; helper still owns the group'
    fi
    stand_cleanup
    stand_cleanup
  `;
  const child = spawn('bash', ['-c', script], {
    cwd: root,
    env: { ...process.env, STAND_LIBRARY: join(root, 'scripts/lib/stand.sh'),
      NODE_BIN: process.execPath, PARENT_FILE: parent, HELPER_PID: helperPid,
      LEADER_PID: leaderPid, WORKSPACE_FILE: workspace, MODE: mode,
      PLOMBIR_GIT_STAND_KEEP_TMP: '0' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '';
  child.stdout.on('data', bytes => { output += bytes; });
  child.stderr.on('data', bytes => { output += bytes; });
  let timer;
  const started = Date.now();
  const done = new Promise((accept, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => accept({ code, signal }));
  });
  try {
    const result = await Promise.race([done, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`stand timeout: ${output}`)), 15_000);
    })]);
    clearTimeout(timer);
    const elapsed = Date.now() - started;
    require(result.code === 0 && result.signal === null,
      `${mode} failed: ${JSON.stringify(result)}\n${output}`);
    if (mode === 'leader-already-reaped') {
      require(output.includes('leader reaped; helper still owns the group'), 'fixture missed the early exit');
    }
    for (const file of [leaderPid, helperPid]) {
      require(existsSync(file), `missing fixture PID ${file}`);
      await gone(Number(readFileSync(file, 'utf8')));
    }
    require(elapsed >= 4_500 && elapsed < 12_000,
      `${mode}: ${elapsed}ms did not follow bounded TERM → KILL teardown`);
    require(!existsSync(readFileSync(workspace, 'utf8')), 'stand left its workspace');
    console.log(`✅ stand process lifecycle: ${mode}, group/helpers gone in ${elapsed}ms`);
  } finally {
    clearTimeout(timer);
    // On a broken implementation, stop only the exact fixture processes.
    // Helper first lets a surviving parent reap it before the parent is killed.
    for (const file of [helperPid, leaderPid]) {
      if (!existsSync(file)) continue;
      const pid = Number(readFileSync(file, 'utf8'));
      if (Number.isInteger(pid) && pid > 0 && exists(pid)) {
        const state = spawnSync('ps', ['-o', 'pid=,ppid=,pgid=,stat=', '-p', String(pid)], { encoding: 'utf8' });
        console.error(`fixture cleanup: ${state.stdout.trim()}`);
        try { process.kill(pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
        await gone(pid);
      }
    }
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      await done;
    }
    if (existsSync(workspace)) removeScratchDir(readFileSync(workspace, 'utf8'));
    removeScratchDir(fixture);
  }
}
