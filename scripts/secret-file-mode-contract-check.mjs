#!/usr/bin/env node

// A secret the server creates must be born owner-only — never created wide and
// narrowed by a second step.
//
// Why this exists: `std::fs::write` (and any bare `File::create`) takes the
// ambient umask, so a freshly generated key or token lands `0644` on a stock
// host, and only the `chmod` the author remembered to write next takes that
// back. The gap is not merely a window another local account can walk through.
// A crash, a `SIGKILL` or a full disk between the two statements leaves the
// file readable *permanently*, because the code that generates a secret runs
// once: every later start finds the file present and steps aside, so nothing
// ever narrows it again. `crates/rg-ssh/src/lib.rs` shipped exactly that for
// the instance's SSH host private key — the material that lets anyone holding
// it answer as this host to every client that has accepted its fingerprint
// (card_ece70ceb51ec).
//
// The fix is a shape, not a habit: pass the mode to `open(2)`, where `O_CREAT`
// masks it with the umask and a umask can only *clear* bits, so the file is
// never wider than `0600` for an instant. `rg_core::platform::fs::
// create_new_owner_only` is that shape, and `create_new` closes the second half
// of the same defect — two first starts racing can no longer have the loser's
// key silently replace the winner's.
//
// The Rust tests beside both call sites prove the behaviour; this check is what
// stops the *spelling* coming back somewhere new. The unit tests cannot see a
// third writer added next month, and the sequence is invisible to the compiler
// and to Clippy alike.
//
// Truth boundary: every Rust file is read through `productionRustCode`, so a
// commented-out call and a `#[cfg(test)]` fixture both read as absent — a test
// that deliberately writes a `0644` key to check the *loader* refuses it is not
// a defect in the server. Strings are blanked in that view too, which is what
// keeps a doc example or a literal mentioning `set_permissions` out of the
// sweep.

import { readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { rustFiles } from './lib/rust-consumer-contract.mjs';
import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.FORGEKEEP_SECRET_FILE_MODE_ROOT ?? join(scriptsDir, '..'));
const cratesDir = join(root, 'crates');
const failures = [];

// The path expression as it is spelled at the call — `path`, `&probe`,
// `self.key_file`. Anything richer than that (an inline `format!`, an indexed
// element) is not a name this can follow to the `chmod`, and a sweep that
// guessed would be reporting its own parse rather than the code.
const PATH_EXPRESSION = '&?[A-Za-z_][A-Za-z0-9_]*(?:\\.[A-Za-z_][A-Za-z0-9_]*)*';

// The two ways to bring a file into being without saying what its mode is.
// `OpenOptions` is deliberately not here: that is the form that *can* carry a
// mode, and asserting on which one it carries is the job of the anchors below.
const WIDE_CREATE = [
  {
    call: 'fs::write',
    re: new RegExp(`\\b(?:std::|tokio::)?fs::write\\s*\\(\\s*(${PATH_EXPRESSION})\\s*,`, 'g'),
  },
  {
    call: 'File::create',
    re: new RegExp(`\\b(?:std::|tokio::)?fs::File::create\\s*\\(\\s*(${PATH_EXPRESSION})\\s*\\)`, 'g'),
  },
];

/** The body of a top-level `fn <name>` in a production view, or `null`. */
function fnBody(code, name) {
  const start = code.search(
    new RegExp(
      `^(?:pub(?:\\([^)]*\\))?\\s+)?(?:async\\s+)?fn ${name}\\s*(?:<[^>]*>)?\\s*\\(`,
      'm',
    ),
  );
  if (start < 0) return null;
  const rest = code.slice(start);
  const close = rest.search(/\n\}/);
  if (close < 0) return null;
  const brace = rest.indexOf('{');
  if (brace < 0 || brace > close) return null;
  return rest.slice(brace, close + 2);
}

/** The balanced brace body beginning at `start`, in an already-masked view. */
function bracedBodyAt(code, start) {
  const open = code.indexOf('{', start);
  if (open < 0) return null;
  let depth = 0;
  for (let cursor = open; cursor < code.length; cursor += 1) {
    if (code[cursor] === '{') depth += 1;
    if (code[cursor] === '}') depth -= 1;
    if (depth === 0) return code.slice(open, cursor + 1);
  }
  return null;
}

// ---------------------------------------------------------------------------
// The sweep: nobody creates a file wide and narrows the same path afterwards.
// ---------------------------------------------------------------------------

const sources = rustFiles(cratesDir).filter((file) => file.includes(`${join('', 'src', '')}`));

// A sweep that inspected nothing passes vacuously, which is what a broken glob
// looks like from the outside. Both halves are asserted: that files were found
// at all, and that the file this check was written for is among them.
if (sources.length === 0) {
  console.error(`❌ No Rust sources found under ${cratesDir} — the sweep is broken, not the workspace.`);
  process.exit(1);
}

const SSH_LIB = 'crates/rg-ssh/src/lib.rs';
if (!sources.some((file) => relative(root, file).split('\\').join('/') === SSH_LIB)) {
  console.error(`❌ ${SSH_LIB} is outside the sweep — the file this check was written for is not being read.`);
  process.exit(1);
}

for (const file of sources) {
  const where = relative(root, file).split('\\').join('/');
  const code = productionRustCode(readFileSync(file, 'utf8'));

  for (const { call, re } of WIDE_CREATE) {
    re.lastIndex = 0;
    let match = re.exec(code);
    while (match !== null) {
      const target = match[1].replace(/^&/, '');
      const narrowing = new RegExp(`\\bset_permissions\\s*\\(\\s*&?${target.replace(/\./g, '\\.')}\\s*,`);
      const after = code.slice(match.index + match[0].length);
      if (narrowing.test(after)) {
        const line = code.slice(0, match.index).split('\n').length;
        failures.push(
          `${where}:${line}: creates \`${target}\` with \`${call}\` and narrows it with a later `
            + '`set_permissions`. The file exists at the ambient umask until that second statement '
            + 'runs, and a crash in between leaves it that way for good — pass the mode to the '
            + 'open instead (`rg_core::platform::fs::create_new_owner_only`).',
        );
      }
      match = re.exec(code);
    }
  }
}

// ---------------------------------------------------------------------------
// The anchors: the two call sites that answer this for a real secret.
// ---------------------------------------------------------------------------
//
// A sweep alone is a check that can only ever go red by accident. If the SSH
// writer were renamed, deleted or rewritten around a different helper, nothing
// above would notice — the offending spelling would simply be absent. So the
// shape is asserted positively where it matters, and the sweep is what keeps a
// *new* offender from appearing.

const helperCode = productionRustCode(
  readFileSync(join(root, 'crates/rg-core/src/platform/fs.rs'), 'utf8'),
);
const helper = fnBody(helperCode, 'create_new_owner_only');
if (!helper) {
  failures.push(
    'crates/rg-core/src/platform/fs.rs: `create_new_owner_only` is gone — the one place that '
      + 'defines how a secret file is created cannot be read.',
  );
} else {
  if (!/\bcreate_new\s*\(\s*true\s*\)/.test(helper)) {
    failures.push(
      'crates/rg-core/src/platform/fs.rs: `create_new_owner_only` no longer opens with '
        + '`create_new(true)`, so an existing key can be replaced by a losing concurrent start.',
    );
  }
  if (!/\.mode\s*\(\s*0o600\s*\)/.test(helper)) {
    failures.push(
      'crates/rg-core/src/platform/fs.rs: `create_new_owner_only` no longer passes `mode(0o600)` '
        + 'to the open, so the file it creates takes the ambient umask.',
    );
  }
}

const sshCode = productionRustCode(readFileSync(join(root, SSH_LIB), 'utf8'));
const writer = fnBody(sshCode, 'write_new_host_key');
if (!writer) {
  failures.push(
    `${SSH_LIB}: \`write_new_host_key\` is gone — the SSH host private key is generated `
      + 'somewhere this check can no longer see.',
  );
} else if (!writer.includes('create_new_owner_only')) {
  failures.push(
    `${SSH_LIB}: \`write_new_host_key\` no longer goes through `
      + '`rg_core::platform::fs::create_new_owner_only`; the host key is the instance\'s SSH '
      + 'identity and must be owner-only from its first byte.',
  );
}

const generator = fnBody(sshCode, 'ensure_host_key');
if (!generator) {
  failures.push(`${SSH_LIB}: \`ensure_host_key\` is gone — first-start key generation cannot be read.`);
} else if (/\bfs::write\s*\(/.test(generator)) {
  failures.push(
    `${SSH_LIB}: \`ensure_host_key\` writes the host key with \`fs::write\`, which creates it at `
      + 'the ambient umask.',
  );
}

// ---------------------------------------------------------------------------
// The two state files the sweep above cannot see.
// ---------------------------------------------------------------------------
//
// Both write a temp file and rename, and neither was a *secret* by the name the
// sweep looks for — but the audit archive is who did what from which IP and the
// backup snapshot is the whole credential store, argon2 hashes and sealed CI
// secrets included (card_d55ad81ab8bb). Their directories are narrowed when the
// server creates them, and that is not the answer: an `[audit].archive_dir` or
// `[backup].dir` an operator created — the usual bind-mount — keeps the mode
// they chose, and a world-readable dump inside a `0755` directory is exactly
// the exposure `ensure_owner_only` refuses to load a credential file over.
//
// They are anchored rather than swept because the sweep asks a different
// question. It catches "created wide and narrowed afterwards"; the shape these
// two were in is "created wide and never narrowed", which is invisible to it.

const ARCHIVER = 'crates/rg-core/src/audit/archiver.rs';
const BACKUP = 'crates/rg-core/src/backup.rs';

const asyncCreator = fnBody(helperCode, 'create_new_owner_only_async');
if (!asyncCreator) {
  failures.push(
    'crates/rg-core/src/platform/fs.rs: `create_new_owner_only_async` is gone — the runtime-side '
      + 'spelling the audit archiver opens its file through cannot be read.',
  );
} else if (!asyncCreator.includes('create_new_owner_only')) {
  failures.push(
    'crates/rg-core/src/platform/fs.rs: `create_new_owner_only_async` no longer goes through '
      + '`create_new_owner_only`, so the mode it promises is not the one on the open.',
  );
}

const narrower = fnBody(helperCode, 'restrict_to_owner_async');
if (!narrower) {
  failures.push(
    'crates/rg-core/src/platform/fs.rs: `restrict_to_owner_async` is gone — the backup snapshot '
      + 'has nothing to narrow it before it takes its final name.',
  );
} else if (!/from_mode\s*\(\s*0o600\s*\)/.test(narrower)) {
  failures.push(
    'crates/rg-core/src/platform/fs.rs: `restrict_to_owner_async` no longer pins `0o600`, so the '
      + 'snapshot keeps whatever `VACUUM INTO` took from the ambient umask.',
  );
}

const archiveWriter = fnBody(
  productionRustCode(readFileSync(join(root, ARCHIVER), 'utf8')),
  'write_archive_atomically',
);
if (!archiveWriter) {
  failures.push(
    `${ARCHIVER}: \`write_archive_atomically\` is gone — the audit archive is written somewhere `
      + 'this check can no longer see.',
  );
} else if (!archiveWriter.includes('create_new_owner_only_async')) {
  failures.push(
    `${ARCHIVER}: \`write_archive_atomically\` no longer opens the archive through `
      + '`rg_core::platform::fs::create_new_owner_only_async`; the file is a record of who did '
      + 'what from which IP and must be owner-only from its first byte.',
  );
}

// `VACUUM INTO` is the one creator this repository cannot pass a mode to —
// SQLite opens the file and refuses a path that already exists — so the order
// is the whole guarantee: narrow the temp name, *then* rename. A `rename` that
// overtook the `set_permissions` would publish the snapshot at the umask and
// every assertion about the final file would still be about a `0600` one,
// because by then the temp is gone.
const backupWriter = fnBody(
  productionRustCode(readFileSync(join(root, BACKUP), 'utf8')),
  'run_backup_once_with_ops',
);
if (!backupWriter) {
  failures.push(
    `${BACKUP}: \`run_backup_once_with_ops\` is gone — the snapshot is written somewhere this `
      + 'check can no longer see.',
  );
} else {
  const narrowed = backupWriter.indexOf('restrict_to_owner_async');
  const renamed = backupWriter.indexOf('rename');
  if (narrowed < 0) {
    failures.push(
      `${BACKUP}: the snapshot is no longer narrowed with `
        + '`rg_core::platform::fs::restrict_to_owner_async`. `VACUUM INTO` writes it at the '
        + 'ambient umask, so without this the whole database lands `0644` in a directory the '
        + 'server does not narrow when an operator created it.',
    );
  } else if (renamed >= 0 && narrowed > renamed) {
    failures.push(
      `${BACKUP}: the snapshot is narrowed *after* the rename, so the final name exists at the `
        + 'ambient umask for the width of that window — narrow the temporary name first.',
    );
  }
}

// ---------------------------------------------------------------------------
// The process boundary for non-secret state created by third-party writers.
// ---------------------------------------------------------------------------
//
// Git, gix, SQLite, CI job processes and registry libraries do not all expose
// a mode-bearing open to our code. The two long-running entrypoints therefore
// install one process policy before their first write. Ordering is load-bearing:
// moving the call below the log appender or the runner's startup sweep would
// leave early files at the launcher-provided umask while every later assertion
// still passed.

const PROCESS = 'crates/rg-process/src/lib.rs';
const CLI_SERVE = 'crates/rg-cli/src/serve.rs';
const CLI_MAIN = 'crates/rg-cli/src/main.rs';
const CLI_COMMANDS = 'crates/rg-cli/src/commands.rs';
const CLI_MODEL = 'crates/rg-cli/src/cli.rs';
const RUNNER_COMMANDS = 'crates/rg-runner/src/commands.rs';

const processCode = productionRustCode(readFileSync(join(root, PROCESS), 'utf8'));
if (!/\bfn install\s*\(\s*self\s*\)[\s\S]*?libc::umask\s*\(\s*self\.umask\s*\(\s*\)/.test(processCode)) {
  failures.push(
    `${PROCESS}: \`StateCreationPermissions::install\` no longer installs the process umask; `
      + 'Git/gix and child processes have no shared file-creation boundary.',
  );
}
for (const [variant, mask] of [
  ['OwnerOnly', '0o077'],
  ['GroupReadable', '0o027'],
  ['GroupWritable', '0o007'],
]) {
  if (!processCode.includes(`Self::${variant} => ${mask}`)) {
    failures.push(
      `${PROCESS}: ${variant} no longer maps to ${mask}; the documented file/directory modes drifted.`,
    );
  }
}

const serveWriter = fnBody(
  productionRustCode(readFileSync(join(root, CLI_SERVE), 'utf8')),
  'run_serve',
);
if (!serveWriter) {
  failures.push(`${CLI_SERVE}: \`run_serve\` is gone — server startup policy cannot be read.`);
} else {
  const installed = serveWriter.indexOf('resolved_state_permissions.install');
  const firstWrite = serveWriter.indexOf('RollingFileAppender::builder');
  if (installed < 0) {
    failures.push(`${CLI_SERVE}: \`run_serve\` no longer installs resolved state permissions.`);
  } else if (firstWrite < 0 || installed > firstWrite) {
    failures.push(
      `${CLI_SERVE}: state permissions must be installed before the log appender performs the `
        + 'server startup\'s first file creation.',
    );
  }
}

const runnerWriter = fnBody(
  productionRustCode(readFileSync(join(root, RUNNER_COMMANDS), 'utf8')),
  'cmd_run',
);
if (!runnerWriter) {
  failures.push(`${RUNNER_COMMANDS}: \`cmd_run\` is gone — runner startup policy cannot be read.`);
} else {
  const installed = runnerWriter.indexOf('state_permissions.install');
  const firstWrite = runnerWriter.indexOf('sweep_stale_job_entries');
  if (installed < 0) {
    failures.push(`${RUNNER_COMMANDS}: \`cmd_run\` no longer installs resolved state permissions.`);
  } else if (firstWrite < 0 || installed > firstWrite) {
    failures.push(
      `${RUNNER_COMMANDS}: state permissions must be installed before the startup sweep and any `
        + 'runner-owned file creation.',
    );
  }
}

// One-shot commands have a different ownership boundary: server-state writers
// install the policy, while `backup-db` preserves the mode of the output path
// the operator selected. The exhaustive Rust match makes every future command
// choose a side; these anchors keep the current classification and ordering
// load-bearing instead of trusting a comment beside the enum.
const oneShotCommandsCode = productionRustCode(readFileSync(join(root, CLI_COMMANDS), 'utf8'));
const oneShotPreparation = fnBody(oneShotCommandsCode, 'prepare_state_writer');
if (!oneShotPreparation) {
  failures.push(
    `${CLI_COMMANDS}: \`prepare_state_writer\` is gone — one-shot state policy cannot be read.`,
  );
} else {
  const loaded = oneShotPreparation.indexOf('load_optional_config_file');
  const installed = oneShotPreparation.indexOf('permissions.install');
  const returned = oneShotPreparation.indexOf('StateWriterConfig(cfg)');
  if (loaded < 0 || installed < 0 || returned < 0 || !(loaded < installed && installed < returned)) {
    failures.push(
      `${CLI_COMMANDS}: one-shot commands must load config once, install its state policy, then `
        + 'return that same config to the handler.',
    );
  }
}

const cliMain = fnBody(
  productionRustCode(readFileSync(join(root, CLI_MAIN), 'utf8')),
  'main',
);
if (!cliMain) {
  failures.push(`${CLI_MAIN}: \`main\` is gone — one-shot process ordering cannot be read.`);
} else {
  const preparedMatch = /\bprepare_state_writer\s*\(\s*&cli\.command\s*\)/.exec(cliMain);
  const prepared = preparedMatch?.index ?? -1;
  const dispatched = cliMain.indexOf('match cli.command');
  if (prepared < 0 || dispatched < 0 || prepared > dispatched) {
    failures.push(
      `${CLI_MAIN}: one-shot state policy must be prepared before command dispatch can create state.`,
    );
  }
}

const cliModelCode = productionRustCode(readFileSync(join(root, CLI_MODEL), 'utf8'));
const classifierStart = cliModelCode.indexOf('fn state_creation_contract');
const classifier = classifierStart < 0 ? null : bracedBodyAt(cliModelCode, classifierStart);
if (!classifier) {
  failures.push(
    `${CLI_MODEL}: exhaustive \`state_creation_contract\` is gone — new commands can bypass `
      + 'the ownership decision.',
  );
} else {
  for (const variant of [
    'Migrate',
    'RotateInstanceKey',
    'RotateEncryptionKey',
    'RebuildFts',
    'RestoreDb',
    'CreateRepo',
    'Import',
    'IndexRepo',
  ]) {
    if (!classifier.includes(`Self::${variant}`)) {
      failures.push(`${CLI_MODEL}: ${variant} is no longer classified as a one-shot state writer.`);
    }
  }
  if (!classifier.includes('PackageCmd::List')) {
    failures.push(`${CLI_MODEL}: package list can run migrations but is not classified explicitly.`);
  }
  if (!/Self::BackupDb[\s\S]*?StateCreationContract::OperatorOwnedOutput/.test(classifier)) {
    failures.push(
      `${CLI_MODEL}: backup-db must preserve the creation contract of its operator-selected output.`,
    );
  }
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`❌ ${failure}`);
  process.exit(1);
}

console.log(
  `✅ secret file mode: ${sources.length} production Rust sources create no secret wide; the SSH `
    + 'host key, the audit archive and the backup snapshot hold owner-only anchors; server and runner '
    + 'install their state-creation policy before the first write; one-shot commands classify state '
    + 'ownership and install the same policy before dispatch',
);
