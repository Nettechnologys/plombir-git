#!/usr/bin/env node

// Runs the cargo-free gates of `.github/workflows/regression.yml` locally.
//
// Why this exists: for its first ten weeks the workflow never executed a single
// step. 400 runs out of 400, from the first one on 2026-07-26 onward, were
// refused before a runner was ever assigned — GitHub's own check-run annotation
// on every job read "The job was not started because recent account payments
// have failed or your spending limit needs to be increased". So every gate
// declared in that file was enforced by nothing, and the push verifier mirrored
// two of the twelve jobs there were then (`cargo fmt`, `cargo clippy`). A gate
// nobody executes is a comment, and a silenced gate is indistinguishable by
// construction from one that keeps passing — which is why nobody noticed that
// green had never happened once.
//
// That ended when the repository went public: Actions runs every job since
// 2026-10-08, and the first runs of `rust` and `git-protocol` caught two real
// defects. The local mirror stays for what it is now — the gate that runs
// BEFORE a push rather than after it — and the jobs it cannot mirror are
// recorded as running in CI only, not "nowhere": a notice claiming CI never
// runs teaches the reader to ignore a red CI (card_6ed21f52b0aa).
//
// This runner covers the gates that need no Rust build, so it remains a single
// reusable target for the card verifier and pre-push fallback. Measured on a
// warm checkout: contract checks 1.2s, compose 0.2s, observability 1.6s,
// frontend 8.8s.
//
// It is deliberately ONE target rather than a list of commands inside the hook:
// a hook that spells out the commands drifts away from the workflow silently,
// which is the same defect one level down. `scripts/local-gate-coverage-
// contract-check.mjs` is the ratchet that keeps this file in step with the
// workflow — every one of its jobs must be accounted for below, and it
// fails when one is not, when an entry names a job that no longer exists, or
// when the verifier stops invoking a cargo command CARGO_JOBS says it invokes.

import { existsSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');

// Cargo-free jobs of regression.yml that this runner deliberately does NOT
// mirror, each with the reason. Read by the coverage contract check, which also
// fails on an entry naming a job that no longer exists — so this cannot become a
// place where a gate is quietly parked.
export const EXCLUDED = new Map([
  [
    'docker-image',
    'builds the release image: `[profile.release]` is fat-LTO with codegen-units=1, '
      + '~18 minutes and 243 MB measured cold. Nothing a pre-push hook may charge for.',
  ],
]);

// The cargo half of regression.yml, and where each job actually runs today.
// This runner does not execute any of them — a Rust build is not a pre-push
// budget — but the accounting has to live somewhere, because without it the
// cargo half is exactly the hand-written list this runner exists to replace,
// and nothing goes red when a fact in it changes.
//
// Each entry declares exactly one of three things, and the coverage contract
// check proves it rather than trusting it:
//   verifier:  a command `scripts/verify-push-gates.sh` must invoke. Delete the
//              line and the check goes red, so the gate cannot be dropped
//              quietly while the hook keeps accepting old workflow assumptions.
//   ciOnly:    the job runs in GitHub Actions on every push and pull request
//              and nowhere before it, with the reason it cannot be mirrored
//              locally. The check proves the workflow is triggered by push or
//              pull_request and the job is not switched off with an `if:`.
//   uncovered: this gate is enforced by NOTHING right now, with the reason.
//              The count is printed, so "covered" and "half covered" stop
//              looking identical.
export const CARGO_JOBS = new Map([
  ['fmt', { verifier: 'cargo fmt' }],
  ['clippy', { verifier: 'cargo clippy' }],
  ['docs', { verifier: 'cargo doc' }],
  [
    'rust',
    {
      ciOnly: 'the workspace test suite, doc-tests and the fresh-DB migration smoke; '
        + '25-30 minutes measured, far past a pre-push budget.',
    },
  ],
  [
    'security-audit',
    {
      ciOnly: '`cargo audit`, `cargo deny` and osv-scanner need three tools installed and an '
        + 'advisory-database fetch — a push must not depend on the network being up.',
    },
  ],
  [
    'git-protocol',
    {
      ciOnly: 'drives a real git client over HTTP and SSH against a release build of Plombir Git; '
        + 'needs the binary built and ports bound.',
    },
  ],
  [
    'ui-access-sweep',
    {
      ciOnly: 'drives every write control in a headless Chrome against a built Plombir Git and '
        + 'its built frontend; needs the binary, Chrome and ports bound.',
    },
  ],
  [
    'postgres-smoke',
    { ciOnly: 'needs a live PostgreSQL; the workflow gets one from a service container.' },
  ],
  [
    'mysql-smoke',
    { ciOnly: 'needs a live MySQL; the workflow gets one from a service container.' },
  ],
]);

// Ordered cheapest-first, so the fastest feedback lands first when several fail.
//
// `invokes` is the command the mirrored job must still run in the workflow, and
// it is the half of the mirror that used to be assumed. Accounting for a job by
// name says nothing about what the job does: commenting out the one line
// `run: node scripts/run-contract-checks.mjs` left the `contract-checks` job
// standing, still mirrored here, still reported as covered — and executing not
// one of the repository's contract checks (card_fad8ad0ef007). The coverage
// contract check proves each of these is still in the job's parsed run bodies,
// so a mirror can no longer claim to cover a job that stopped doing the work.
export const GATES = [
  {
    job: 'contract-checks',
    name: 'Frontend/backend contract checks',
    run: runContractChecks,
    invokes: 'node scripts/run-contract-checks.mjs',
  },
  {
    job: 'deploy-config',
    name: 'Docker compose config',
    run: runDeployConfig,
    invokes: 'docker compose',
  },
  // The second half of `deploy-config`: the reverse-proxy examples an operator
  // copies out of deploy/, loaded by nginx and Caddy themselves through the
  // one script the workflow step runs too (card_b7bfdbf0b00d).
  {
    job: 'deploy-config',
    name: 'Reverse-proxy examples',
    run: runReverseProxyExamples,
    invokes: 'bash scripts/check-reverse-proxy-examples.sh',
  },
  {
    job: 'observability-config',
    name: 'Prometheus, Alertmanager and Grafana config',
    run: runObservability,
    invokes: 'promtool check',
  },
  {
    job: 'frontend',
    name: 'Frontend check and build',
    run: runFrontend,
    invokes: 'npm test',
  },
];

// A gate that cannot run is NOT a gate that passed. Every helper below returns
// either ok, or a reason — a missing tool is reported red exactly like a failed
// assertion, because "docker is not installed" and "the config is valid" must
// never produce the same exit code. `git push --no-verify` stays the documented
// escape hatch for the case where that is genuinely not what you want.
function requireTool(tool, why) {
  const probe = spawnSync(tool, ['--version'], { encoding: 'utf8' });
  if (probe.error) return `\`${tool}\` is not on PATH — ${why}`;
  return null;
}

function sh(command, { cwd = root, env } = {}) {
  return spawnSync('bash', ['-euo', 'pipefail', '-c', command], {
    cwd,
    encoding: 'utf8',
    env: { ...process.env, ...env },
  });
}

function fromResult(result) {
  if (result.error) return { ok: false, output: String(result.error.message) };
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`.trimEnd();
  if (result.signal) return { ok: false, output: `${output}\nkilled by ${result.signal}` };
  return { ok: result.status === 0, output };
}

function runContractChecks() {
  const missing = requireTool('node', 'the contract checks are Node reading repository sources');
  if (missing) return { ok: false, output: missing };
  // The runner globs scripts/*-contract-check.mjs itself and fails on an empty
  // glob, so there is no check list to keep in sync here.
  return fromResult(
    spawnSync(process.execPath, [join(scriptsDir, 'run-contract-checks.mjs')], {
      cwd: root,
      encoding: 'utf8',
    }),
  );
}

export function runDeployConfig({ cwd = root } = {}) {
  const missing = requireTool('docker', '`docker compose config` is what validates the compose files');
  if (missing) return { ok: false, output: missing };

  // Mirrors the workflow step, including its ratchet: an empty glob fails loudly
  // rather than passing a loop over nothing. Both app compose files require an
  // env_file, and the main compose reads PLOMBIR_GIT_JWT_SECRET out of it.
  //
  // A missing deploy/.env used to be copied into that shared repository path
  // and removed by a shell trap. Two concurrent pre-push hooks could therefore
  // mistake one another's throwaway file for a developer-owned file, then lose
  // it while docker compose was still reading it. Keep real user state read-only
  // and give every invocation its own env file outside the checkout instead.
  const repoRoot = resolve(cwd);
  const userEnv = join(repoRoot, 'deploy', '.env');
  let temporaryRoot;
  let envFile = userEnv;
  let preface = 'deploy/.env exists; validating against it without modifying it.';

  if (!existsSync(userEnv)) {
    try {
      temporaryRoot = scratchDir(join(tmpdir(), 'plombir-git-deploy-config-'));
      envFile = join(temporaryRoot, '.env');
      const examplePath = join(repoRoot, 'deploy', '.env.example');
      const example = readFileSync(examplePath, 'utf8');
      const secretLine = /^PLOMBIR_GIT_JWT_SECRET=.*$/m;
      if (!secretLine.test(example)) {
        throw new Error(`${examplePath} no longer declares PLOMBIR_GIT_JWT_SECRET`);
      }
      // The observability stack refuses to interpolate without a Grafana admin
      // password (no admin/admin fallback), and `.env.example` ships it empty.
      const grafanaLine = /^GRAFANA_ADMIN_PASSWORD=.*$/m;
      if (!grafanaLine.test(example)) {
        throw new Error(`${examplePath} no longer declares GRAFANA_ADMIN_PASSWORD`);
      }
      writeFileSync(
        envFile,
        example
          .replace(secretLine, `PLOMBIR_GIT_JWT_SECRET=${randomBytes(32).toString('hex')}`)
          .replace(grafanaLine, `GRAFANA_ADMIN_PASSWORD=${randomBytes(16).toString('hex')}`),
        { mode: 0o600 },
      );
      preface = 'deploy/.env is absent; validating with an isolated temporary env file.';
    } catch (error) {
      if (temporaryRoot) rmSync(temporaryRoot, { recursive: true, force: true });
      return { ok: false, output: `Could not prepare an isolated compose env file: ${error.message}` };
    }
  }

  let gate = fromResult(sh(`
    shopt -s nullglob
    composes=(deploy/docker-compose*.yml)
    if [ "\${#composes[@]}" -eq 0 ]; then
      echo "No deploy/docker-compose*.yml found — the glob no longer matches anything." >&2
      exit 1
    fi
    for compose in "\${composes[@]}"; do
      echo "Validating \${compose}"
      docker compose --env-file "\${PLOMBIR_GIT_DEPLOY_ENV_FILE}" -f "\${compose}" config >/dev/null
    done
  `, {
    cwd: repoRoot,
    env: { PLOMBIR_GIT_DEPLOY_ENV_FILE: envFile },
  }));


  if (temporaryRoot) {
    try {
      rmSync(temporaryRoot, { recursive: true, force: true });
    } catch (error) {
      gate = {
        ok: false,
        output: `${gate.output}\nFailed to remove isolated compose env directory: ${error.message}`.trim(),
      };
    }
  }

  return { ...gate, output: `${preface}\n${gate.output}`.trim() };
}

export function runReverseProxyExamples({ cwd = root } = {}) {
  const missing = requireTool('docker', 'nginx -t and caddy validate run inside the official images')
    || requireTool('openssl', 'nginx -t loads the certificate the example names, so one is made up for it');
  if (missing) return { ok: false, output: missing };
  return fromResult(sh('bash scripts/check-reverse-proxy-examples.sh', { cwd: resolve(cwd) }));
}

export function runObservability({ cwd = root } = {}) {
  const repoRoot = resolve(cwd);
  const provisioning = fromResult(
    spawnSync(
      process.execPath,
      [join(scriptsDir, 'grafana-provisioning-contract-check.mjs'), repoRoot],
      { cwd: repoRoot, encoding: 'utf8' },
    ),
  );
  if (!provisioning.ok) return provisioning;

  const missing = requireTool('docker', 'promtool and amtool ship inside the official images and nowhere else');
  if (missing) return { ok: false, output: `${provisioning.output}\n${missing}`.trim() };

  // Image tags are read out of the compose file rather than pinned a second time
  // here, for the reason the workflow gives: a duplicate pin would validate the
  // config with a promtool that is not the one loading it the moment somebody
  // bumps the stack. The read itself is the workflow's own step — one parse of
  // the document, not a grep over its bytes, which used to let a commented-out
  // pin above the live one win by `head -1` in both copies of this shell
  // (card_f0fbdd88a68b). The same script proves every bind-mounted file exists.
  const read = spawnSync(
    process.execPath,
    [join(scriptsDir, 'observability-compose-contract-check.mjs'), repoRoot],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  const compose = fromResult(read);
  if (!compose.ok) return { ok: false, output: `${provisioning.output}\n${compose.output}`.trim() };

  // stdout is the machine half: one `<service>=<image>` line per image the gate
  // has to run. A line this loop cannot read is the contract changing under the
  // mirror, so it is refused rather than passed to `docker run` as an empty tag.
  const images = {};
  for (const line of (read.stdout ?? '').split('\n').filter((entry) => entry.trim() !== '')) {
    const separator = line.indexOf('=');
    if (separator <= 0) {
      return {
        ok: false,
        output: `observability-compose-contract-check.mjs printed ${JSON.stringify(line)}, not \`service=image\`.`,
      };
    }
    images[line.slice(0, separator)] = line.slice(separator + 1);
  }
  for (const service of ['prometheus', 'alertmanager']) {
    if (!images[service]) {
      return { ok: false, output: `observability-compose-contract-check.mjs printed no image for ${service}.` };
    }
  }

  const gate = fromResult(sh(`
    shopt -s nullglob

    configs=(deploy/prometheus/prometheus*.yml)
    rules=(deploy/prometheus/alerts*.yml)
    if [ "\${#configs[@]}" -eq 0 ] || [ "\${#rules[@]}" -eq 0 ]; then
      echo "deploy/prometheus/ no longer holds a prometheus*.yml and an alerts*.yml." >&2
      exit 1
    fi
    for config in "\${configs[@]}"; do
      docker run --rm -v "\${PWD}/deploy/prometheus:/cfg:ro" \\
        --entrypoint promtool "\${IMAGE_PROMETHEUS}" check config "/cfg/$(basename "\${config}")"
    done
    for rule in "\${rules[@]}"; do
      docker run --rm -v "\${PWD}/deploy/prometheus:/cfg:ro" \\
        --entrypoint promtool "\${IMAGE_PROMETHEUS}" check rules "/cfg/$(basename "\${rule}")"
    done

    amconfigs=(deploy/alertmanager/*.yml)
    if [ "\${#amconfigs[@]}" -eq 0 ]; then
      echo "deploy/alertmanager/ no longer holds a *.yml." >&2
      exit 1
    fi
    for config in "\${amconfigs[@]}"; do
      docker run --rm -v "\${PWD}/deploy/alertmanager:/cfg:ro" \\
        --entrypoint amtool "\${IMAGE_ALERTMANAGER}" check-config "/cfg/$(basename "\${config}")"
    done

    dashboards=(deploy/grafana/dashboards/*.json)
    if [ "\${#dashboards[@]}" -eq 0 ]; then
      echo "deploy/grafana/dashboards/ no longer holds a *.json." >&2
      exit 1
    fi
    for dashboard in "\${dashboards[@]}"; do
      echo "Parsing \${dashboard}"
      node -e 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))' "\${dashboard}"
    done
  `, {
    cwd: repoRoot,
    env: { IMAGE_PROMETHEUS: images.prometheus, IMAGE_ALERTMANAGER: images.alertmanager },
  }));
  return {
    ...gate,
    output: `${provisioning.output}\n${compose.output}\n${gate.output}`.trim(),
  };
}

function runFrontend() {
  const missing = requireTool('npm', 'the frontend gate is svelte-check, vitest and the production build');
  if (missing) return { ok: false, output: missing };

  const web = join(root, 'web');
  const lock = join(web, 'package-lock.json');
  const installed = join(web, 'node_modules', '.package-lock.json');

  // CI runs `npm ci`, so it always tests exactly what the lockfile pins. Locally
  // `npm ci` would delete a developer's node_modules on every push, so this runs
  // against what is installed — and refuses to run against a tree that no longer
  // matches the lockfile, instead of quietly gating on the wrong dependencies.
  if (!existsSync(installed)) {
    return { ok: false, output: 'web/node_modules is missing or was not installed from the lockfile — run `npm ci` in web/.' };
  }
  if (statSync(lock).mtimeMs > statSync(installed).mtimeMs) {
    return { ok: false, output: 'web/package-lock.json is newer than web/node_modules — run `npm ci` in web/ so this gate tests what the lockfile pins.' };
  }

  // `npm test` is vitest under jsdom; the suite that matters is the markdown
  // sanitizer, the boundary between user-supplied markdown and the DOM.
  return fromResult(sh('npm run check && npm test && npm run build', { cwd: web }));
}

function main() {
  const stale = [...EXCLUDED.keys()].filter((job) => GATES.some((gate) => gate.job === job));
  if (stale.length > 0) {
    console.error(`❌ ${stale.join(', ')} appear in both GATES and EXCLUDED — one of the two is wrong.`);
    process.exit(1);
  }

  const failures = [];
  for (const gate of GATES) {
    process.stdout.write(`▶ ${gate.name} (${gate.job})\n`);
    const started = Date.now();
    const { ok, output } = gate.run();
    const seconds = ((Date.now() - started) / 1000).toFixed(1);
    if (ok) {
      console.log(`✅ ${gate.job} — ${seconds}s`);
    } else {
      console.log(`❌ ${gate.job} — ${seconds}s`);
      failures.push({ gate, output });
    }
  }

  for (const { gate, output } of failures) {
    console.error(`\n----- ${gate.job} (${gate.name}) -----\n${output}`);
  }

  const summary = `${GATES.length - failures.length}/${GATES.length} local gates green`
    + (EXCLUDED.size > 0 ? `, ${EXCLUDED.size} cargo-free job(s) excluded by design` : '')
    + (failures.length > 0 ? `, ${failures.length} FAILED` : '');
  console.log(`\n${failures.length === 0 ? '✅' : '❌'} ${summary}`);

  // Said out loud on every verification: a green run here covers the cargo-free
  // half and the verifier's cargo commands. The rest of regression.yml is
  // enforced after the push, by CI — a red CI there is a real failure, not
  // noise — and anything enforced by nothing is named as such.
  const jobsClaiming = (claim) => [...CARGO_JOBS].filter(([, where]) => where[claim]).map(([job]) => job);
  const ciOnly = jobsClaiming('ciOnly');
  if (ciOnly.length > 0) {
    console.log(
      `ℹ️  ${ciOnly.length} gate(s) of regression.yml run in CI only, after the push — ${ciOnly.join(', ')}. `
        + 'Check the Actions run of the pushed commit.',
    );
  }
  const uncovered = jobsClaiming('uncovered');
  if (uncovered.length > 0) {
    console.log(`⚠️  ${uncovered.length} gate(s) of regression.yml run nowhere — ${uncovered.join(', ')}.`);
  }

  process.exit(failures.length === 0 ? 0 : 1);
}

// Importable by the coverage contract check without executing the gates.
if (process.argv[1] === fileURLToPath(import.meta.url)) main();
