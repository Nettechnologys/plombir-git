#!/usr/bin/env node

// The documented data directory must be created owner-only.
//
// Why this exists: every quick-start told the operator `mkdir -p data`, which
// takes the ambient umask — 0755 on a stock host. Everything the server
// persists lands below that one directory: the bare clones of every *private*
// repository, `plombir-git.db` with its e-mail addresses and argon2 hashes, the
// `VACUUM INTO` snapshots of it (0644 by construction), the audit archive, LFS
// objects and OCI layers. So a single missing `-m 700` handed all of it to
// every other local account on the host, and no amount of care in the writers
// below took that back.
//
// This is checked here, and not only by the Rust doc-contract test beside
// `every_documented_env_install_creates_an_owner_only_file`, because the
// workspace test suite is not part of any gate that runs before a push: the
// `rust` job of regression.yml is recorded `uncovered` in
// `scripts/run-local-gates.mjs`, and regression.yml itself has never executed.
// A fact nothing runs is not guarded. The Rust test keeps its own value — its
// `include_str!` binds the document paths at compile time — but the teeth are
// here.
//
// Truth boundary: this reads the documents as bytes. The instruction in
// `docker-compose.hostdir.yml` lives in a YAML *comment* — it is the quick-start
// a reader of that file follows — so a production-view parse would correctly
// see nothing at all. What an operator copy-pastes is the subject, so the raw
// text is the right view here and the wrong one for a check about behaviour.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { yamlAnnotatedLines } from './lib/yaml-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.PLOMBIR_GIT_DATA_DIR_MODE_ROOT || join(scriptsDir, '..'));
const failures = [];

// The surfaces that tell an operator to create the host data directory, and the
// spelling that creates it owner-only in one step. `install -d -m 700` rather
// than `mkdir` + `chmod`: a two-command form has a window in which the
// directory exists wide, and the second half is the one that gets dropped when
// somebody shortens the instructions.
const SAFE_CREATE = 'install -d -m 700 data';
const UNSAFE_CREATE = 'mkdir -p data';

// `deploy/docker-compose.hostdir.yml` carries its quick-start in YAML
// *comments*, which is where this claim genuinely lives: a production view of
// that file holds nothing about creating a directory at all. So it is read the
// way the parser splits it and the comment halves are the subject — not the raw
// bytes, which would also match a `#` inside a quoted scalar.
function quickStartText(surface, body) {
  if (!surface.endsWith('.yml')) return body;
  return yamlAnnotatedLines(body)
    .map(({ comment }) => comment ?? '')
    .join('\n');
}

const QUICK_START_SURFACES = ['deploy/README.md', 'deploy/docker-compose.hostdir.yml'];

for (const surface of QUICK_START_SURFACES) {
  const body = quickStartText(surface, readFileSync(join(root, surface), 'utf8'));

  if (!body.includes(SAFE_CREATE)) {
    failures.push(`${surface}: does not create the data directory owner-only (\`${SAFE_CREATE}\`)`);
  }
  if (body.includes(UNSAFE_CREATE)) {
    failures.push(
      `${surface}: recommends \`${UNSAFE_CREATE}\`, which takes the ambient umask — the private `
        + 'repositories, the database and its backups below it become readable by every other '
        + 'local account',
    );
  }
}

// The named-volume deploy (`docker-compose.yml`) never runs the quick-start
// above: Docker seeds a fresh volume from the image path, modes included, so
// the Dockerfile is that layout's only chance to get `/data` right.
//
// Comment lines are stripped first, and that is not a detail: the `RUN` line is
// introduced by a comment explaining it, and a raw `includes` was satisfied by
// the explanation alone — deleting the instruction left this check green. The
// two documents above are prose all the way down and are read as written; a
// Dockerfile has an executable half, and this claim is about that half.
const dockerfile = readFileSync(join(root, 'Dockerfile'), 'utf8')
  .split('\n')
  .filter((line) => !line.trimStart().startsWith('#'))
  .join('\n');
if (!dockerfile.includes('chmod 700 /data')) {
  failures.push(
    'Dockerfile: /data is created but never narrowed, so a fresh named volume inherits '
      + "`mkdir`'s 0755 along with the repositories and the database inside it",
  );
}

// A troubleshooting table that answers a permission error with `chmod 755` un-
// does all of the above, and it is the shape those tables drift into: widening
// makes the symptom go away. The host-key row already says `chmod 600`, so the
// direction is established — this keeps it.
// The mode is read as a number rather than matched as a pattern: `chmod 700
// data` and `chmod 755 data` differ only in digits a regex is easy to get
// backwards, and getting it backwards here means flagging the safe form.
const CHMOD_ON_DATA = /chmod\s+(?:-R\s+)?([0-7]{3,4})\s+(?:\.\/)?(?:[\w./-]*\/)?data\b/g;
for (const surface of ['deploy/README.md', 'README.md']) {
  const body = readFileSync(join(root, surface), 'utf8');
  for (const widened of body.matchAll(CHMOD_ON_DATA)) {
    if ((Number.parseInt(widened[1], 8) & 0o077) === 0) continue;
    const line = body.slice(0, widened.index).split('\n').length;
    failures.push(
      `${surface}:${line}: remediation \`${widened[0]}\` widens the data directory back open`,
    );
  }
}

if (failures.length > 0) {
  console.error('❌ deploy data-directory mode contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  '✅ deploy data-directory mode contract: every documented layout creates the data root owner-only',
);
