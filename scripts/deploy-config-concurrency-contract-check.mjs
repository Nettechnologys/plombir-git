#!/usr/bin/env node

// Behavioral regression for the local deploy-config gate. The real compose
// files are copied into a private fixture and a fake docker process holds two
// gate invocations at the same point. This proves both invocations own distinct
// env files without requiring a Docker daemon in the cheap contract-check gate.

import { spawn } from 'node:child_process';
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionYamlSource } from './lib/yaml-source.mjs';
import { runDeployConfig } from './run-local-gates.mjs';

const thisFile = fileURLToPath(import.meta.url);
const root = resolve(dirname(thisFile), '..');

if (process.argv[2] === '--worker') {
  const result = runDeployConfig({ cwd: process.argv[3] });
  if (result.output) console.log(result.output);
  process.exit(result.ok ? 0 : 1);
}

function worker(fixture, env) {
  return new Promise((resolveWorker, rejectWorker) => {
    const child = spawn(process.execPath, [thisFile, '--worker', fixture], {
      cwd: root,
      env: { ...process.env, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });
    child.on('error', rejectWorker);
    child.on('close', (status, signal) => resolveWorker({ status, signal, output }));
  });
}

function requireSuccess(name, result) {
  if (result.status !== 0 || result.signal) {
    throw new Error(
      `${name}: expected exit 0, got status=${result.status} signal=${result.signal}\n${result.output}`,
    );
  }
}

const fixture = scratchDir(join(tmpdir(), 'plombir-git-deploy-config-contract-'));

try {
  const deploy = join(fixture, 'deploy');
  const fakeBin = join(fixture, 'fake-bin');
  const barrier = join(fixture, 'barrier');
  mkdirSync(deploy);
  mkdirSync(fakeBin);
  mkdirSync(barrier);

  for (const file of [
    '.env.example',
    'docker-compose.yml',
    'docker-compose.hostdir.yml',
    'docker-compose.observability.yml',
  ]) {
    cpSync(join(root, 'deploy', file), join(deploy, file));
  }

  for (const file of ['docker-compose.yml', 'docker-compose.hostdir.yml']) {
    // Through the production view, not the bytes: a commented-out volume entry
    // satisfies a raw `includes` while compose reads nothing of the sort, so
    // this precondition would hold over a file that can no longer take the
    // gate's isolated env file at all.
    const compose = productionYamlSource(readFileSync(join(deploy, file), 'utf8'));
    if (!compose.includes('- ${PLOMBIR_GIT_DEPLOY_ENV_FILE:-.env}')) {
      throw new Error(`${file} no longer lets the gate supply its isolated env file`);
    }
  }

  writeFileSync(
    join(fakeBin, 'docker'),
    `#!/usr/bin/env bash
set -euo pipefail

if [[ "\${1:-}" == "--version" ]]; then
  exit 0
fi

[[ "\${1:-}" == "compose" ]]
shift
env_file=""
while [[ \$# -gt 0 ]]; do
  case "\$1" in
    --env-file)
      env_file="\$2"
      shift 2
      ;;
    -f)
      shift 2
      ;;
    config)
      shift
      ;;
    *)
      shift
      ;;
  esac
done

[[ -n "\${env_file}" ]]
[[ "\${env_file}" == "\${PLOMBIR_GIT_DEPLOY_ENV_FILE}" ]]
[[ -f "\${env_file}" ]]

if [[ -n "\${PLOMBIR_GIT_TEST_EXPECT_ENV:-}" ]]; then
  [[ "\${env_file}" == "\${PLOMBIR_GIT_TEST_EXPECT_ENV}" ]]
fi

if [[ -n "\${PLOMBIR_GIT_TEST_BARRIER:-}" ]]; then
  marker="\${PLOMBIR_GIT_TEST_BARRIER}/\$(basename "\$(dirname "\${env_file}")")"
  printf '%s\\n' "\${env_file}" >"\${marker}"
  count=0
  for _ in {1..200}; do
    count="\$(find "\${PLOMBIR_GIT_TEST_BARRIER}" -type f | wc -l)"
    [[ "\${count}" -ge 2 ]] && break
    sleep 0.01
  done
  [[ "\${count}" -ge 2 ]]
  [[ -f "\${env_file}" ]]
fi
`,
    { mode: 0o755 },
  );

  const env = {
    PATH: `${fakeBin}:${process.env.PATH}`,
    PLOMBIR_GIT_TEST_BARRIER: barrier,
  };
  const parallel = await Promise.all([worker(fixture, env), worker(fixture, env)]);
  parallel.forEach((result, index) => requireSuccess(`parallel worker ${index + 1}`, result));

  if (existsSync(join(deploy, '.env'))) {
    throw new Error('parallel gates created deploy/.env inside the fixture checkout');
  }

  const markers = readdirSync(barrier);
  if (markers.length !== 2) {
    throw new Error(`parallel gates used ${markers.length} isolated env paths instead of 2`);
  }
  const isolatedEnvFiles = markers.map((marker) => readFileSync(join(barrier, marker), 'utf8').trim());
  if (new Set(isolatedEnvFiles).size !== 2) {
    throw new Error(`parallel gates shared one env path: ${isolatedEnvFiles.join(', ')}`);
  }
  for (const envFile of isolatedEnvFiles) {
    if (existsSync(envFile)) throw new Error(`temporary env file survived gate cleanup: ${envFile}`);
  }

  const userEnv = join(deploy, '.env');
  const sentinel = 'PLOMBIR_GIT_JWT_SECRET=user-owned-sentinel\n';
  writeFileSync(userEnv, sentinel, { mode: 0o600 });
  const existing = await worker(fixture, {
    PATH: `${fakeBin}:${process.env.PATH}`,
    PLOMBIR_GIT_TEST_EXPECT_ENV: userEnv,
  });
  requireSuccess('existing user env worker', existing);
  if (readFileSync(userEnv, 'utf8') !== sentinel) {
    throw new Error('deploy-config changed or removed the existing user env file');
  }

  console.log('✅ deploy-config uses per-run env files and preserves an existing deploy/.env');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
