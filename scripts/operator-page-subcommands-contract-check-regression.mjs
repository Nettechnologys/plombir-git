#!/usr/bin/env node

// Does `scripts/operator-page-subcommands-contract-check.mjs` still bite?
//
// Fixtures in the vocabulary of the two defects it was written for: a page
// naming a subcommand the binary does not have (main's guide handing a v0.1.1
// operator `create-admin`, card_d670212c28bb), and a subcommand written right
// after a compose service, which Docker runs as a program of that name because
// the image's ENTRYPOINT is only a pass-through init. Plus the premise and the
// reader: an ENTRYPOINT that is not that init appearing, and prose outside code
// not counting as a command.
//
// Every fixture is a copied repository shape, never the checkout.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/operator-page-subcommands-contract-check.mjs';
const COPIED = [
  check,
  'scripts/lib',
  'README.md',
  'deploy/README.md',
  'Dockerfile',
  'crates/rg-cli/src/cli.rs',
  'crates/rg-runner/src/cli.rs',
];

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-operator-page-subcommands-'));
  for (const path of COPIED) {
    mkdirSync(dirname(join(fixture, path)), { recursive: true });
    cpSync(join(root, path), join(fixture, path), { recursive: true });
  }
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [join(fixture, check), fixture], {
    cwd: fixture,
    encoding: 'utf8',
  });
  return { status: result.status, stdout: result.stdout ?? '', output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

let failures = 0;

function report(name, reason, result) {
  if (reason) {
    failures += 1;
    console.error(`❌ ${name}: ${reason}\n----- exit ${result?.status} -----\n${result?.output ?? ''}`);
  } else {
    console.log(`✅ ${name}`);
  }
}

// `expect` is a diagnostic fragment for a red fixture, or `{ green: line }` for
// one that must pass and print `line`.
function fixture(name, file, mutate, expect) {
  const dir = fixtureRoot();
  try {
    const path = join(dir, file);
    const source = readFileSync(path, 'utf8');
    const mutated = mutate(source);
    if (mutated === source) throw new Error('the mutation changed nothing — the anchor it edits is gone');
    writeFileSync(path, mutated);
    const result = run(dir);
    let reason = null;
    if (typeof expect === 'object') {
      if (result.status !== 0) reason = 'the check failed';
      else if (!result.stdout.split('\n').includes(expect.green)) reason = `stdout does not list ${JSON.stringify(expect.green)}`;
      else if (expect.absent && result.stdout.includes(expect.absent)) reason = `stdout lists ${JSON.stringify(expect.absent)}`;
    } else if (result.status === 0) {
      reason = 'the check passed';
    } else if (!result.output.includes(expect)) {
      reason = `no diagnostic mentioning ${JSON.stringify(expect)}`;
    }
    report(name, reason, result);
  } catch (error) {
    report(name, error.message, null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

{
  const dir = fixtureRoot();
  const result = run(dir);
  rmSync(dir, { recursive: true, force: true });
  let reason = null;
  if (result.status !== 0) reason = 'the check failed';
  else if (!result.stdout.split('\n').includes('plombir-git create-admin')) reason = 'stdout does not list `plombir-git create-admin`';
  else if (/^plombir-git (grep|sh|plombir-git)$/m.test(result.stdout)) reason = 'stdout lists a service or a shell as a subcommand';
  report('the unmutated fixture', reason, result);
}

fixture(
  'a subcommand right after the compose service is red',
  'deploy/README.md',
  (source) => source.replace(
    'docker compose run --rm plombir-git plombir-git migrate',
    'docker compose run --rm plombir-git migrate',
  ),
  "the image's ENTRYPOINT only execs its arguments",
);

fixture(
  'a page naming a subcommand the binary lacks is red',
  'deploy/README.md',
  (source) => source.replace('plombir-git plombir-git create-admin', 'plombir-git plombir-git create-administrator'),
  'declares no `create-administrator` subcommand',
);

fixture(
  'renaming the subcommand in clap while the page keeps the old name is red',
  'crates/rg-cli/src/cli.rs',
  (source) => source.replace(/^( {4})CreateAdmin \{$/m, '$1MakeAdmin {'),
  'declares no `create-admin` subcommand',
);

fixture(
  'an ENTRYPOINT other than the pass-through init is red until the rule is revisited',
  'Dockerfile',
  (source) => source.replace(/^ENTRYPOINT \[.*\]$/m, 'ENTRYPOINT ["/usr/bin/tini", "--", "plombir-git"]'),
  'which is not the pass-through init',
);

fixture(
  'a second ENTRYPOINT after the init is red',
  'Dockerfile',
  (source) => source.replace(/^CMD \[/m, 'ENTRYPOINT ["plombir-git"]\nCMD ['),
  'which is not the pass-through init',
);

fixture(
  'prose outside code does not count as a command',
  'deploy/README.md',
  (source) => `${source}\nNever run plombir-git frobnicate on a live server.\n`,
  { green: 'plombir-git serve', absent: 'frobnicate' },
);

fixture(
  'a subcommand inside an `sh -c` body is read',
  'deploy/README.md',
  (source) => `${source}\n\`\`\`bash\ndocker compose exec plombir-git sh -c 'cd /data && plombir-git frobnicate'\n\`\`\`\n`,
  'declares no `frobnicate` subcommand',
);

if (failures > 0) {
  console.error(`\n${failures} fixture(s) did not behave — the operator-page subcommands check has stopped asserting what it claims.`);
  process.exit(1);
}
console.log('\noperator-page-subcommands-contract-check still bites.');
