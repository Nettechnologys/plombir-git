#!/usr/bin/env node

// A clone must never be held in memory — not by rg-git, not by rg-http.
//
// Why this exists: `git pack-objects --stdout` streams the pack out as it
// builds it, and both upload-pack dialects used to undo that with
// `read_to_end` into a `Vec`. The HTTP transport then drained the duplex into a
// *second* `Vec` before answering, so one `git clone` of a 2 GiB repository
// peaked at roughly 4 GiB of server memory — a figure chosen by whoever ran the
// clone, not by any configured limit (card_73f02e2a97ad).
//
// The fix is structural: `crates/rg-git/src/protocol/pack_stream.rs` is the one
// place that reads pack bytes, and it moves them a chunk at a time; rg-http
// streams that duplex straight to the socket. Both halves are one line away
// from being undone — `read_to_end` is the obvious spelling, and the buffered
// response helper is still right there in the same crate, correctly used by the
// download handlers that really do need their payload whole. So this check
// asserts the shape rather than trusting the comments.
//
// Why here and not only in Rust: the workspace test suite runs in no gate that
// precedes a push (`rust` is recorded `uncovered` in
// `scripts/run-local-gates.mjs`, and regression.yml has never executed). The
// Rust tests beside the code carry the behaviour — a pack larger than every
// internal window arrives whole, a late `pack-objects` failure breaks the body
// instead of ending it cleanly — and this carries the teeth.
//
// Truth boundary: every Rust file is read through `productionRustSource`, so a
// `read_to_end` inside a `#[cfg(test)]` fixture or a comment cannot make this
// go red, and — the half that matters more — a test double cannot satisfy a
// requirement on production code.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode, productionRustSource, rustFnBlock } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.FORGEKEEP_PACK_STREAMING_ROOT || join(scriptsDir, '..'));
const failures = [];

const PACK_STREAM = 'crates/rg-git/src/protocol/pack_stream.rs';
const UPLOAD_PACK = 'crates/rg-git/src/protocol/upload_pack.rs';
const V2 = 'crates/rg-git/src/protocol/v2.rs';
const GIT_HTTP = 'crates/rg-http/src/git_http.rs';

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
 * `rustFnBlock` reads only `pub`/`pub(crate) async fn`, and the two functions
 * this check has to read inside are deliberately private — a streaming
 * response builder and a response drainer, neither of which anything outside
 * its module may call. Same anchoring rule as the shared helper: a top-level
 * item at column 0, closed by a `}` at column 0, and `null` when the form is
 * one this cannot read, so an unreadable function goes red instead of silently
 * passing.
 */
function privateFnBody(source, name) {
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

/** Count non-overlapping occurrences of a literal. */
function occurrences(haystack, needle) {
  return haystack.split(needle).length - 1;
}

// ── rg-git: one reader of pack bytes, and it does not collect them ──────────

const packStream = read(PACK_STREAM);
if (packStream !== null) {
  const production = productionRustSource(packStream);
  if (occurrences(production, 'read_to_end') > 0) {
    failures.push(
      `${PACK_STREAM}: collects a stream with \`read_to_end\` — this module is the one place `
        + 'pack (and pack-objects stderr) bytes are read, so an unbounded read here is the whole '
        + 'defect coming back through its own fix',
    );
  }
  if (!production.includes('write_sideband_error')) {
    failures.push(
      `${PACK_STREAM}: never sends a band-3 error, so a \`pack-objects\` failure discovered after `
        + 'the pack started flowing reaches the client as an unexplained short transfer',
    );
  }
}

// Both dialects must generate their pack through that one reader. Naming the
// call is not decoration: a handler that spawns `pack-objects` itself is free
// to read it any way it likes, and the assertion above would never see it.
for (const surface of [UPLOAD_PACK, V2]) {
  const source = read(surface);
  if (source === null) continue;
  const production = productionRustSource(source);

  if (occurrences(production, 'read_to_end') > 0) {
    failures.push(
      `${surface}: reads a subprocess stream with \`read_to_end\` — the outgoing pack must move `
        + 'through `pack_stream::stream_pack_objects` a chunk at a time, never into a `Vec` sized '
        + 'by the repository',
    );
  }
  if (!production.includes('pack_stream::stream_pack_objects')) {
    failures.push(
      `${surface}: does not generate its pack through \`pack_stream::stream_pack_objects\`, so `
        + 'nothing bounds how much of a clone this dialect holds',
    );
  }
}

// ── rg-http: the transport does not make a second copy ──────────────────────

const gitHttp = read(GIT_HTTP);
if (gitHttp !== null) {
  const production = productionRustSource(gitHttp);

  // `buffered_body_with_idle` is the right helper for a payload that is already
  // whole (artifacts, attachments, packages hash theirs before serving a byte).
  // It is the wrong one here, and its presence in this file means somebody
  // buffered a clone to reach for it.
  if (production.includes('buffered_body_with_idle')) {
    failures.push(
      `${GIT_HTTP}: hands a git response to \`buffered_body_with_idle\`, which takes an `
        + 'already-collected `Vec` — a clone is sized by the repository and must be streamed with '
        + '`reader_body_with_idle` instead',
    );
  }
  if (!production.includes('reader_body_with_idle')) {
    failures.push(
      `${GIT_HTTP}: never builds a streaming response body, so the upload-pack response is being `
        + 'materialised somewhere before it is sent',
    );
  }

  // The one drain left in this file belongs to receive-pack, whose response is
  // a per-ref status report bounded by the push's own ref count. Counting it
  // rather than allowing the name file-wide is what stops the drain quietly
  // spreading back onto the clone path.
  const drains = occurrences(production, 'read_to_end');
  const drainer = privateFnBody(gitHttp, 'spawn_git_response_reader');
  if (drainer === null) {
    failures.push(
      `${GIT_HTTP}: \`spawn_git_response_reader\` is not in a form this check can read — the `
        + 'receive-pack drain has to be readable for its `read_to_end` to be accounted for',
    );
  } else {
    const accounted = occurrences(drainer, 'read_to_end');
    if (drains > accounted) {
      failures.push(
        `${GIT_HTTP}: ${drains} \`read_to_end\` call(s) in production code but only ${accounted} `
          + 'inside `spawn_git_response_reader` — every other one drains something whose size the '
          + 'client chooses',
      );
    }
  }

  // Door-level, not file-level: the clone endpoint itself must reach the
  // streaming path. A file that merely *contains* the streaming helper says
  // nothing about which handler uses it.
  const handler = rustFnBlock(gitHttp, 'handle_git_upload_pack');
  if (handler === null) {
    failures.push(
      `${GIT_HTTP}: \`handle_git_upload_pack\` is not in a form this check can read — the clone `
        + 'endpoint must stay readable or this assertion silently stops asserting',
    );
  } else {
    if (!handler.body.includes('stream_upload_pack_response')) {
      failures.push(
        `${GIT_HTTP}: \`handle_git_upload_pack\` no longer answers through `
          + '`stream_upload_pack_response`, so the clone body is being produced some other way',
      );
    }
    if (handler.body.includes('spawn_git_response_reader')) {
      failures.push(
        `${GIT_HTTP}: \`handle_git_upload_pack\` drains its response with `
          + '`spawn_git_response_reader` — that collects the whole pack before the first byte '
          + 'reaches the client',
      );
    }
  }

  // The streaming builder is where the status/stream split lives: it reads the
  // first chunk so a failure before any output is still a 500, and it must not
  // grow a collect of its own.
  const streamer = privateFnBody(gitHttp, 'stream_upload_pack_response');
  if (streamer === null) {
    failures.push(
      `${GIT_HTTP}: \`stream_upload_pack_response\` is missing or unreadable — the upload-pack `
        + 'response has no bounded builder',
    );
  } else if (!streamer.includes('reader_body_with_idle')) {
    failures.push(
      `${GIT_HTTP}: \`stream_upload_pack_response\` does not build its body with `
        + '`reader_body_with_idle`, so nothing keeps the response bounded or breaks it when the '
        + 'generator fails late',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ git pack streaming contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ git pack streaming contract: the outgoing pack is never collected');
