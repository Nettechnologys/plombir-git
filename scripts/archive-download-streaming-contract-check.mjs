#!/usr/bin/env node

// A repository archive must never be collected into memory, and never produced
// by a blocking call inside an async handler.
//
// Why this exists: `GET /repos/{owner}/{name}/archive/{sha}.zip` — the public
// "Download ZIP / tar.gz" button — built its answer with
// `GitCommandGateway::run`, one call carrying two defects (card_fbdae59573ca).
// `run` returns `GitOutput { stdout: Vec<u8> }`, so the whole archive was
// materialised in server memory and handed to `Body::from` as a single frame,
// at a size chosen by whoever clicked the button rather than by any configured
// limit. And `run` is *synchronous* — `recv_timeout` on the calling thread —
// so each request also parked a tokio worker for up to `git_cmd_secs`.
//
// Both halves are one line away from coming back: `gateway.run(&["archive", …])`
// is the obvious spelling, it is shorter than the streaming path, and the
// buffered response helper sits in the same crate, correctly used by the
// downloads that really do need their payload whole. So this asserts the shape.
//
// The second surface is the shared builder itself. Moving the streaming into
// `http_stream::git_child_body_with_idle` means one regression there silently
// un-bounds both git downloads at once, so its two idle points and its
// late-failure verdict are asserted here too, and so is the runner workspace
// download that shares it — a builder with one caller left is a builder halfway
// back to being inlined.
//
// Truth boundary: every Rust file is read through `productionRustSource`, so a
// `gateway.run` inside a `#[cfg(test)]` fixture (the byte-parity test really
// does buffer one, on purpose, to compare against) cannot make this go red, and
// a test double cannot satisfy a requirement on production code.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode, productionRustSource, rustFnBlock } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.PLOMBIR_GIT_ARCHIVE_STREAMING_ROOT || join(scriptsDir, '..'));
const failures = [];

const ARCHIVE = 'crates/rg-http/src/api/archive.rs';
const RUNNERS = 'crates/rg-http/src/api/runners.rs';
const HTTP_STREAM = 'crates/rg-http/src/http_stream.rs';

function read(surface) {
  try {
    return readFileSync(join(root, surface), 'utf8');
  } catch (error) {
    failures.push(`${surface}: cannot be read (${error.code ?? error.message})`);
    return null;
  }
}

/**
 * The body of a top-level `fn <name>` in the production view, or `null`.
 *
 * `rustFnBlock` reads only `pub`/`pub(crate) async fn`, and two of the
 * functions this has to read inside are neither — the shared streaming builder
 * is synchronous, and the pump under it is private. Same anchoring rule: a
 * top-level item at column 0 closed by a `}` at column 0, and `null` when the
 * form is one this cannot read, so an unreadable function goes red instead of
 * silently passing.
 */
function fnBody(source, name) {
  const structure = productionRustCode(source);
  const text = productionRustSource(source);
  const start = structure.search(
    new RegExp(`^(?:pub(?:\\([^)]*\\))?\\s+)?(?:async\\s+)?fn ${name}\\s*(?:<[^>]*>)?\\s*\\(`, 'm'),
  );
  if (start < 0) return null;
  const close = structure.slice(start).search(/\n\}/);
  if (close < 0) return null;
  return text.slice(start, start + close + 2);
}

// ── the public archive endpoint streams, and decides its 400 before it does ──

const archive = read(ARCHIVE);
if (archive !== null) {
  const production = productionRustSource(archive);

  if (production.includes('buffered_body_with_idle')) {
    failures.push(
      `${ARCHIVE}: hands its response to \`buffered_body_with_idle\`, which takes an `
        + 'already-collected `Vec` — an archive is sized by the repository and must never be '
        + 'collected at all',
    );
  }

  const handler = rustFnBlock(archive, 'download_archive');
  if (handler === null) {
    failures.push(
      `${ARCHIVE}: \`download_archive\` is not in a form this check can read — the download `
        + 'endpoint must stay readable or this assertion silently stops asserting',
    );
  } else {
    // The blocking call, in the two spellings it comes back as. Naming the
    // gateway is not decoration: the defect was not "a Vec appeared", it was
    // "the synchronous gateway was called from an async handler", and that call
    // produces the Vec as a side effect.
    for (const blocking of ['git.run(', 'gateway.run(', 'GitCommandGateway::run']) {
      if (handler.body.includes(blocking)) {
        failures.push(
          `${ARCHIVE}: \`download_archive\` calls \`${blocking}\` — the synchronous gateway `
            + 'blocks a tokio worker for up to `git_cmd_secs` and returns the whole archive as a '
            + '`Vec`; spawn git with `spawn_async` and stream its stdout instead',
        );
      }
    }
    if (!handler.body.includes('spawn_async')) {
      failures.push(
        `${ARCHIVE}: \`download_archive\` never spawns git with \`spawn_async\`, so its output `
          + 'is being produced by something that has already finished — which means it is whole, '
          + 'in memory, before the first byte is sent',
      );
    }
    if (!handler.body.includes('git_child_body_with_idle')) {
      failures.push(
        `${ARCHIVE}: \`download_archive\` does not build its body with `
          + '`http_stream::git_child_body_with_idle`, so nothing bounds how much of the archive '
          + 'this endpoint holds or how long a stalled client may hold it',
      );
    }
    // Streaming must not cost the endpoint its one honest 4xx. The status is
    // spent once a byte is on the wire, so the classification has to happen on
    // the first read — a handler that answers 200 first can only ever report a
    // bad ref as a broken download.
    for (const marker of ['is_bad_tree_ish', 'BAD_TREE_ISH']) {
      if (!handler.body.includes(marker)) {
        failures.push(
          `${ARCHIVE}: \`download_archive\` no longer names \`${marker}\`, so the \`400\` for a `
            + 'ref git cannot resolve is not being decided before the response begins',
        );
      }
    }
  }
}

// ── the runner workspace download shares that builder ───────────────────────

const runners = read(RUNNERS);
if (runners !== null) {
  const workspace = rustFnBlock(runners, 'download_workspace');
  if (workspace === null) {
    failures.push(
      `${RUNNERS}: \`download_workspace\` is not in a form this check can read — the other `
        + 'consumer of the shared streaming builder must stay readable',
    );
  } else if (!workspace.body.includes('git_child_body_with_idle')) {
    failures.push(
      `${RUNNERS}: \`download_workspace\` no longer streams through `
        + '`http_stream::git_child_body_with_idle` — a workspace tar is sized by the repository '
        + 'just as the public archive is',
    );
  }
}

// ── the shared builder keeps both idle points and its late-failure verdict ───

const httpStream = read(HTTP_STREAM);
if (httpStream !== null) {
  const builder = fnBody(httpStream, 'git_child_body_with_idle');
  if (builder === null) {
    failures.push(
      `${HTTP_STREAM}: \`git_child_body_with_idle\` is missing or unreadable — both git `
        + 'downloads build their body with it, so it has no bounded builder left',
    );
  } else {
    if (!builder.includes('pump_child_stdout')) {
      failures.push(
        `${HTTP_STREAM}: \`git_child_body_with_idle\` no longer forwards git's stdout through `
          + '`pump_child_stdout`, so whatever it does instead is unaccounted for by the idle '
          + 'assertions below',
      );
    }
    // A git that fails after the first byte cannot take the status back, so the
    // body must break. Ending it cleanly hands the client a well-formed,
    // complete-looking, truncated archive — worse than the buffering this
    // replaced, because it fails silently.
    if (!builder.includes('tx.send(Err(')) {
      failures.push(
        `${HTTP_STREAM}: \`git_child_body_with_idle\` never yields an error into the body, so a `
          + 'git that exits non-zero mid-response ends the stream as if the payload were '
          + 'complete — a truncated archive that looks like a whole download',
      );
    }
  }

  const pump = fnBody(httpStream, 'pump_child_stdout');
  if (pump === null) {
    failures.push(
      `${HTTP_STREAM}: \`pump_child_stdout\` is missing or unreadable — the two idle points that `
        + 'bound a git download live in it',
    );
  } else {
    // The read bound catches a hung git: an async-spawned one has no
    // `git_cmd_secs` wall-clock behind it, unlike the synchronous gateway call
    // this path replaced.
    if (!pump.includes('tokio::time::timeout')) {
      failures.push(
        `${HTTP_STREAM}: \`pump_child_stdout\` does not bound its read from git — an `
          + 'async-spawned git has no wall-clock timeout, so a hung one holds the request and '
          + 'its pipes open forever',
      );
    }
    // The send bound catches a stalled client: hyper stops draining, the
    // bounded channel fills, and the blocked `send` is where that becomes
    // observable to us.
    if (!pump.includes('send_chunk')) {
      failures.push(
        `${HTTP_STREAM}: \`pump_child_stdout\` does not hand its chunks to \`send_chunk\`, so `
          + 'nothing bounds a client that stops reading half way through an archive',
      );
    }
  }
}

if (failures.length > 0) {
  console.error('❌ archive download streaming contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ archive download streaming contract: a repository archive is never collected');
