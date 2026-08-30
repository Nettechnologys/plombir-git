#!/usr/bin/env node

// Runs the cargo-free gates of `.github/workflows/regression.yml` locally.
//
// Why this exists: the workflow has never executed a single step. 400 runs out
// of 400, from the first one on 2026-07-26 onward, were refused before a runner
// was ever assigned — GitHub's own check-run annotation on every job reads "The
// job was not started because recent account payments have failed or your
// spending limit needs to be increased". So every gate declared in that file was
// enforced by nothing, and the push verifier mirrored two of the twelve
// (`cargo fmt`, `cargo clippy`). A gate nobody executes is a comment, and a
// silenced gate is indistinguishable by construction from one that keeps
// passing — which is why nobody noticed that green had never happened once.
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
// workflow — every one of the twelve jobs must be accounted for below, and it
// fails when one is not, when an entry names a job that no longer exists, or
// when the verifier stops invoking a cargo command CARGO_JOBS says it invokes.

import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
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
// cargo half is exactly the hand-written list this runner exists to replace:
// the card verifier names two cargo commands, five jobs run nowhere at all,
// and nothing goes red when either fact changes.
//
// Each entry declares one of two things, and the coverage contract check proves
// it rather than trusting it:
//   verifier:  a command `scripts/verify-push-gates.sh` must invoke. Delete the
//              line and the check goes red, so the gate cannot be dropped
//              quietly while the hook keeps accepting old workflow assumptions.
//   uncovered: this gate is enforced by NOTHING right now, with the reason it
//              cannot be mirrored before a push. The count is printed, so
//              "covered" and "half covered" stop looking identical.
export const CARGO_JOBS = new Map([
  ['fmt', { verifier: 'cargo fmt' }],
  ['clippy', { verifier: 'cargo clippy' }],
  [
    'rust',
    {
      uncovered: 'the workspace test suite, doc-tests and the fresh-DB migration smoke; '
        + '25-30 minutes measured, and `cargo test -p rg-http` is not yet green on HEAD anyway.',
    },
  ],
  [
    'security-audit',
    {
      uncovered: '`cargo audit`, `cargo deny` and osv-scanner need three tools installed and an '
        + 'advisory-database fetch — a push must not depend on the network being up.',
    },
  ],
  [
    'git-protocol',
    {
      uncovered: 'drives a real git client over HTTP and SSH against a release build of ForgeKeep; '
        + 'needs the binary built and ports bound.',
    },
  ],
  [
    'postgres-smoke',
    { uncovered: 'needs a live PostgreSQL; the workflow gets one from a service container.' },
  ],
  [
    'mysql-smoke',
    { uncovered: 'needs a live MySQL; the workflow gets one from a service container.' },
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
  // env_file, and the main compose reads FORGEKEEP_JWT_SECRET out of it.
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
      temporaryRoot = mkdtempSync(join(tmpdir(), 'forgekeep-deploy-config-'));
      envFile = join(temporaryRoot, '.env');
      const examplePath = join(repoRoot, 'deploy', '.env.example');
      const example = readFileSync(examplePath, 'utf8');
      const secretLine = /^FORGEKEEP_JWT_SECRET=.*$/m;
      if (!secretLine.test(example)) {
        throw new Error(`${examplePath} no longer declares FORGEKEEP_JWT_SECRET`);
      }
      writeFileSync(
        envFile,
        example.replace(secretLine, `FORGEKEEP_JWT_SECRET=${randomBytes(32).toString('hex')}`),
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
      docker compose --env-file "\${FORGEKEEP_DEPLOY_ENV_FILE}" -f "\${compose}" config >/dev/null
    done
  `, {
    cwd: repoRoot,
    env: { FORGEKEEP_DEPLOY_ENV_FILE: envFile },
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
  // half. The verifier covers the two cargo commands; the rest of regression.yml
  // is still enforced by nothing, and silence would read as coverage.
  const uncovered = [...CARGO_JOBS].filter(([, where]) => where.uncovered).map(([job]) => job);
  if (uncovered.length > 0) {
    console.log(
      `ℹ️  ${uncovered.length} gate(s) of regression.yml run nowhere — ${uncovered.join(', ')}. `
        + 'CI has never executed a step; see the header of this file.',
    );
  }

  process.exit(failures.length === 0 ? 0 : 1);
}

// Importable by the coverage contract check without executing the gates.
if (process.argv[1] === fileURLToPath(import.meta.url)) main();
