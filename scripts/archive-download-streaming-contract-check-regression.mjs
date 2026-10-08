#!/usr/bin/env node

// Mutation stand for archive-download-streaming-contract-check.mjs.
//
// A green repository only proves the defect is absent today. These fixtures put
// it back — each in one of the shapes the original had (card_fbdae59573ca): the
// synchronous `gateway.run` inside the async handler, a body built from a
// finished `Vec`, a streaming handler that gave up its `400` on the way, one of
// the two idle points dropped from the shared pump, and the late-failure
// verdict that keeps a truncated archive from reading as a whole one.
//
// The floors matter as much as the mutations. A check reading the wrong tree,
// or one that stopped parsing its subject, reports success forever; so the
// baseline must pass, and a `gateway.run` planted inside a `#[cfg(test)]` item
// must *not* go red — the byte-parity test really does buffer one on purpose,
// to compare the stream against, and that is the only evidence the check reads
// the production view rather than raw bytes.

import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'archive-download-streaming-contract-check.mjs');

const ARCHIVE = 'crates/rg-http/src/api/archive.rs';
const RUNNERS = 'crates/rg-http/src/api/runners.rs';
const HTTP_STREAM = 'crates/rg-http/src/http_stream.rs';

/** The shape the repository actually has: streaming on every leg. */
const BASELINE = {
  [ARCHIVE]: `const BAD_TREE_ISH: &str = "invalid ref or SHA";

fn is_bad_tree_ish(stderr: &str) -> bool {
    stderr.contains("not a valid object name")
}

pub async fn download_archive(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let child = match git
        .spawn_async(&["archive", &format!("--format={}", format_flag), &sha], Some(&repo_path))
        .await
    {
        Ok(child) => child,
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut stream = match crate::http_stream::split_git_child(child) {
        Ok(stream) => stream,
        Err(error) => return AppError::from(error).into_response(),
    };
    if first == 0 {
        let error = if is_bad_tree_ish(&stderr) {
            error.context(rg_core::error::InvalidRequest::new(BAD_TREE_ISH))
        } else {
            error.context("git archive failed")
        };
        return AppError::from(error).into_response();
    }
    let body = crate::http_stream::git_child_body_with_idle(stream, idle_secs, source);
    archive_response(&name, &sha, ext, mime, body)
}
`,
  [RUNNERS]: `pub async fn download_workspace(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let stream = match crate::http_stream::split_git_child(child) {
        Ok(stream) => stream,
        Err(error) => return AppError::internal(format!("{error}")).into_response(),
    };
    (
        StatusCode::OK,
        crate::http_stream::git_child_body_with_idle(stream, state.git_idle_timeout_secs, source),
    )
        .into_response()
}
`,
  [HTTP_STREAM]: `pub(crate) fn git_child_body_with_idle(
    stream: GitChildStream,
    idle_secs: u64,
    source: GitStreamSource,
) -> Body {
    tokio::spawn(async move {
        let ended = pump_child_stdout(&mut stdout, &tx, head, idle, idle_secs, &source).await;
        if ended == PumpEnd::Eof {
            drop(tx.send(Err(std::io::Error::other("git exited non-zero mid-response"))).await);
        }
    });
    Body::new(http_body_util::StreamBody::new(frame_stream))
}

async fn pump_child_stdout<R>(
    stdout: &mut R,
    tx: &tokio::sync::mpsc::Sender<std::io::Result<axum::body::Bytes>>,
    head: axum::body::Bytes,
    idle: Option<std::time::Duration>,
    idle_secs: u64,
    source: &GitStreamSource,
) -> PumpEnd
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let read = match idle {
            Some(dur) => tokio::time::timeout(dur, stdout.read(&mut buf)).await,
            None => stdout.read(&mut buf).await,
        };
        if !send_chunk(tx, chunk, idle, idle_secs).await {
            return PumpEnd::Aborted;
        }
    }
}
`,
};

function writeFixture(overrides = {}) {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-archive-streaming-'));
  for (const [surface, body] of Object.entries({ ...BASELINE, ...overrides })) {
    const target = join(fixture, surface);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, body);
  }
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    env: { ...process.env, PLOMBIR_GIT_ARCHIVE_STREAMING_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const failures = [];

function expect(label, overrides, { red, mentions = [] }) {
  const fixture = writeFixture(overrides);
  try {
    const { status, output } = run(fixture);
    if (red && status === 0) {
      failures.push(`${label}: the check stayed green on a mutated fixture — it asserts nothing`);
      return;
    }
    if (!red && status !== 0) {
      failures.push(`${label}: the check went red on a fixture it should accept:\n${output}`);
      return;
    }
    for (const fragment of mentions) {
      if (!output.includes(fragment)) {
        failures.push(`${label}: red, but the message never names \`${fragment}\`:\n${output}`);
      }
    }
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// ── Floor: the shape the repository has must pass ───────────────────────────
expect('baseline', {}, { red: false });

// ── The original defect, in the spelling it had ─────────────────────────────
expect(
  'the synchronous gateway is back in the async handler',
  {
    [ARCHIVE]: BASELINE[ARCHIVE]
      .replace(
        `    let child = match git
        .spawn_async(&["archive", &format!("--format={}", format_flag), &sha], Some(&repo_path))
        .await
    {`,
        `    let child = match git.run(
        &["archive", &format!("--format={}", format_flag), &sha],
        Some(&repo_path),
    ) {`,
      ),
  },
  { red: true, mentions: ['git.run(', 'blocks a tokio worker'] },
);

expect(
  'the body is built from a finished Vec again',
  {
    [ARCHIVE]: BASELINE[ARCHIVE].replace(
      '    let body = crate::http_stream::git_child_body_with_idle(stream, idle_secs, source);',
      '    let body = Body::from(output);',
    ),
  },
  { red: true, mentions: ['git_child_body_with_idle'] },
);

expect(
  'the buffered helper is reached for instead',
  {
    [ARCHIVE]: BASELINE[ARCHIVE].replace(
      '    let body = crate::http_stream::git_child_body_with_idle(stream, idle_secs, source);',
      '    let body = crate::http_stream::buffered_body_with_idle(output, idle_secs);',
    ),
  },
  { red: true, mentions: ['buffered_body_with_idle'] },
);

// ── Streaming must not cost the endpoint its one honest 4xx ─────────────────
expect(
  'the bad-ref classification is dropped on the way to streaming',
  {
    [ARCHIVE]: BASELINE[ARCHIVE].replace(
      `        let error = if is_bad_tree_ish(&stderr) {
            error.context(rg_core::error::InvalidRequest::new(BAD_TREE_ISH))
        } else {
            error.context("git archive failed")
        };`,
      '        let error = error.context("git archive failed");',
    ),
  },
  { red: true, mentions: ['is_bad_tree_ish'] },
);

// ── The shared builder must keep both consumers and both bounds ─────────────
expect(
  'the runner workspace download stops sharing the builder',
  {
    [RUNNERS]: BASELINE[RUNNERS].replace(
      'crate::http_stream::git_child_body_with_idle(stream, state.git_idle_timeout_secs, source),',
      'Body::from(output),',
    ),
  },
  { red: true, mentions: ['download_workspace'] },
);

expect(
  'the read idle bound is dropped, so a hung git holds the request forever',
  {
    [HTTP_STREAM]: BASELINE[HTTP_STREAM].replace(
      `        let read = match idle {
            Some(dur) => tokio::time::timeout(dur, stdout.read(&mut buf)).await,
            None => stdout.read(&mut buf).await,
        };`,
      '        let read = stdout.read(&mut buf).await;',
    ),
  },
  { red: true, mentions: ['does not bound its read from git'] },
);

expect(
  'the send idle bound is dropped, so a stalled client is unbounded',
  {
    [HTTP_STREAM]: BASELINE[HTTP_STREAM].replace(
      `        if !send_chunk(tx, chunk, idle, idle_secs).await {
            return PumpEnd::Aborted;
        }`,
      `        if tx.send(Ok(chunk)).await.is_err() {
            return PumpEnd::Aborted;
        }`,
    ),
  },
  { red: true, mentions: ['send_chunk'] },
);

expect(
  'a late git failure ends the body cleanly again',
  {
    [HTTP_STREAM]: BASELINE[HTTP_STREAM].replace(
      `        if ended == PumpEnd::Eof {
            drop(tx.send(Err(std::io::Error::other("git exited non-zero mid-response"))).await);
        }`,
      '        tracing::warn!("git exited non-zero after the response had begun");',
    ),
  },
  { red: true, mentions: ['never yields an error into the body'] },
);

// ── Parse floors: an unreadable subject must go red, not vacuously green ────
expect(
  'the handler is renamed out from under the check',
  { [ARCHIVE]: BASELINE[ARCHIVE].replace('pub async fn download_archive(', 'pub async fn serve_archive(') },
  { red: true, mentions: ['not in a form this check can read'] },
);

expect(
  'the shared builder is renamed out from under the check',
  {
    [HTTP_STREAM]: BASELINE[HTTP_STREAM].replace(
      'pub(crate) fn git_child_body_with_idle(',
      'pub(crate) fn git_child_body(',
    ),
  },
  { red: true, mentions: ['missing or unreadable'] },
);

// ── Production view: a buffered `run` in a test fixture is not the defect ────
expect(
  'a gateway.run inside a #[cfg(test)] item stays green',
  {
    [ARCHIVE]: `${BASELINE[ARCHIVE]}
#[cfg(test)]
mod tests {
    use super::*;

    /// The streamed archive must be byte-identical to a buffered one, so the
    /// test buffers a reference copy on purpose.
    #[tokio::test]
    async fn streamed_archive_matches_buffered_git_archive() {
        let buffered = gw().run(&["archive", "--format=tar", "HEAD"], Some(repo)).unwrap();
        assert_eq!(streamed, buffered.stdout);
    }
}
`,
  },
  { red: false },
);

if (failures.length > 0) {
  console.error('❌ archive download streaming stand failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  '✅ archive download streaming stand: the check goes red on every shape of the defect',
);
