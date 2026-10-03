#!/usr/bin/env node

// Mutation stand for git-pack-streaming-contract-check.mjs.
//
// A green repository only proves the defect is absent today. These fixtures put
// it back — each in one of the shapes the original had (card_73f02e2a97ad):
// `read_to_end` over `pack-objects` stdout in either dialect, a second
// pack-sized `Vec` on the HTTP side, a clone endpoint that drains before it
// answers — and assert the check goes red on each.
//
// The floors matter as much as the mutations. An absence-based check reading
// the wrong tree, or one that stopped parsing its subject, reports success
// forever; so the baseline must pass, and a `read_to_end` planted inside a
// `#[cfg(test)]` item must *not* go red — that is the only evidence the check
// reads the production view rather than raw bytes.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'git-pack-streaming-contract-check.mjs');

const PACK_STREAM = 'crates/rg-git/src/protocol/pack_stream.rs';
const UPLOAD_PACK = 'crates/rg-git/src/protocol/upload_pack.rs';
const V2 = 'crates/rg-git/src/protocol/v2.rs';
const GIT_HTTP = 'crates/rg-http/src/git_http.rs';

/** The shape the repository actually has: streaming on every leg. */
const BASELINE = {
  [PACK_STREAM]: `use crate::sideband;

pub(crate) async fn stream_pack_objects<W>(
    mut child: Child,
    writer: &mut W,
    use_sideband: bool,
) -> Result<u64>
where
    W: AsyncWrite + Unpin,
{
    let streamed = copy_pack(stdout, writer, use_sideband).await?;
    if !status.success() {
        sideband::write_sideband_error(writer, &announced).await?;
        bail!("git pack-objects failed");
    }
    Ok(streamed)
}
`,
  [UPLOAD_PACK]: `async fn send_packfile<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    use_sideband: bool,
) -> Result<()> {
    let pack_size = pack_stream::stream_pack_objects(cmd, writer, use_sideband).await?;
    Ok(())
}
`,
  [V2]: `async fn stream_packfile<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    use_sideband: bool,
) -> Result<u64> {
    let pack_bytes = pack_stream::stream_pack_objects(cmd, writer, use_sideband).await?;
    Ok(pack_bytes)
}
`,
  [GIT_HTTP]: `fn spawn_git_response_reader<R>(reader: R) -> tokio::task::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await?;
        Ok(output)
    })
}

async fn stream_upload_pack_response(
    protocol: UploadPackProtocol,
    repo_path: std::path::PathBuf,
    staged: StagedGitBody,
) -> Response {
    crate::http_stream::reader_body_with_idle(head, buf_reader, completion, idle_timeout_secs)
}

pub(crate) async fn handle_git_upload_pack(
    State(state): State<AppState>,
) -> Response {
    stream_upload_pack_response(protocol, repo_path, staged).await
}
`,
};

function writeFixture(overrides = {}) {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-pack-streaming-'));
  for (const [surface, body] of Object.entries({ ...BASELINE, ...overrides })) {
    const target = join(fixture, surface);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, body);
  }
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    env: { ...process.env, PLOMBIR_GIT_PACK_STREAMING_ROOT: fixture },
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

// ── Floor: a test fixture must not be able to make it red ──────────────────
expect(
  'read_to_end inside a #[cfg(test)] item',
  {
    [PACK_STREAM]: `${BASELINE[PACK_STREAM]}
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_fixture_may_collect_a_stream() {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
    }
}
`,
  },
  { red: false },
);

// ── The original defect, one leg at a time ─────────────────────────────────
expect(
  'pack_stream collects the pack again',
  {
    [PACK_STREAM]: BASELINE[PACK_STREAM].replace(
      'let streamed = copy_pack(stdout, writer, use_sideband).await?;',
      'let mut pack = Vec::new();\n    stdout.read_to_end(&mut pack).await?;',
    ),
  },
  { red: true, mentions: [PACK_STREAM] },
);

expect(
  'pack_stream stops announcing a late failure',
  {
    [PACK_STREAM]: BASELINE[PACK_STREAM].replace(
      'sideband::write_sideband_error(writer, &announced).await?;',
      '',
    ),
  },
  { red: true, mentions: ['band-3'] },
);

expect(
  'v1 reads pack-objects stdout itself',
  {
    [UPLOAD_PACK]: `async fn send_packfile<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    let mut pack_data = Vec::new();
    pack_reader.read_to_end(&mut pack_data).await?;
    sideband::write_sideband_data(writer, &pack_data).await?;
    Ok(())
}
`,
  },
  { red: true, mentions: [UPLOAD_PACK] },
);

expect(
  'v2 stops routing through the single reader',
  {
    [V2]: BASELINE[V2].replace('pack_stream::stream_pack_objects', 'spawn_and_collect'),
  },
  { red: true, mentions: ['pack_stream::stream_pack_objects'] },
);

expect(
  'the transport buffers the response again',
  {
    [GIT_HTTP]: BASELINE[GIT_HTTP].replace(
      'crate::http_stream::reader_body_with_idle(head, buf_reader, completion, idle_timeout_secs)',
      'crate::http_stream::buffered_body_with_idle(output, idle_timeout_secs)',
    ),
  },
  { red: true, mentions: ['buffered_body_with_idle'] },
);

expect(
  'the clone endpoint drains before answering',
  {
    [GIT_HTTP]: BASELINE[GIT_HTTP].replace(
      '    stream_upload_pack_response(protocol, repo_path, staged).await',
      '    let reader_task = spawn_git_response_reader(buf_reader);\n'
        + '    stream_upload_pack_response(protocol, repo_path, staged).await',
    ),
  },
  { red: true, mentions: ['spawn_git_response_reader'] },
);

expect(
  'a second drain appears outside the receive-pack reader',
  {
    [GIT_HTTP]: `${BASELINE[GIT_HTTP]}
async fn collect_upload_pack(mut reader: DuplexStream) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await?;
    Ok(output)
}
`,
  },
  { red: true, mentions: ['read_to_end'] },
);

// ── Floor: an unreadable subject must be red, not silently green ───────────
expect(
  'the streaming builder is renamed away',
  {
    [GIT_HTTP]: BASELINE[GIT_HTTP].replaceAll('stream_upload_pack_response', 'answer_upload_pack'),
  },
  { red: true, mentions: ['stream_upload_pack_response'] },
);

if (failures.length > 0) {
  console.error('❌ git pack streaming mutation stand failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ git pack streaming mutation stand: the contract check still bites');
