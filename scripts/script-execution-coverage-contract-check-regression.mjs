#!/usr/bin/env node

// Copied-tree mutation stand for script-execution-coverage-contract-check.mjs.
// The production checker must distinguish executable shell/JavaScript wiring
// from a basename that survives only in a comment or inert string literal.

import { spawnSync } from 'node:child_process';
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SELF = 'script-execution-coverage-contract-check.mjs';

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-script-execution-coverage-'));
  mkdirSync(join(fixture, '.github'), { recursive: true });
  mkdirSync(join(fixture, 'web'), { recursive: true });
  cpSync(join(root, 'scripts'), join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, '.github', 'workflows'), join(fixture, '.github', 'workflows'), { recursive: true });
  cpSync(join(root, '.githooks'), join(fixture, '.githooks'), { recursive: true });
  cpSync(join(root, 'web', 'package.json'), join(fixture, 'web', 'package.json'));
  if (existsSync(join(root, 'package.json'))) {
    cpSync(join(root, 'package.json'), join(fixture, 'package.json'));
  }
  return fixture;
}

function replaceRequired(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function append(file, source) {
  writeFileSync(file, `${readFileSync(file, 'utf8')}\n${source}\n`);
}

function addScript(fixture, name) {
  const file = join(fixture, 'scripts', name);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, '#!/usr/bin/env node\n');
}

function runFixture(name, mutate, expectedStatus, expectedOutput, mutateCheck = null) {
  const fixture = fixtureRoot();
  try {
    const check = join(fixture, 'scripts', SELF);
    if (mutateCheck) mutateCheck(check);
    if (mutate) mutate(fixture);
    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: { ...process.env, PLOMBIR_GIT_SCRIPT_EXECUTION_COVERAGE_ROOT: fixture },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (result.status !== expectedStatus || !output.includes(expectedOutput)) {
      throw new Error(
        `${name}: expected exit ${expectedStatus} and ${JSON.stringify(expectedOutput)}, `
          + `got exit ${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const workflow = (fixture) => join(fixture, '.github', 'workflows', 'regression.yml');
const hook = (fixture) => join(fixture, '.githooks', 'pre-push');
const localRunner = (fixture) => join(fixture, 'scripts', 'run-local-gates.mjs');

runFixture(
  'the copied clean tree preserves the execution census',
  null,
  0,
  'script execution coverage:',
);

const inlineWorkflowComment = (fixture) => {
  addScript(fixture, 'orphan-workflow-inline-fixture.mjs');
  replaceRequired(
    workflow(fixture),
    '        run: node scripts/run-contract-checks.mjs',
    '        run: node scripts/run-contract-checks.mjs\n'
      + '      - name: Inline reachability decoy\n'
      + '        run: |\n'
      + '          true # node scripts/orphan-workflow-inline-fixture.mjs',
  );
};

runFixture(
  'a workflow inline shell comment is not execution',
  inlineWorkflowComment,
  1,
  'scripts/orphan-workflow-inline-fixture.mjs is executed by nothing',
);

runFixture(
  'a hook inline shell comment is not execution',
  (fixture) => replaceRequired(
    hook(fixture),
    '    sh scripts/verify-push-gates.sh',
    '    true # sh scripts/verify-push-gates.sh',
  ),
  1,
  'scripts/verify-push-gates.sh is executed by nothing',
);

for (const [label, decoy] of [
  ['an inline JavaScript comment is not execution', "true; // import './orphan-inline-fixture.mjs';"],
  ['a JavaScript block comment is not execution', "true; /* import './orphan-block-fixture.mjs'; */"],
  ['an inert JavaScript string is not execution', "const inertFixtureName = 'orphan-string-fixture.mjs';"],
  [
    'RegExp.exec is not a child-process execution sink',
    "const inertRegexFixture = 'orphan-regex-fixture.mjs'; /fixture/.exec(inertRegexFixture);",
  ],
]) {
  const script = decoy.match(/orphan-[a-z-]+\.mjs/)?.[0];
  runFixture(
    label,
    (fixture) => {
      addScript(fixture, script);
      append(localRunner(fixture), decoy);
    },
    1,
    `scripts/${script} is executed by nothing`,
  );
}

runFixture(
  'a runner suffix declaration inside a block comment does not drive the glob',
  (fixture) => {
    addScript(fixture, 'orphan-suffix-fixture-contract-check.mjs');
    replaceRequired(
      join(fixture, 'scripts', 'run-contract-checks.mjs'),
      "const CHECK_SUFFIX = '-contract-check.mjs';",
      "/* const CHECK_SUFFIX = '-contract-check.mjs'; */",
    );
  },
  1,
  'scripts/orphan-suffix-fixture-contract-check.mjs is executed by nothing',
);

runFixture(
  'a live JavaScript import remains an execution edge',
  (fixture) => {
    addScript(fixture, 'lib/live-import-fixture.mjs');
    append(localRunner(fixture), "import './lib/live-import-fixture.mjs';");
  },
  0,
  'script execution coverage:',
);

runFixture(
  'a filename binding stops counting when it no longer reaches spawn',
  (fixture) => replaceRequired(
    join(fixture, 'scripts', 'codex-hourly-automation-contract-check.mjs'),
    'const child = spawn(process.execPath, [automation], {',
    "const child = spawn(process.execPath, ['-e', ''], {",
  ),
  1,
  'scripts/codex-hourly-automation.mjs is executed by nothing',
);

runFixture(
  'an iterated filename list stops counting when its wrapper no longer launches that parameter',
  (fixture) => replaceRequired(
    join(fixture, 'scripts', 'browser-smoke-cdp-contract-check.mjs'),
    "spawn(process.execPath, [join(scriptsDir, scriptName), '--cdp-endpoint-only'], {",
    "spawn(process.execPath, [join(scriptsDir, 'browser-admin-smoke.mjs'), '--cdp-endpoint-only'], {",
  ),
  1,
  'scripts/console-smoke.mjs is executed by nothing',
);

runFixture(
  'quoted hashes and assignment hashes do not hide later live shell arguments',
  (fixture) => {
    addScript(fixture, 'live-quoted-fixture.mjs');
    addScript(fixture, 'live-assignment-fixture.mjs');
    append(
      hook(fixture),
      "printf '%s\\n' '# data'; node \"scripts/live-quoted-fixture.mjs\"\n"
        + 'value=v3#1; node scripts/live-assignment-fixture.mjs',
    );
  },
  0,
  'script execution coverage:',
);

runFixture(
  'malformed workflow YAML fails closed',
  (fixture) => append(workflow(fixture), 'broken: ['),
  1,
  'is not a readable workflow',
);

runFixture(
  'the old whole-line workflow filter reproduces the inline-comment false green',
  inlineWorkflowComment,
  0,
  'script execution coverage:',
  (check) => replaceRequired(
    check,
    "return referencedIn(shellCodeOnly(runs.join('\\n')));",
    "return referencedIn(runs.join('\\n').split('\\n').filter((line) => !line.trimStart().startsWith('#')).join('\\n'));",
  ),
);
