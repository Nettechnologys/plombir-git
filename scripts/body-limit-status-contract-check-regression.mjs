#!/usr/bin/env node

// Mutation stand for body-limit-status-contract-check.mjs.
//
// That check answers "does every streaming body read in rg-http reach a
// length-limit classifier, and does every classifier answer 413". A green
// answer is worth exactly the red ones it is still capable of, and this gate
// has three separate ways to go quiet, so each gets its own fixture:
//
//   * the VERDICT half can be dropped — the classifier is still called, the
//     branch it guards answers `400`. This is the mutation that proved
//     card_134c43fa6089 (`stream_to_file` returning `bad_request` on a tripped
//     ceiling) and the one the card asks this stand to reproduce;
//   * the COVERAGE half can be dropped, in three grains that a coarser reader
//     would miss in turn: a function that classified stops (artifacts), a
//     function that hands its error upward keeps handing it to a caller that
//     no longer classifies (LFS), and a caller that classifies three body
//     errors stops classifying a fourth (OCI) or a multipart read stops
//     classifying while the same function still classifies its other reads
//     (Twine). The last two are why the check reads call sites and not just
//     functions;
//   * the READER can stall — a spelling falling out of `READ_SITES`, a
//     classifier surviving only as a comment, or a streaming read moving to a
//     crate this check does not look at. A check that recognises nothing
//     reports a clean tree, which is indistinguishable from a green run by exit
//     code alone.
//
// Fixtures are a throwaway copy of the tree; nothing here writes to the
// worktree.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const checkName = 'body-limit-status-contract-check.mjs';

/** A throwaway copy of the tree the check reads: rg-http, the check, its libs. */
function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-body-status-'));
  mkdirSync(join(fixture, 'crates', 'rg-http'), { recursive: true });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, 'crates', 'rg-http', 'src'), join(fixture, 'crates', 'rg-http', 'src'), {
    recursive: true,
  });
  cpSync(join(root, 'scripts', checkName), join(fixture, 'scripts', checkName));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  return fixture;
}

function edit(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

// Where the check is expected to point, resolved against the fixture instead of
// written down as a number. A pinned `file:NNN` makes every insertion ABOVE the
// site red — the shape that cost a session when `write_body_to_file` moved from
// 864 to 893 and this stand blamed a gate that had answered perfectly. The pair
// (function, call spelling) is what the expectation is really about, and it
// still holds the check to one exact line: point at the wrong function, or one
// line off, and the mention misses.
function site(path, fn, call) {
  return { site: { path, fn, call } };
}

/** Resolves one `site(…)` mention to the `file:line` this run should print. */
function siteIn(fixture, { path: relative, fn, call }) {
  const lines = readFileSync(join(fixture, relative), 'utf8').split('\n');
  const declaration = new RegExp(`\\bfn ${fn}\\s*[(<]`);
  const declared = [];
  lines.forEach((line, index) => {
    if (declaration.test(line)) declared.push(index);
  });
  if (declared.length !== 1) {
    throw new Error(
      `${relative}: expected exactly one \`fn ${fn}\`, found ${declared.length} — the anchor this `
        + 'expectation resolves its line through is gone or ambiguous',
    );
  }
  const start = declared[0];
  // rustfmt closes a top-level item with a `}` in column 0, so the search stays
  // inside the function: a call spelling that left it must not resolve to the
  // same spelling in the next one.
  const closed = lines.findIndex((line, index) => index > start && line === '}');
  const body = lines.slice(start, closed < 0 ? lines.length : closed + 1);
  const hit = body.findIndex((line) => line.includes(call));
  if (hit < 0) {
    throw new Error(`${relative}: \`${fn}\` no longer contains ${JSON.stringify(call)}`);
  }
  return `${relative}:${start + hit + 1}`;
}

const failures = [];

function expect(name, mutate, { red, mentions = [] }) {
  const fixture = fixtureRoot();
  const http = join(fixture, 'crates', 'rg-http', 'src');
  try {
    mutate({
      fixture,
      artifacts: join(http, 'api', 'artifacts.rs'),
      attachments: join(http, 'api', 'attachments.rs'),
      bodyLimit: join(http, 'body_limit.rs'),
      lfs: join(http, 'api', 'lfs.rs'),
      oci: join(http, 'oci.rs'),
      packages: join(http, 'api', 'packages.rs'),
      check: join(fixture, 'scripts', checkName),
    });
    const result = spawnSync(process.execPath, [join(fixture, 'scripts', checkName)], {
      cwd: fixture,
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (red && result.status === 0) {
      failures.push(`${name}: the check stayed green on a mutated fixture — it asserts nothing`);
      return;
    }
    if (!red && result.status !== 0) {
      failures.push(`${name}: the check went red on a fixture it should accept:\n${output}`);
      return;
    }
    for (const fragment of mentions) {
      const expected = typeof fragment === 'string' ? fragment : siteIn(fixture, fragment.site);
      if (!output.includes(expected)) {
        failures.push(`${name}: the verdict never mentions ${JSON.stringify(expected)}:\n${output}`);
        return;
      }
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The copied tree is the shipped tree. If this is not green, every red below is
// about the copy rather than about the mutation it claims to prove.
expect('an unmutated copy of the tree passes', () => {}, { red: false });

// ── The verdict half ───────────────────────────────────────────────────────

// The exact mutation card_134c43fa6089 was proved with. The classifier call
// stays; only the answer changes, which is what makes this the one a coverage-
// only gate cannot see.
expect(
  'a recognised ceiling answered as 400 is caught',
  ({ artifacts }) =>
    edit(
      artifacts,
      `                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(over_ceiling());`,
      `                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(AppError::bad_request("artifact archive is too large"));`,
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/api/artifacts.rs', 'stream_to_file', 'is_length_limit_error('),
      'recognises a tripped body ceiling and then answers with something other than 413',
    ],
  },
);

// The same defect one level of indirection away: the OCI registry builds its
// own error envelope, so its 413 is a `StatusCode`, not an `AppError`.
expect(
  'a classifier helper answering 500 is caught',
  ({ oci }) =>
    edit(
      oci,
      `    if crate::body_limit::is_length_limit_error(error.as_ref()) {
        oci_err(
            StatusCode::PAYLOAD_TOO_LARGE,`,
      `    if crate::body_limit::is_length_limit_error(error.as_ref()) {
        oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,`,
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/oci.rs', 'oci_body_error', 'is_length_limit_error('),
      '`oci_body_error` recognises a tripped body',
    ],
  },
);

// ── The coverage half, in the three grains it can be lost in ───────────────

expect(
  'a streaming path that stops classifying at all is caught',
  ({ artifacts }) =>
    edit(
      artifacts,
      `                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(over_ceiling());
                }
`,
      '',
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/api/artifacts.rs', 'stream_to_file', '.into_data_stream('),
      '`stream_to_file` streams the request body',
    ],
  },
);

// `write_body_to_file` deliberately does not classify: it preserves the typed
// error so its caller can classify once. Take the caller's classification away
// and the streaming read is uncovered, even though nothing in that function
// changed.
expect(
  'a streaming helper whose caller stops classifying is caught',
  ({ lfs }) =>
    edit(
      lfs,
      '        Err(e) => lfs_body_error(e).into_response(),',
      '        Err(e) => AppError::bad_request(format!("{e}")).into_response(),',
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/api/lfs.rs', 'write_body_to_file', '.into_data_stream('),
      '`write_body_to_file` streams the request body',
    ],
  },
);

// The grain a function-level reader cannot see: `complete_upload` classifies
// three body errors and would go on looking like "a function that classifies"
// with the fourth arm answering 400.
expect(
  'one arm of a multi-arm caller losing the classifier is caught',
  ({ oci }) =>
    edit(
      oci,
      `                None => match upload_body_is_empty(body).await {
                    Ok(is_empty) => is_empty,
                    Err(error) => return oci_body_error(error),
                },`,
      `                None => match upload_body_is_empty(body).await {
                    Ok(is_empty) => is_empty,
                    Err(error) => {
                        return oci_err(StatusCode::BAD_REQUEST, "UNKNOWN", &format!("{error:#}"))
                    }
                },`,
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/oci.rs', 'upload_body_is_empty', '.into_data_stream('),
      '`upload_body_is_empty` streams the request body',
    ],
  },
);

// The same grain on the multipart side: `decode_twine_upload` classifies four
// multipart errors, and losing the one on `next_field` leaves the function
// still classifying — the read site is what has to be asked.
expect(
  'one multipart read losing the classifier is caught',
  ({ packages }) =>
    edit(
      packages,
      '        .map_err(|error| package_multipart_error(error, "invalid Twine multipart body"))?',
      '        .map_err(|error| AppError::bad_request(format!("invalid Twine body: {error}")))?',
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/api/packages.rs', 'decode_twine_upload', '.next_field('),
      "this `Multipart::next_field` call's error is not handed to",
    ],
  },
);

// ── The reader, and the ways it can go quiet ───────────────────────────────

// `RequestBodyLimitLayer` nests `LengthLimitError` inside an `axum::Error`, so
// a classifier that stops walking `source()` recognises nothing while every
// caller keeps its 413 branch — nine paths that answer 400 with no line of any
// handler changed.
expect(
  'a classifier that stops walking the source chain is caught',
  ({ bodyLimit }) => {
    const source = readFileSync(bodyLimit, 'utf8');
    const walk = source.indexOf('    let mut current = error.source();');
    const tail = source.indexOf('    false');
    if (walk < 0 || tail < 0) throw new Error('body_limit.rs: fixture anchors disappeared');
    writeFileSync(bodyLimit, source.slice(0, walk) + source.slice(tail));
  },
  {
    red: true,
    mentions: ['no longer mentions `source()`'],
  },
);

// Raw bytes are not the program: a classifier that survives only as a comment
// must read as deleted (card_d67b6f433341).
expect(
  'a classifier that exists only in a comment reads as absent',
  ({ packages }) =>
    edit(
      packages,
      `                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(AppError::payload_too_large(
                        "package upload exceeds the configured request-body limit",
                    ));
                }`,
      `                // if crate::body_limit::is_length_limit_error(&*inner) {
                //     return Err(AppError::payload_too_large(
                //         "package upload exceeds the configured request-body limit",
                //     ));
                // }`,
    ),
  {
    red: true,
    mentions: [
      site('crates/rg-http/src/api/packages.rs', 'stage_package_upload', '.into_data_stream('),
      '`stage_package_upload` streams the request body',
    ],
  },
);

// A `READ_SITES` entry going stale is the quiet failure this floor exists for:
// unrecognised reads are not reported as uncovered, they are not reported at
// all.
expect(
  'a read spelling falling out of the list trips the floor',
  ({ check }) =>
    edit(
      check,
      "  { name: 'Body::into_data_stream', re: /\\.into_data_stream\\s*\\(/g, immediate: false },\n",
      '',
    ),
  {
    red: true,
    mentions: ['streaming body read(s) were recognised in rg-http (expected at least'],
  },
);

// The subject boundary is a fact about today's tree, so it is asserted rather
// than assumed: a streaming read in a second crate must be a red, not a silence.
expect(
  'a streaming read outside rg-http is caught',
  ({ fixture }) => {
    const src = join(fixture, 'crates', 'rg-core', 'src');
    mkdirSync(src, { recursive: true });
    writeFileSync(
      join(src, 'drain.rs'),
      'async fn drain(body: axum::body::Body) {\n'
        + '    let mut stream = body.into_data_stream();\n'
        + '    let _ = stream;\n'
        + '}\n',
    );
  },
  {
    red: true,
    mentions: ['crates/rg-core/src/drain.rs:2', 'is called outside `rg-http`'],
  },
);

// What decides the subject is the dependency on Axum, not the spelling of the
// call: a crate that declares Axum is read even when its manifest says so as a
// workspace dependency, and its read is named by kind, not as `undefined`.
expect(
  'a streaming read in a crate that depends on axum is caught',
  ({ fixture }) => {
    const crate = join(fixture, 'crates', 'rg-core');
    mkdirSync(join(crate, 'src'), { recursive: true });
    writeFileSync(join(crate, 'Cargo.toml'), '[dependencies]\naxum.workspace = true\n');
    writeFileSync(
      join(crate, 'src', 'upload.rs'),
      'async fn upload(mut field: axum::extract::multipart::Field<\'_>) {\n'
        + '    let _ = field.chunk().await;\n'
        + '}\n',
    );
  },
  {
    red: true,
    mentions: ['crates/rg-core/src/upload.rs:2', '`Field::chunk` is called outside `rg-http`'],
  },
);

// And the other half: an outbound response read in a crate with no Axum in its
// manifest is no request body, and must not hold a push.
expect(
  'an outbound response read in a crate without axum is not a request body',
  ({ fixture }) => {
    const crate = join(fixture, 'crates', 'rg-core');
    mkdirSync(join(crate, 'src'), { recursive: true });
    writeFileSync(join(crate, 'Cargo.toml'), '[dependencies]\nreqwest = { workspace = true }\n');
    writeFileSync(
      join(crate, 'src', 'fetch.rs'),
      'async fn fetch(mut response: reqwest::Response) {\n'
        + '    while let Ok(Some(_)) = response.chunk().await {}\n'
        + '}\n',
    );
  },
  { red: false },
);

if (failures.length > 0) {
  console.error('❌ body limit status mutation stand failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ body limit status contract still bites on every shape of the defect');
