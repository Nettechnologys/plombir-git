#!/usr/bin/env node

// Runs the cargo-free gates of `.github/workflows/regression.yml` locally.
//
// Why this exists: the workflow has never executed a single step. 400 runs out
// of 400, from the first one on 2026-07-26 onward, were refused before a runner
// was ever assigned — GitHub's own check-run annotation on every job reads "The
// job was not started because recent account payments have failed or your
// spending limit needs to be increased". So every gate declared in that file was
// enforced by nothing, and `.githooks/pre-push` mirrored two of the twelve
// (`cargo fmt`, `cargo clippy`). A gate nobody executes is a comment, and a
// silenced gate is indistinguishable by construction from one that keeps
// passing — which is why nobody noticed that green had never happened once.
//
// This runner covers the gates that need no Rust build, so it stays inside the
// seconds-not-minutes budget a pre-push hook can honestly ask for. Measured on a
// warm checkout: contract checks 1.2s, compose 0.2s, observability 1.6s,
// frontend 8.8s.
//
// It is deliberately ONE target rather than a list of commands inside the hook:
// a hook that spells out the commands drifts away from the workflow silently,
// which is the same defect one level down. `scripts/local-gate-coverage-
// contract-check.mjs` is the ratchet that keeps this file in step with the
// workflow — every one of the twelve jobs must be accounted for below, and it
// fails when one is not, when an entry names a job that no longer exists, or
// when the hook stops invoking a cargo command CARGO_JOBS says it invokes.

import { existsSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
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
// `.githooks/pre-push` names two cargo commands, five jobs run nowhere at all,
// and nothing goes red when either fact changes.
//
// Each entry declares one of two things, and the coverage contract check proves
// it rather than trusting it:
//   hook:      a command `.githooks/pre-push` must invoke. Delete the line from
//              the hook and the check goes red, so the gate cannot be dropped
//              quietly the way it could be while the hook was the only record.
//   uncovered: this gate is enforced by NOTHING right now, with the reason it
//              cannot be mirrored before a push. The count is printed, so
//              "covered" and "half covered" stop looking identical.
export const CARGO_JOBS = new Map([
  ['fmt', { hook: 'cargo fmt' }],
  ['clippy', { hook: 'cargo clippy' }],
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
export const GATES = [
  { job: 'contract-checks', name: 'Frontend/backend contract checks', run: runContractChecks },
  { job: 'deploy-config', name: 'Docker compose config', run: runDeployConfig },
  { job: 'observability-config', name: 'Prometheus and Alertmanager config', run: runObservability },
  { job: 'frontend', name: 'Frontend check and build', run: runFrontend },
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

function runDeployConfig() {
  const missing = requireTool('docker', '`docker compose config` is what validates the compose files');
  if (missing) return { ok: false, output: missing };

  // Mirrors the workflow step, including its ratchet: an empty glob fails loudly
  // rather than passing a loop over nothing. deploy/.env is created because
  // hostdir's `env_file` requires it to exist and the main compose reads
  // FORGEKEEP_JWT_SECRET out of it; the trap removes it even on failure.
  //
  // Unlike the runner-local CI copy, this refuses to clobber a deploy/.env the
  // developer already has — that file holds real local secrets on a workstation.
  return fromResult(sh(`
    if [ -e deploy/.env ]; then
      echo "deploy/.env exists; validating against it rather than overwriting it."
      created=""
    else
      cp deploy/.env.example deploy/.env
      secret="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \\n')"
      sed -i "s/^FORGEKEEP_JWT_SECRET=.*/FORGEKEEP_JWT_SECRET=\${secret}/" deploy/.env
      created=1
    fi
    trap '[ -n "\${created}" ] && rm -f deploy/.env' EXIT

    shopt -s nullglob
    composes=(deploy/docker-compose*.yml)
    if [ "\${#composes[@]}" -eq 0 ]; then
      echo "No deploy/docker-compose*.yml found — the glob no longer matches anything." >&2
      exit 1
    fi
    for compose in "\${composes[@]}"; do
      echo "Validating \${compose}"
      docker compose -f "\${compose}" config >/dev/null
    done
  `));
}

function runObservability() {
  const missing = requireTool('docker', 'promtool and amtool ship inside the official images and nowhere else');
  if (missing) return { ok: false, output: missing };

  // Image tags are read out of the compose file rather than pinned a second time
  // here, for the reason the workflow gives: a duplicate pin would validate the
  // config with a promtool that is not the one loading it the moment somebody
  // bumps the stack.
  return fromResult(sh(`
    COMPOSE=deploy/docker-compose.observability.yml
    shopt -s nullglob

    for service in prometheus alertmanager; do
      image="$(grep -oE "image: prom/\${service}:[^[:space:]]+" "\${COMPOSE}" | head -1 | cut -d' ' -f2)"
      if [ -z "\${image}" ]; then
        echo "\${COMPOSE} no longer pins a prom/\${service} image." >&2
        exit 1
      fi
      declare "image_\${service}=\${image}"
      echo "\${service}: \${image}"
    done

    configs=(deploy/prometheus/prometheus*.yml)
    rules=(deploy/prometheus/alerts*.yml)
    if [ "\${#configs[@]}" -eq 0 ] || [ "\${#rules[@]}" -eq 0 ]; then
      echo "deploy/prometheus/ no longer holds a prometheus*.yml and an alerts*.yml." >&2
      exit 1
    fi
    for config in "\${configs[@]}"; do
      docker run --rm -v "\${PWD}/deploy/prometheus:/cfg:ro" \\
        --entrypoint promtool "\${image_prometheus}" check config "/cfg/$(basename "\${config}")"
    done
    for rule in "\${rules[@]}"; do
      docker run --rm -v "\${PWD}/deploy/prometheus:/cfg:ro" \\
        --entrypoint promtool "\${image_prometheus}" check rules "/cfg/$(basename "\${rule}")"
    done

    amconfigs=(deploy/alertmanager/*.yml)
    if [ "\${#amconfigs[@]}" -eq 0 ]; then
      echo "deploy/alertmanager/ no longer holds a *.yml." >&2
      exit 1
    fi
    for config in "\${amconfigs[@]}"; do
      docker run --rm -v "\${PWD}/deploy/alertmanager:/cfg:ro" \\
        --entrypoint amtool "\${image_alertmanager}" check-config "/cfg/$(basename "\${config}")"
    done

    # Docker's failure mode for a missing bind-mount source is silent: it creates
    # an empty directory, so a renamed alerts.yml gets Prometheus a directory
    # where it expects a rule file. \`docker compose config\` never looks at paths.
    missing=0
    mounts="$(sed -nE 's|^[[:space:]]+- (\\./[^:]+):.*|\\1|p' "\${COMPOSE}")"
    if [ -z "\${mounts}" ]; then
      echo "\${COMPOSE} declares no relative bind mounts — the parse broke." >&2
      exit 1
    fi
    while read -r mount; do
      path="deploy/\${mount#./}"
      if [ -e "\${path}" ]; then
        echo "ok  \${path}"
      else
        echo "MISSING  \${path} — compose would create an empty directory there." >&2
        missing=1
      fi
    done <<<"\${mounts}"
    [ "\${missing}" -eq 0 ] || exit 1

    dashboards=(deploy/grafana/dashboards/*.json)
    if [ "\${#dashboards[@]}" -eq 0 ]; then
      echo "deploy/grafana/dashboards/ no longer holds a *.json." >&2
      exit 1
    fi
    for dashboard in "\${dashboards[@]}"; do
      echo "Parsing \${dashboard}"
      node -e 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))' "\${dashboard}"
    done
  `));
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

  // Said out loud on every push: a green run here covers the cargo-free half and
  // the two cargo commands the hook names. The rest of regression.yml is still
  // enforced by nothing, and silence would read as coverage.
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
