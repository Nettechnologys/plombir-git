#!/usr/bin/env node

// Mutation stand for rust-source-view-contract-check.mjs.
//
// A green workspace proves only that no Rust guard reads raw bytes today. It
// says nothing about whether the ratchet would notice the next one — and a
// reader that has stopped recognising reads reports the same clean corpus.
// That failure mode is the one this whole family exists to refuse, so it is the
// one the stand has to hold shut first.
//
// Each case writes a small crate tree into a fixture, points the real check at
// it, and judges it by exit code AND by what the diagnostic names. The second
// half matters as much as the first: a ratchet that goes red at the wrong line
// sends the reader to a file that is fine, and the two are indistinguishable
// from a non-zero exit.
//
// Every fixture carries a stub `tests/support/rust_source.rs` declaring the
// named views, because the check derives its normalizer set from that file
// rather than from a list — so a fixture is also a statement about how the
// discovery behaves when the shared module is small.

import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'rust-source-view-contract-check.mjs');

let failed = 0;

/**
 * The shared reader every fixture carries.
 *
 * Two production views and one test-inclusive view, spelled the way
 * `tests/support/rust_source.rs` spells them — the check seeds on these names
 * and closes over whatever calls them, so a fixture helper becomes a normalizer
 * by calling one rather than by being listed anywhere.
 */
const SUPPORT = `pub(crate) fn rust_code_only(text: &str) -> String {
    text.to_owned()
}

pub(crate) fn production_rust_code_only(text: &str) -> String {
    rust_code_only(text)
}

pub(crate) fn production_rust_source(text: &str) -> String {
    text.to_owned()
}

pub(crate) fn production_rust_code_with_doc_comments(text: &str) -> String {
    text.to_owned()
}

pub(crate) struct RustFunction {
    pub(crate) name: String,
    pub(crate) body: String,
}

pub(crate) fn functions(text: &str) -> Vec<RustFunction> {
    let _ = rust_code_only(text);
    Vec::new()
}

pub(crate) fn rust_files(dir: &str, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.display().to_string());
        }
    }
}
`;

/** Run the real check over a fixture whose `crates/demo/src/guard.rs` is `body`. */
function runCase(name, { body, min = 1, support = SUPPORT, expect }) {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-rust-view-'));
  try {
    mkdirSync(join(fixture, 'crates/demo/src'), { recursive: true });
    mkdirSync(join(fixture, 'tests/support'), { recursive: true });
    writeFileSync(join(fixture, 'tests/support/rust_source.rs'), support);
    writeFileSync(join(fixture, 'crates/demo/src/guard.rs'), body);

    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: {
        ...process.env,
        FORGEKEEP_RUST_VIEW_ROOT: fixture,
        FORGEKEEP_RUST_VIEW_MIN: String(min),
      },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const red = result.status !== 0;

    if (red !== expect.red) {
      console.error(
        `❌ ${name}: expected the ratchet to go ${expect.red ? 'red' : 'green'}, it went ${red ? 'red' : 'green'}:\n${output}`,
      );
      failed += 1;
      return;
    }
    for (const needle of expect.mentions ?? []) {
      if (!output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic never named ${needle}:\n${output}`);
        failed += 1;
        return;
      }
    }
    for (const needle of expect.silent ?? []) {
      if (output.includes(needle)) {
        console.error(`❌ ${name}: the diagnostic named ${needle}, which is not the defect:\n${output}`);
        failed += 1;
        return;
      }
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The defect itself, in the shape nine commits have already removed by hand: a
// guard binds the bytes of a `.rs` file and greps them.
const RAW_BINDING = `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        assert!(source.contains("record_audit("));
    }
}
`;

runCase('a raw `include_str!` bound and grepped is reported', {
  body: RAW_BINDING,
  expect: { red: true, mentions: ['`source`', '.contains('] },
});

// The same guard through a named view. Nothing about the assertion changed —
// only where the bytes came from — so the silence here is what says the ratchet
// is about the view and not about `contains`.
runCase('the same guard through a production view is silent', {
  body: RAW_BINDING.replace(
    'let source = include_str!("../../other/src/writer.rs");',
    'let source = rust_source::production_rust_code_only(include_str!("../../other/src/writer.rs"));',
  ),
  expect: { red: false },
});

// Unbound. `include_str!(…).contains(…)` names nothing, so a reader keyed on
// `let` walks past it — and this is the exact spelling one recurring gotcha in
// this tree carries five times over.
runCase('an `include_str!` grepped without ever being bound is reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        assert!(include_str!("../../other/src/writer.rs").contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['.contains('] },
});

// A guard's own `contract(source)` is where all of its assertions go, so the
// laundering question is asked about it and not only about the library. This
// one launders: its body reaches a named view, which is how it is recognised —
// nothing lists it.
runCase('a local helper that reaches a named view launders the bytes', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn contract(source: &str) -> Result<(), String> {
        let code = rust_source::production_rust_code_only(source);
        if code.contains("record_audit(") {
            return Ok(());
        }
        Err("the writer left".to_owned())
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        contract(source).unwrap();
    }
}
`,
  expect: { red: false },
});

// The other side of that case, so the silence above is a decision rather than
// an accident: the identical helper that greps instead is reported, and the
// diagnostic names the helper rather than the assertion inside it — that is
// where the reader has to look.
runCase('a local helper that greps instead is reported by name', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn contract(source: &str) -> Result<(), String> {
        if source.contains("record_audit(") {
            return Ok(());
        }
        Err("the writer left".to_owned())
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        contract(source).unwrap();
    }
}
`,
  expect: { red: true, mentions: ['handed to `contract`'] },
});

// The test-inclusive half. A sweep that only REPORTS what it finds is not
// fooled by a fixture — the worst one can do is ask for a look — and it says so
// by naming `rust_code_only`, not by being parked on an exclusion list.
runCase('a sweep through the test-inclusive named view is silent', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn every_listener_reports_its_address() {
        let source = rust_source::rust_code_only(include_str!("../../other/src/listener.rs"));
        for line in source.lines() {
            assert!(!line.contains("bind(0)"));
        }
    }
}
`,
  expect: { red: false },
});

// The two-view idiom, which is what a guard writes when its verdict comes off
// the code view but its DIAGNOSTIC has to quote the file as written. Reporting
// the second half would demand that guards stop quoting themselves.
runCase('quoting the original line beside the code view is not a raw assertion', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn every_gate_call_is_named_with_its_line() {
        let text = include_str!("../../other/src/writer.rs");
        let hits: Vec<(usize, &str)> = rust_source::production_rust_code_only(text)
            .lines()
            .zip(text.lines())
            .enumerate()
            .filter(|(_, (code, _))| code.contains("record_audit("))
            .map(|(n, (_, original))| (n + 1, original))
            .collect();
        assert!(!hits.is_empty());
    }
}
`,
  expect: { red: false },
});

// A read of something that is not Rust. `/proc/<pid>/stat` is read with the
// same call in a file that also names `.rs` elsewhere; keying on the file
// instead of on the read is how this reader first accused four of them.
runCase('a `read_to_string` of something that is not Rust is not a read of Rust', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn proc_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().next()?.chars().next()
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = rust_source::production_rust_code_only(include_str!("../../other/src/writer.rs"));
        assert!(source.contains("record_audit("));
        assert!(proc_state(1).is_none() || true);
    }
}
`,
  expect: { red: false, silent: ['stat'] },
});

// A walk. The path never appears as a literal on the way to the read — the
// function names `.rs` only inside the walker it calls — so following literals,
// which is all a lexical reader can do, walks straight past it.
runCase('bytes fed by a `.rs` walk and grepped are reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn no_module_keeps_its_own_writer() {
        let mut files = Vec::new();
        rust_source::rust_files("crates", &mut files);
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            assert!(!text.contains("fn record_audit"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`text`'] },
});

// Producer and consumer in different functions, which is how nearly every guard
// in this tree is written: one helper collects the bytes, another greps them.
// A reader with no cross-function step is decorative on exactly those — five
// real gates were mutated back to raw text and only one reddened before this
// existed.
runCase('a helper that reads and a test that greps are one defect', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn writer_source() -> String {
        include_str!("../../other/src/writer.rs").to_owned()
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = writer_source();
        assert!(source.contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['`source`', '.contains('] },
});

// The other half of the cross-function step, and the one that decides whether
// it is usable: a producer handing back `(path, text)` pairs taints the text
// and NOT the path. Tainting the whole pattern accused `path.starts_with(…)` —
// a string operation on a directory name — of being a raw source assertion,
// which is a ratchet nobody can keep green.
runCase('a producer of (path, text) pairs does not taint the path', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn workspace_sources() -> Vec<(String, String)> {
        let mut files = Vec::new();
        rust_source::rust_files("crates", &mut files);
        files
            .into_iter()
            .map(|file| {
                let text = std::fs::read_to_string(&file).unwrap();
                (file, text)
            })
            .collect()
    }

    #[test]
    fn only_one_module_writes_the_row() {
        for (path, source) in workspace_sources() {
            if path.starts_with("rg-core/src/audit/") {
                continue;
            }
            assert!(!rust_source::production_rust_code_only(&source).contains("ActiveModel {"));
        }
    }
}
`,
  expect: { red: false, silent: ['`path`'] },
});

// The anti-vacuous half, and the failure mode this family is really about: a
// reader that has stopped recognising reads reports the same clean corpus as
// one that has nothing to report.
runCase('a corpus below the recognised-read floor is rejected', {
  body: RAW_BINDING.replace(
    'let source = include_str!("../../other/src/writer.rs");',
    'let source = rust_source::production_rust_code_only(include_str!("../../other/src/writer.rs"));',
  ),
  min: 3,
  expect: { red: true, mentions: ['expected at least 3'] },
});

// And the seed itself. Without the shared reader there is no normalizer set, so
// every read would look laundered and the ratchet would pass over anything —
// which has to be red rather than green.
runCase('a tree with no shared reader module refuses to answer', {
  body: RAW_BINDING,
  support: 'pub(crate) fn unrelated() {}\n',
  expect: { red: true, mentions: ['`source`'] },
});

// One derivation. `functions(&text)` launders `text` — it reaches a named view,
// and the byte-aligned two-view idiom depends on that staying laundered — but
// what it hands back is the ORIGINAL bytes of each function body, and a grep of
// those is the same defect one hop along. Two real gates were mutated back to
// raw text and stayed green here (`foreign_gate_guard`, `gitea_actions`).
runCase('bytes a view handed back and then grepped are reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn no_handler_reads_the_database_itself() {
        let text = include_str!("../../other/src/handlers.rs");
        for function in rust_source::functions(&text) {
            assert!(!function.body.contains("rg_db::"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`function`', '.contains('] },
});

// The same hop through a helper rather than a method, which is how the gate
// this case is drawn from actually spells it: the mutation lands inside the
// helper, the call site never changes, and the helper simply stops being a
// normalizer.
runCase('bytes a view handed back and then handed to a grepping helper are reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn mentions_database(body: &str) -> bool {
        body.contains("rg_db::")
    }

    #[test]
    fn no_handler_reads_the_database_itself() {
        let text = include_str!("../../other/src/handlers.rs");
        for function in rust_source::functions(&text) {
            assert!(!mentions_database(&function.body));
        }
    }
}
`,
  expect: { red: true, mentions: ['handed to `mentions_database`'] },
});

// And the green half, so the two above are a decision rather than an accident:
// the identical shape whose helper views the body it was handed is silent. Only
// the helper changed.
runCase('bytes a view handed back and then re-viewed are silent', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn mentions_database(body: &str) -> bool {
        rust_source::production_rust_code_only(body).contains("rg_db::")
    }

    #[test]
    fn no_handler_reads_the_database_itself() {
        let text = include_str!("../../other/src/handlers.rs");
        for function in rust_source::functions(&text) {
            assert!(!mentions_database(&function.body));
        }
    }
}
`,
  expect: { red: false },
});

// A view alias: a `fn` whose body is a named view applied and returned. What it
// hands back IS the view, so the binding taken off it is clean and following it
// would report the idiom itself. This is the case that decides whether the hop
// above is usable at all.
runCase('a wrapper that returns a named view is not followed into', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn code_view(text: &str) -> String {
        rust_source::production_rust_code_only(text)
    }

    #[test]
    fn the_writer_is_still_wired() {
        let text = include_str!("../../other/src/writer.rs");
        let code = code_view(text);
        assert!(code.contains("record_audit("));
    }
}
`,
  expect: { red: false, silent: ['`code`'] },
});

// The other end of the same restraint: a binding taken off a TRANSFORM of what
// the view returned is something else — a set of names, an offset — and this
// reader abstains rather than accusing it. Both shapes are live in this tree
// (`local_fns` in `global_id_anchor_guard`, `tracker_call` in `git_http`), and
// both were reported the first time the hop was taken.
runCase('a transform of what a view returned is not the bytes', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn all_are_handlers(names: &[String]) -> bool {
        names.iter().all(|name| name.starts_with("handle_"))
    }

    #[test]
    fn every_function_is_a_handler() {
        let text = include_str!("../../other/src/handlers.rs");
        let names: Vec<String> = rust_source::functions(&text)
            .into_iter()
            .map(|function| function.name)
            .collect();
        assert!(all_are_handlers(&names));
    }
}
`,
  expect: { red: false, silent: ['`names`'] },
});

// A census that spells its own walk instead of calling a helper. The walker set
// is keyed on NAMES and matched by call, so a function that IS the walker calls
// none — and the read at the bottom of its loop was not merely unreported, it
// was never recognised, so it did not even hold up the floor.
runCase('bytes read by a walk the function spells itself are reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn no_module_keeps_its_own_writer() {
        let mut files = vec![std::path::PathBuf::from("crates")];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(
                    std::fs::read_dir(&path)
                        .expect("read source directory")
                        .map(|entry| entry.expect("read entry").path()),
                );
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            assert!(!text.contains("fn record_audit"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`text`'] },
});

// And the walk that is about something else. Recognising an inline walk is what
// makes this distinction load-bearing: the extension is the only thing that
// says the bytes are Rust, and a manifest sweep must not be dragged in by the
// `.rs` a neighbouring function names.
runCase('a walk that names no Rust extension is not a read of Rust', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn every_crate_declares_its_edition() {
        for entry in std::fs::read_dir("crates").expect("read crates") {
            let manifest = entry.expect("read entry").path().join("Cargo.toml");
            let text = std::fs::read_to_string(&manifest).expect("read manifest");
            assert!(text.contains("edition"));
        }
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = rust_source::production_rust_code_only(include_str!("../../other/src/writer.rs"));
        assert!(source.contains("record_audit("));
    }
}
`,
  expect: { red: false, silent: ['Cargo.toml', '`text`'] },
});

// A read spelled inside a module-level `const` TUPLE. The path and the bytes
// travel together so the diagnostic can name the file, and the binding regex
// asks the read to follow the `=` directly — so the opening parenthesis walked
// the reader past the read entirely. It was not merely unreported: it was never
// counted, so the floor did not hold it either (card_7c2d24ce98b3).
const TUPLE_ALIAS = `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    const ALIAS: (&str, &str) = (
        "crates/other/src/cli.rs",
        include_str!("../../other/src/cli.rs"),
    );

    fn help_block() -> String {
        let (name, source) = ALIAS;
        BODY
    }

    #[test]
    fn the_alias_still_documents_the_runner() {
        assert!(help_block().contains("--server"));
    }
}
`;

runCase('a read spelled inside a tuple `const` and grepped is reported', {
  body: TUPLE_ALIAS.replace(
    '        BODY',
    `        let (_, rest) = source.split_once("Runner {").unwrap_or_else(|| panic!("{name}"));
        rest.to_owned()`,
  ),
  expect: { red: true, mentions: ['`source`', 'slot 1 of `ALIAS`', '.split_once('] },
});

// The same tuple through the view it is meant to be read through. Nothing about
// the shape changed — only where the bytes were normalized — so the silence
// here is what says the case above is about the view and not about tuples.
runCase('the same tuple `const` read through a named view is silent', {
  body: TUPLE_ALIAS.replace(
    '        BODY',
    `        let production = rust_source::production_rust_code_with_doc_comments(source);
        let (_, rest) = production.split_once("Runner {").unwrap_or_else(|| panic!("{name}"));
        rest.to_owned()`,
  ),
  expect: { red: false },
});

// And the half that decides whether the tuple hop is usable at all: slot 0 is a
// PATH, and a guard quotes it in its own failure message. Tainting the whole
// declaration accuses `name.starts_with(…)` and `ALIAS.0.ends_with(…)` of being
// raw source assertions — the same false positive `tupleSlot` was written to
// prevent one construct over.
runCase('the path slot of a tuple `const` is not the bytes', {
  body: TUPLE_ALIAS.replace(
    '        BODY',
    `        assert!(name.starts_with("crates/"));
        assert!(ALIAS.0.ends_with(".rs"));
        rust_source::production_rust_code_with_doc_comments(source)`,
  ),
  expect: { red: false, silent: ['`name`', '`ALIAS`'] },
});

// A rename is not a derivation. `let owned = source.to_owned();` asks nothing
// about the bytes — it hands them on — so what it binds is the read under a
// second name, and a reader that stopped at the rename left the grep one line
// later unanswered. This is the vocabulary `returnsValueOf` already treats as
// value-preserving, asked of a binding instead of of a tail.
runCase('bytes handed on under a second name are still the bytes', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let owned = source.to_owned();
        assert!(owned.contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['`owned`', 'same `.rs` bytes as `source`', '.contains('] },
});

// The restraint the tuple hop needs to be usable: the `const` owns the whole
// module, but the name a consumer unpacks it into does not. `source` is a name
// half a guard file uses, and resolving it module-wide is how this reader once
// answered about one guard's `text` using another's four hundred lines away
// (sol_706e02368e50) — so the pattern is resolved inside the function that
// spells it, and the properly-viewed read in the next test is left alone.
runCase('a tuple `const` unpacked into a common name answers only for its own function', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    const ALIAS: (&str, &str) = (
        "crates/other/src/cli.rs",
        include_str!("../../other/src/cli.rs"),
    );

    fn help_block() -> String {
        let (name, source) = ALIAS;
        let (_, rest) = source.split_once("Runner {").unwrap_or_else(|| panic!("{name}"));
        rest.to_owned()
    }

    #[test]
    fn the_readme_module_still_documents_the_alias() {
        let source = rust_source::production_rust_code_only(include_str!("../../other/src/readme.rs"));
        assert!(source.contains("forgekeep-runner"));
    }

    #[test]
    fn the_alias_still_documents_the_runner() {
        assert!(help_block().contains("--server"));
    }
}
`,
  expect: { red: true, mentions: ['.split_once('], silent: ['.contains('] },
});

if (failed > 0) {
  console.error(`❌ rust-source-view mutation stand: ${failed} case(s) failed`);
  process.exit(1);
}
console.log(
  '✅ rust-source-view mutation stand: a raw read is reported bound and unbound, a local helper '
    + 'launders only by reaching a named view, what a view hands BACK is followed one hop while a '
    + 'view alias and a transform of one are not, a read hidden in a tuple `const` is seen and only '
    + 'its byte slot is accused and only inside the function that unpacks it, a rename is still '
    + 'the bytes, a walk a '
    + 'function spells itself is still a walk, the test-inclusive view is an intent rather than an '
    + 'exclusion, and a reader that stops seeing the corpus is refused',
);
