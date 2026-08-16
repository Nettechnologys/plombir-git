#!/usr/bin/env node

// Mutation stand for released-port-contract-check.mjs. A green repository only
// proves the forbidden spellings are absent; this fixture proves the sweep
// still goes red when both the Rust and shell variants are reintroduced.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-released-port-contract.'));

try {
  mkdirSync(join(fixture, 'crates/demo/src'), { recursive: true });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });

  writeFileSync(
    join(fixture, 'crates/demo/src/lib.rs'),
    `async fn dead_server_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}
`,
  );
  writeFileSync(
    join(fixture, 'scripts/free-port.sh'),
    `python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
`,
  );

  const result = spawnSync(process.execPath, [join(scriptsDir, 'released-port-contract-check.mjs')], {
    cwd: fixture,
    env: { ...process.env, FORGEKEEP_RELEASED_PORT_ROOT: fixture },
    encoding: 'utf8',
  });
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;

  if (result.status === 0) {
    console.error('❌ released-port mutation passed: the contract check no longer rejects the defect');
    process.exit(1);
  }
  for (const expected of ['crates/demo/src/lib.rs:4', 'scripts/free-port.sh:3']) {
    if (!output.includes(expected)) {
      console.error(`❌ released-port mutation did not name ${expected}:\n${output}`);
      process.exit(1);
    }
  }

  console.log('✅ released-port mutation: Rust drop and shell getsockname probes are both rejected');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
