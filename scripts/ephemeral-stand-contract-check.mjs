#!/usr/bin/env node

// Asserts that the ephemeral browser/e2e stand stays ephemeral, stays wired to
// its own backend, and stays the only place that boots one.
//
// Why this exists — three ways this stand can rot, each of which looks green:
//
//   1. A second boot sequence. `scripts/git-protocol-e2e.sh` was the only
//      script that knew how to start a private ForgeKeep and wait until it was
//      really up; a browser test needs the same thing, and the cheap way to get
//      it is to copy the block. Then one copy learns about a new flag and the
//      other does not. The invariant here is that `--listen-address-file` — the
//      flag a test-owned server is started with — appears in exactly one file.
//
//   2. A preview server with no proxy. The app calls its backend on the origin
//      it was loaded from (`web/src/lib/api/_base.svelte.ts` defaults the API
//      base to the relative `/api/v1`), so `vite preview` has to forward that
//      path to the stand's backend. Without the proxy every page still renders
//      and every API call answers 404 from the preview server itself: a stand
//      that boots, looks healthy, and tests nothing. Worse, `server.proxy`
//      alone keeps `npm run dev` working, so the hole is invisible to everyone
//      not running a preview. This check requires the two proxies to name the
//      same prefixes, and the target to come from the environment variable the
//      loader exports — a literal port cannot be right for a stand that binds
//      port 0.
//
//   3. A teardown that misses something. Whatever the loader starts, it must
//      register for the trap, and the trap must remove the workspace. A leaked
//      `vite preview` holds its port; a leaked workspace is a database the next
//      run's "the list holds exactly one issue" assertion trips over.
//
// Truth boundary: shell and JavaScript are read through their language-aware
// source views. Real comments are blanked, including inline ones; hashes inside
// shell quotes, assignments and parameter expansions, plus JavaScript string
// bodies, stay live data. This proves the wiring is spelled, not that it
// executes; the stand's own end-to-end runs are what prove that.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { shellCodeOnly } from './lib/shell-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.FORGEKEEP_EPHEMERAL_STAND_ROOT || join(scriptsDir, '..'));

const LIB = 'scripts/lib/stand.sh';
const ENTRY = 'scripts/ephemeral-stand.sh';
const VITE_CONFIG = 'web/vite.config.ts';
const BACKEND_ORIGIN_ENV = 'FORGEKEEP_BACKEND_ORIGIN';
const LISTEN_FLAG = '--listen-address-file';

const failures = [];

function read(relPath) {
  const full = join(root, relPath);
  if (!existsSync(full)) {
    failures.push(`${relPath} is missing — the ephemeral stand cannot be checked without it`);
    return null;
  }
  return readFileSync(full, 'utf8');
}

/** The body of a `name() { … }` shell function, or null. */
function shellFunctionBody(code, name) {
  const header = new RegExp(`^${name}\\s*\\(\\)\\s*\\{`, 'm');
  const start = code.search(header);
  if (start < 0) return null;
  const open = code.indexOf('{', start);
  const close = code.indexOf('\n}', open);
  if (close < 0) return null;
  return code.slice(open + 1, close);
}

function requireIn(body, needle, where, why) {
  if (body === null) return;
  if (!body.includes(needle)) failures.push(`${where} does not ${why} (looked for \`${needle}\`)`);
}

// ---------------------------------------------------------------- the loader
const libSource = read(LIB);
const libCode = libSource === null ? null : shellCodeOnly(libSource);

if (libCode !== null) {
  for (const fn of ['stand_open', 'stand_spawn', 'stand_cleanup', 'stand_start_backend', 'stand_start_frontend']) {
    if (shellFunctionBody(libCode, fn) === null) {
      failures.push(`${LIB} no longer defines \`${fn}\`, which its callers source it for`);
    }
  }

  const open = shellFunctionBody(libCode, 'stand_open');
  if (open !== null) {
    const trap = /trap\s+'?[^']*'?\s+([A-Z ]+)/.exec(open);
    const signals = trap ? trap[1].split(/\s+/) : [];
    for (const signal of ['EXIT', 'INT', 'TERM']) {
      if (!signals.includes(signal)) {
        failures.push(
          `${LIB}: stand_open does not install a teardown trap on ${signal}, so an interrupted run ` +
            'leaves its server and its temporary database behind',
        );
      }
    }
  }

  requireIn(
    shellFunctionBody(libCode, 'stand_spawn'),
    'STAND_PIDS+=(',
    `${LIB}: stand_spawn`,
    'register the child it started for teardown',
  );

  const cleanup = shellFunctionBody(libCode, 'stand_cleanup');
  requireIn(cleanup, 'STAND_PIDS', `${LIB}: stand_cleanup`, 'stop the processes the stand started');
  requireIn(cleanup, 'rm -rf', `${LIB}: stand_cleanup`, 'remove the temporary workspace');
  requireIn(cleanup, 'STAND_WORK_DIR', `${LIB}: stand_cleanup`, 'name the workspace it removes');

  requireIn(
    shellFunctionBody(libCode, 'stand_start_frontend'),
    BACKEND_ORIGIN_ENV,
    `${LIB}: stand_start_frontend`,
    `tell the preview server where the backend is via ${BACKEND_ORIGIN_ENV}`,
  );

  const backend = shellFunctionBody(libCode, 'stand_start_backend');
  requireIn(backend, '127.0.0.1:0', `${LIB}: stand_start_backend`, 'bind ephemeral ports');
  requireIn(backend, 'STAND_WORK_DIR', `${LIB}: stand_start_backend`, 'keep its database inside the temporary workspace');
}

// ------------------------------------------------------------ one boot path
//
// There are no exemptions. A consumer may wrap the entry point, but it must not
// grow its own spelling of the boot sequence again.

function scriptsUnder(dir, prefix, extensions) {
  if (!existsSync(dir)) return [];
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.isDirectory()) out.push(...scriptsUnder(join(dir, entry.name), `${prefix}${entry.name}/`, extensions));
    else if (extensions.some((extension) => entry.name.endsWith(extension))) out.push(`${prefix}${entry.name}`);
  }
  return out;
}

// The sweep covers Node as well as shell: the duplicate this loader replaced was
// a shell copy, but the next one is as likely to be written in whatever language
// its caller happens to be, and a shell-only invariant would not see it.
//
// "Boots one" means the binary is INVOKED with `serve`, not that the flag is
// mentioned: four scripts name `--listen-address-file` while parsing it, and
// reporting those would make this check something nobody can keep green.
const STARTS_A_SERVER = [
  /\$\{[A-Za-z_]*BIN[^}]*\}"?\s+serve\b/, // "${STAND_BIN}" serve …
  /forgekeep['"]\s*,\s*\[\s*['"]serve['"]/, // spawn('…/forgekeep', ['serve', …
];

// This file and its mutation stand quote the antipattern on purpose — the stand
// writes a fixture that boots a server, which is how it proves this sweep still
// bites. Scoring them would report the check's own subject as a finding.
const QUOTES_THE_ANTIPATTERN = [
  'scripts/ephemeral-stand-contract-check.mjs',
  'scripts/ephemeral-stand-contract-check-regression.mjs',
];

const booters = [];
for (const script of scriptsUnder(join(root, 'scripts'), 'scripts/', ['.sh', '.mjs'])) {
  if (QUOTES_THE_ANTIPATTERN.includes(script)) continue;
  const source = readFileSync(join(root, script), 'utf8');
  const code = script.endsWith('.sh') ? shellCodeOnly(source) : productionTsSource(source);
  if (code.includes(LISTEN_FLAG) && STARTS_A_SERVER.some((pattern) => pattern.test(code))) {
    booters.push(script);
  }
}
if (!booters.includes(LIB)) {
  failures.push(`${LIB} no longer starts a server with \`${LISTEN_FLAG}\`, so no shared loader is left to reuse`);
}
for (const script of booters.filter((script) => script !== LIB)) {
  failures.push(
    `${script} starts its own ForgeKeep with \`${LISTEN_FLAG}\` instead of reusing ${LIB}; two boot ` +
      'sequences drift, and the copy is what the tests then inherit. Reuse the loader.',
  );
}

const e2e = read('scripts/git-protocol-e2e.sh');
if (e2e !== null && !shellCodeOnly(e2e).includes('lib/stand.sh')) {
  failures.push(
    'scripts/git-protocol-e2e.sh no longer sources the shared loader — it is where the boot sequence came ' +
      'from, so a copy there is the original defect returning',
  );
}

// ------------------------------------------------------------- the entry point
const entry = read(ENTRY);
if (entry !== null) {
  const code = shellCodeOnly(entry);
  for (const [needle, what] of [
    ['backend=', 'the backend URL'],
    ['frontend=', 'the frontend URL'],
  ]) {
    if (!code.includes(needle)) {
      failures.push(`${ENTRY} does not print ${what}; a stand nobody can address is not usable by hand`);
    }
  }
  if (!code.includes(LIB.replace('scripts/', ''))) {
    failures.push(`${ENTRY} does not source ${LIB}`);
  }
}

// -------------------------------------------------- dev and preview must agree
const viteSource = read(VITE_CONFIG);
if (viteSource !== null) {
  const config = productionTsSource(viteSource);

  if (!config.includes(BACKEND_ORIGIN_ENV)) {
    failures.push(
      `${VITE_CONFIG} does not read ${BACKEND_ORIGIN_ENV}: the stand binds port 0, so a literal backend ` +
        'port cannot be the one it started',
    );
  }

  // The proxy table a `server:` / `preview:` key ends up with, following the
  // shorthand (`{ proxy }`) and one level of `const` indirection — the two
  // spellings a config uses precisely when it means "both servers, same table".
  // Brace matching over a view that keeps string bodies is safe here because
  // the values are URLs; a config that puts a brace in a proxy string would
  // read as unparsable and fail loudly below rather than silently.
  function balanced(text, openIndex) {
    let depth = 0;
    for (let i = openIndex; i < text.length; i += 1) {
      if (text[i] === '{') depth += 1;
      else if (text[i] === '}') {
        depth -= 1;
        if (depth === 0) return text.slice(openIndex + 1, i);
      }
    }
    return null;
  }

  function objectNamed(name) {
    const declaration = new RegExp(`\\b(?:const|let|var)\\s+${name}\\s*(?::[^=]*)?=\\s*\\{`).exec(config);
    if (!declaration) return null;
    return balanced(config, config.indexOf('{', declaration.index));
  }

  function proxyPrefixes(key) {
    const at = config.search(new RegExp(`\\b${key}\\s*:\\s*\\{`, 'm'));
    if (at < 0) return null;
    const body = balanced(config, config.indexOf('{', at));
    if (body === null) return null;

    let table = null;
    const assigned = /\bproxy\s*:\s*(\{|[A-Za-z_$][\w$]*)/.exec(body);
    if (assigned && assigned[1] === '{') {
      table = balanced(body, body.indexOf('{', assigned.index));
    } else if (assigned) {
      table = objectNamed(assigned[1]);
    } else if (/\bproxy\b\s*(?:[,}\n]|$)/.test(body.trim())) {
      table = objectNamed('proxy');
    }
    if (table === null) return null;
    return [...table.matchAll(/['"](\/[^'"]*)['"]\s*:/g)].map((match) => match[1]).sort();
  }

  const dev = proxyPrefixes('server');
  const preview = proxyPrefixes('preview');

  if (preview === null) {
    failures.push(
      `${VITE_CONFIG} declares no preview proxy: \`vite preview\` would serve the app and answer every ` +
        `\`/api/v1\` call with its own 404, which is the stand booting and testing nothing`,
    );
  } else if (dev === null) {
    failures.push(`${VITE_CONFIG} declares no dev-server proxy, so this check cannot compare the two`);
  } else if (dev.join(',') !== preview.join(',')) {
    failures.push(
      `${VITE_CONFIG}: the dev proxy forwards [${dev.join(', ')}] and the preview proxy forwards ` +
        `[${preview.join(', ')}]. A path that only works under \`npm run dev\` is a browser test failing ` +
        'on the built app for a reason no developer can reproduce',
    );
  } else if (!preview.includes('/api/v1')) {
    failures.push(`${VITE_CONFIG}: neither proxy forwards /api/v1, which is where the whole client API lives`);
  }

  if (!/\bws\s*:\s*true/.test(config)) {
    failures.push(
      `${VITE_CONFIG} does not enable websocket proxying: the notification socket opened by ` +
        '`connectNotificationWebSocket` (web/src/lib/api/websockets.ts) would never connect, and the ' +
        'browser console errors that follow read as an app bug',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ Ephemeral stand contract failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `✅ ephemeral stand: one boot path (${relative(root, join(root, LIB))}), teardown covers processes and ` +
    'workspace, and dev/preview proxy the same prefixes at the backend the loader publishes',
);
