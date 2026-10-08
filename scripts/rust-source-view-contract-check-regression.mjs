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
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
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

/**
 * Run the real check over a fixture whose `crates/demo/src/guard.rs` is `body`.
 *
 * `files` writes anything else the case needs, keyed by a path relative to the
 * fixture root. The shared-reader question needs it: whether a wrapper counts
 * as the view depends on the file it is DECLARED in, so a case about that has
 * to be able to put one somewhere other than the guard.
 */
function runCase(name, { body, min = 1, support = SUPPORT, files = {}, expect }) {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-rust-view-'));
  try {
    mkdirSync(join(fixture, 'crates/demo/src'), { recursive: true });
    mkdirSync(join(fixture, 'tests/support'), { recursive: true });
    writeFileSync(join(fixture, 'tests/support/rust_source.rs'), support);
    writeFileSync(join(fixture, 'crates/demo/src/guard.rs'), body);
    for (const [path, content] of Object.entries(files)) {
      mkdirSync(dirname(join(fixture, path)), { recursive: true });
      writeFileSync(join(fixture, path), content);
    }

    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: {
        ...process.env,
        PLOMBIR_GIT_RUST_VIEW_ROOT: fixture,
        PLOMBIR_GIT_RUST_VIEW_MIN: String(min),
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
        assert!(source.contains("plombir-git-runner"));
    }

    #[test]
    fn the_alias_still_documents_the_runner() {
        assert!(help_block().contains("--server"));
    }
}
`,
  expect: { red: true, mentions: ['.split_once('], silent: ['.contains('] },
});

// The second axis of the seed split. `production_rust_source` blanks test items
// and KEEPS comments and literals on purpose, so a `fn` that is it applied and
// returned hands back text a comment can still fool. Reading that as a view
// alias made the binding not a read at all, and seven crates could have dropped
// the code view their consumers apply with nothing objecting.
const STRING_BEARING = `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    /// The production view of the module under audit: test items blanked,
    /// comments and literals kept so a real attribute can be decoded.
    fn production_source() -> String {
        rust_source::production_rust_source(include_str!("../../other/src/writer.rs"))
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = production_source();
        BODY
    }
}
`;

runCase('bytes from the string-bearing view are not finished bytes', {
  body: STRING_BEARING.replace('        BODY', '        assert!(source.contains("record_audit("));'),
  expect: { red: true, mentions: ['`source`', '.contains('] },
});

// The same guard once a code view is applied on top. Only where the bytes were
// normalized changed, so the silence here is what says the case above is about
// the second axis and not about `production_source` being a helper.
runCase('the same bytes through a code view on top are silent', {
  body: STRING_BEARING.replace(
    '        BODY',
    '        assert!(rust_source::production_rust_code_only(&source).contains("record_audit("));',
  ),
  expect: { red: false },
});

// The helper form, which is how every crate holding this view actually spells
// it: the call site never changes, the mutation lands inside the helper, and
// the helper simply stops applying the code view.
runCase('string-bearing bytes handed to a helper that greps them are reported', {
  body: STRING_BEARING.replace(
    '    #[test]',
    `    fn mentions_writer(source: &str) -> bool {
        source.contains("record_audit(")
    }

    #[test]`,
  ).replace('        BODY', '        assert!(mentions_writer(&source));'),
  expect: { red: true, mentions: ['handed to `mentions_writer`'] },
});

// The rule that makes the case above reachable at all. A raw read is laundered
// wholesale by the first view it reaches — the two-view zip idiom depends on
// that — but a string-bearing read has ALREADY reached one, so the wholesale
// rule would answer the question with its own premise: every guard holding this
// view hands the bytes to several helpers, and one of them viewing them said
// nothing about the rest. This is the live shape of `issue_template.rs`, where
// `template_model_types(&source)` sits two lines above `serde_fields(&source, …)`.
runCase('a code view on one mention does not launder the other mentions', {
  body: STRING_BEARING.replace(
    '    #[test]',
    `    fn model_types(source: &str) -> Vec<String> {
        let code = rust_source::production_rust_code_only(source);
        code.lines()
            .filter_map(|line| line.trim().strip_prefix("struct "))
            .map(|rest| rest.trim_end_matches(" {").to_owned())
            .collect()
    }

    fn mentions_writer(source: &str) -> bool {
        source.contains("record_audit(")
    }

    #[test]`,
  ).replace(
    '        BODY',
    `        assert!(!model_types(&source).is_empty());
        assert!(mentions_writer(&source));`,
  ),
  expect: { red: true, mentions: ['handed to `mentions_writer`'], silent: ['`model_types`'] },
});

// And the restraint that keeps the case above usable: the byte-aligned two-view
// idiom, which is the whole reason `production_rust_source` exists. The helper
// bounds a construct in the CODE view and hands back the ORIGINAL slice,
// because the literals inside it are what it came to read. Following what such
// a helper returns accuses the idiom itself — `team_owner_permission_guard` in
// `rg-core/src/review/codeowners.rs` and `serde_fields`' type text are both
// written exactly this way.
runCase('a helper that bounds in the code view and returns the original slice is silent', {
  body: STRING_BEARING.replace(
    '    #[test]',
    `    fn permission_guard(source: &str) -> &str {
        const PREFIX: &str = "matches!(permission, ";

        let code = rust_source::rust_code_only(source);
        let start = code.find(PREFIX).map(|at| at + PREFIX.len()).expect("the guard must stay");
        let end = code[start..]
            .find(')')
            .map(|relative| start + relative)
            .expect("the guard must close");
        &source[start..end]
    }

    #[test]`,
  ).replace(
    '        BODY',
    `        let guard = permission_guard(&source);
        assert!(guard.split('|').count() >= 2);`,
  ),
  expect: { red: false, silent: ['`guard`'] },
});

// The normalizer closure keys on a NAME, and a name is not a function. A `fn
// new` in a shared module that reaches a view turned every `Vec::new()` in the
// corpus into a laundering call — which is how a `serde_fields` with its code
// view removed stayed a normalizer, and its caller stayed green. A capitalised
// `::` qualifier is a type, and its method is somebody else's.
runCase('a shared `fn new` does not make a normalizer of every `Vec::new()`', {
  support: `${SUPPORT}
pub(crate) fn new(text: &str) -> String {
    rust_code_only(text)
}
`,
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn contract(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        if source.contains("record_audit(") {
            out.push(String::new());
        }
        out
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        assert!(!contract(source).is_empty());
    }
}
`,
  expect: { red: true, mentions: ['handed to `contract`'] },
});

// The producer tag reaches the binding and stops there. Both shapes the reader
// knew key on the CALL — `let x = producer()` and `for pat in producer()` — so a
// consumer that binds the collection on one line and takes it apart on a later
// one is read by neither, and it fell silent for an honest reason: `sources.len()`
// is not an assertion and `for (_, text) in &sources` is not a call.
// `crates/rg-mcp/src/lib.rs` is written that way, and the crate's whole `const
// DEFAULT_*` census could have been rebuilt on a comment-live view with the gate
// green (card_ecda7101bf7d).
runCase('a collection bound by name and destructured on a later line is followed', {
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
    fn every_default_is_stated() {
        let sources = workspace_sources();
        assert!(sources.len() >= 1, "the walk found nothing, and an empty census agrees with anything");
        for (_, text) in &sources {
            assert!(text.contains("const DEFAULT_STAGE"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`text`', '.contains('] },
});

// The same loop through the code view. Nothing about the consumer changed — only
// what the bytes passed through — so the silence here is what says the new shape
// is about the view and not about `for`.
runCase('the same later-line loop through a code view is silent', {
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
    fn every_default_is_stated() {
        let sources = workspace_sources();
        assert!(sources.len() >= 1, "the walk found nothing, and an empty census agrees with anything");
        for (_, text) in &sources {
            assert!(rust_source::production_rust_code_only(text).contains("const DEFAULT_STAGE"));
        }
    }
}
`,
  expect: { red: false },
});

// And why the producer's slot has to travel with the tag instead of the whole
// pattern being tainted: slot 0 of the pair is a PATH, and `path.starts_with(…)`
// is a string operation on a directory name. A reader that tagged both halves
// would report it — the ratchet nobody keeps green. The slot is a fact about the
// producer, read out of its own body, so the caller may spell the pattern however
// it likes.
runCase('the path slot survives the later-line loop untainted', {
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
    fn every_default_is_stated() {
        let sources = workspace_sources();
        assert!(sources.len() >= 1, "the walk found nothing, and an empty census agrees with anything");
        for (path, source) in &sources {
            if path.starts_with("rg-core/src/audit/") {
                continue;
            }
            assert!(source.contains("const DEFAULT_STAGE"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`source`'], silent: ['`path`'] },
});

// The producer that puts the bytes in the pair under a SECOND name — `let text =
// read_to_string(&path)?; let production = view(&text); sources.push((path,
// production))` — which is the walk `crates/rg-mcp/src/lib.rs` writes and the one
// whose view a mutation removes. The tuple names `production`, not the read, so a
// slot search that stopped at the read's own name found none, and with no slot
// the loop above has to stay silent rather than guess which half is the program.
runCase('a producer that relays the bytes through a second name still spells its slot', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    fn workspace_sources() -> Vec<(String, String)> {
        let mut files = Vec::new();
        rust_source::rust_files("crates", &mut files);
        let mut sources = Vec::new();

        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let production = text.clone();
            sources.push((file, production));
        }

        sources
    }

    #[test]
    fn every_default_is_stated() {
        let sources = workspace_sources();
        assert!(sources.len() >= 1, "the walk found nothing, and an empty census agrees with anything");
        for (path, source) in &sources {
            if path.starts_with("rg-core/src/audit/") {
                continue;
            }
            assert!(source.contains("const DEFAULT_STAGE"));
        }
    }
}
`,
  expect: { red: true, mentions: ['`source`'], silent: ['`path`'] },
});

// The other half of that same rejection, and the phantom it produced. A
// constructor of a type the FILE implements is not somebody else's method:
// `Views::new(…)` applies the view in its own body, so a helper that calls it
// launders exactly the way one calling the view directly does. Rejecting it
// with `Vec::new()` reported this helper as a plain grep — and, one construct
// over, made every producer that launders through a constructor a phantom whose
// consumers could not be followed at all (card_53a95b2b6217).
const CONSTRUCTOR_VIEW = `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    struct Views {
        code: String,
    }

    impl Views {
        fn new(text: &str) -> Self {
            Self {
                code: rust_source::production_rust_code_only(text),
            }
        }

        fn declares(&self, item: &str) -> bool {
            self.code.contains(item)
        }
    }

    fn contract(source: &str) -> bool {
        Views::new(source).declares("record_audit(")
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        assert!(contract(source));
    }
}
`;

runCase('a constructor of a locally implemented type launders like the view it calls', {
  body: CONSTRUCTOR_VIEW,
  expect: { red: false },
});

// And the same helper with the view taken out of the constructor. Nothing about
// the call site changed — only what the constructor does with the bytes — so the
// redness here is what says the laundering is about the view and not about the
// `::`.
runCase('the same constructor with its view removed is reported', {
  body: CONSTRUCTOR_VIEW.replace(
    'code: rust_source::production_rust_code_only(text),',
    'code: text.to_owned(),',
  ),
  expect: { red: true, mentions: ['`source`', 'handed to `contract`'] },
});

// `Self::new(…)` inside `impl Views` is the same method under the spelling a
// constructor is most often written in, so the chain through it is followed the
// same way.
runCase('a constructor reached through `Self` is the same method', {
  body: CONSTRUCTOR_VIEW.replace(
    `        fn declares(&self, item: &str) -> bool {
            self.code.contains(item)
        }`,
    `        fn of(text: &str) -> Self {
            Self::new(text)
        }

        fn declares(&self, item: &str) -> bool {
            self.code.contains(item)
        }`,
  ).replace('Views::new(source).declares(', 'Views::of(source).declares('),
  expect: { red: false },
});

// Why the answer is keyed on the PAIR and not on the method name. Two types of
// one file may both spell `fn new`, and only one of them touches a view — a
// name-keyed answer laundered both, which is `Vec::new()` again with the
// collision moved inside the file instead of across it.
runCase('a same-named constructor on another type of the same file launders nothing', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    struct Views {
        code: String,
    }

    impl Views {
        fn new(text: &str) -> Self {
            Self {
                code: rust_source::production_rust_code_only(text),
            }
        }
    }

    struct Cursor {
        at: usize,
    }

    impl Cursor {
        fn new(at: usize) -> Self {
            Self { at }
        }
    }

    fn contract(source: &str) -> bool {
        let _ = Cursor::new(0);
        source.contains("record_audit(")
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        assert!(contract(source));
    }
}
`,
  expect: { red: true, mentions: ['handed to `contract`'] },
});

// The live shape the phantom was found in: a walk that reads each file and puts
// the VIEWS of it into a struct. The producer hands back nothing raw, so it is
// not a producer — and with the constructor unread it was one, with no slot,
// which is why the whole `for pat in &NAME` mechanism had to be locked to
// "slot known" (card_ecda7101bf7d). `crates/rg-ci/src/config.rs` is written
// this way.
const CONSTRUCTOR_WALK = `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    struct Entry {
        path: String,
        code: String,
    }

    impl Entry {
        fn new(path: String, text: &str) -> Self {
            Self {
                path,
                code: rust_source::production_rust_code_only(text),
            }
        }
    }

    fn workspace_sources() -> Vec<Entry> {
        let mut files = Vec::new();
        rust_source::rust_files("crates", &mut files);
        let mut sources = Vec::new();

        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            sources.push(Entry::new(file, &text));
        }

        sources
    }

    #[test]
    fn every_default_is_stated() {
        let sources = workspace_sources();
        assert!(sources.len() >= 1, "the walk found nothing, and an empty census agrees with anything");
        for entry in &sources {
            assert!(entry.code.contains("const DEFAULT_STAGE"));
        }
    }
}
`;

runCase('a walk that stores the views of each file is not a producer of raw bytes', {
  body: CONSTRUCTOR_WALK,
  expect: { red: false },
});

// And the mutation that shape exists to catch: the constructor stops applying
// the view, and the bytes the walk read reach the census raw.
runCase('the same walk with the constructor no longer viewing is reported', {
  body: CONSTRUCTOR_WALK.replace(
    'code: rust_source::production_rust_code_only(text),',
    'code: text.to_owned(),',
  ),
  expect: { red: true, mentions: ['`text`', 'handed to `new`'] },
});

// A transform that hands the SAME bytes back. `trim()` returns a `&str` into
// the very bytes it was given — comments, `#[cfg(test)]` modules and string
// literals all still in there — but the reader asked only the FIRST call in the
// chain, found `trim` in no assertion list, and walked away with the read
// counted and the `contains` behind it never examined.
runCase('a passthrough transform between the bytes and the grep is still reported', {
  body: RAW_BINDING.replace(
    'assert!(source.contains("record_audit("));',
    'assert!(source.trim().contains("record_audit("));',
  ),
  expect: { red: true, mentions: ['`source`', '.contains('] },
});

// Several of them in a row, so what is proved is the winding and not one
// special-cased hop — and `replace` carries an argument, which is where a
// reader that stops at the first `)` loses the rest of the chain.
runCase('a chain of passthrough transforms is wound past to the assertion', {
  body: RAW_BINDING.replace(
    'assert!(source.contains("record_audit("));',
    'assert!(source.to_lowercase().replace("\\r\\n", "\\n").trim().contains("record_audit("));',
  ),
  expect: { red: true, mentions: ['`source`', '.contains('] },
});

// The unbound half of the same word. `include_str!(…).trim().contains(…)` names
// nothing, so it is judged where it is written — by the other branch, through
// the same reader.
runCase('an unbound `include_str!` greped through a passthrough transform is reported', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        assert!(include_str!("../../other/src/writer.rs").trim().contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['.contains('] },
});

// The other side, so winding is a decision about WHERE the bytes came from and
// not a new accusation of its own: the identical chain on a view's result is a
// guard trimming its own view, and there is nothing to report.
runCase('the same chain on a viewed read accuses nobody', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        let code = rust_source::production_rust_code_only(include_str!("../../other/src/writer.rs"));
        assert!(code.trim().contains("record_audit("));
    }
}
`,
  expect: { red: false },
});

// The same word spelled as a rename. `let production = source.trim();` binds a
// `&str` into the very bytes `source` holds, so the grep of `production` is the
// grep of the file — but a reader that followed only the shims read the `trim`
// as a new value and never registered the second name at all.
runCase('a rename through a passthrough transform is still the bytes', {
  body: RAW_BINDING.replace(
    'assert!(source.contains("record_audit("));',
    `let production = source.trim();
        assert!(production.contains("record_audit("));`,
  ),
  expect: { red: true, mentions: ['`production`', 'same `.rs` bytes as `source`'] },
});

// And spelled as a transform of what a view handed back. The string-bearing
// view finishes nothing — the comments and the literals are still in what it
// returns — so trimming its result and greping that is the defect the
// string-bearing hop exists to catch, one `.trim()` further out.
runCase('what a string-bearing view handed back is followed through a passthrough transform', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let text = rust_source::production_rust_source(source).trim();
        assert!(text.contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['`text`', '`production_rust_source`'] },
});

// The reverse side of both: a code view ends the conversation, so trimming ITS
// result is a guard tidying its own view and there is nothing left to follow.
runCase('a rename off a code view is not followed through the transform', {
  body: `#[cfg(test)]
mod tests {
    mod rust_source {
        include!("../../../../tests/support/rust_source.rs");
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let code = rust_source::production_rust_code_only(source);
        let production = code.trim();
        assert!(production.contains("record_audit("));
    }
}
`,
  expect: { red: false },
});

// A local `fn` wearing the view's name. The body is `text.to_owned()` — it
// answers nothing about comments, literals or `#[cfg(test)]` items — and under
// the name `some_local_helper` the ratchet says so. The name was the whole
// difference: the check seeded on the spelling and asked nothing about the
// function behind it, so this exact body went green (card_e0f4ada65cee), which
// is the mechanism that hid the third and fourth copies of the `#[cfg(test)]`
// reader.
const IMPOSTOR = `#[cfg(test)]
mod tests {
    fn production_rust_code_only(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let masked = production_rust_code_only(&source);
        assert!(masked.contains("record_audit("));
    }
}
`;

runCase('a local `fn` wearing the view name launders nothing', {
  body: IMPOSTOR,
  expect: {
    red: true,
    mentions: ['guard.rs', '`source`', 'does not resolve to the view'],
  },
});

// The same body under a name nobody seeds on. It was already reported, and it
// has to stay reported — the fix is "resolve the name", not "distrust the
// view", and a case that only pins the impostor would pass with the reader
// blind to both.
runCase('the same body under an ordinary name is reported as it always was', {
  body: IMPOSTOR.split('production_rust_code_only').join('some_local_helper'),
  expect: { red: true, mentions: ['handed to `some_local_helper`'] },
});

// Cheaper still: declare nothing at all and simply spell the name at the call
// site. There is no such function anywhere in the fixture, and the check used
// to accept the read as laundered on the strength of the four identifiers.
runCase('a view name spelled at a call site that resolves to no view is reported', {
  body: `#[cfg(test)]
mod tests {
    use crate::common::fake_view::production_rust_code_only;

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let masked = production_rust_code_only(&source);
        assert!(masked.contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['does not resolve to the view'] },
});

// The counter-danger, and the reason the rule is "resolve" rather than
// "declared in tests/support/ or nowhere": `crates/rg-http/tests/integration/
// common/source_scan.rs` declares three `pub fn`s carrying the view names whose
// whole body is one line of delegation into the `include!`d module. Those are
// the view, reached the way an integration test tree reaches it, and a rule
// that reddened them would redden an honest tree.
const DELEGATE = `mod rust_source {
    include!("../../../../tests/support/rust_source.rs");
}

pub fn production_rust_code_only(text: &str) -> String {
    rust_source::production_rust_code_only(text)
}
`;

runCase('a one-line delegation in a shared module is the view', {
  files: { 'crates/demo/tests/integration/common/source_scan.rs': DELEGATE },
  body: `#[cfg(test)]
mod tests {
    use crate::common::source_scan::production_rust_code_only;

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let masked = production_rust_code_only(&source);
        assert!(masked.contains("record_audit("));
    }
}
`,
  expect: { red: false },
});

// And the same shared module with the delegation replaced by the impostor's
// body. Nothing about the guard changed — only what the wrapper it imports
// actually does — so this is the pair that says the ratchet reads the wrapper
// rather than its path.
runCase('a shared module whose wrapper stopped delegating launders nothing', {
  files: {
    'crates/demo/tests/integration/common/source_scan.rs': DELEGATE.replace(
      'rust_source::production_rust_code_only(text)',
      'text.to_owned()',
    ),
  },
  body: `#[cfg(test)]
mod tests {
    use crate::common::source_scan::production_rust_code_only;

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let masked = production_rust_code_only(&source);
        assert!(masked.contains("record_audit("));
    }
}
`,
  expect: { red: true, mentions: ['does not resolve to the view'] },
});

// The other half of "a name is not a behaviour", one level up from the seeds:
// `isShared` makes every `fn` under `tests/support/` or any `common/` directory
// a laundering NAME for the whole workspace, so a genuine `masked_body` in one
// crate's shared module used to launder an unrelated `fn masked_body` in
// another crate. The pair is the proof — the same guard, with and without the
// shared module that has nothing to do with it.
const HOMONYM_GUARD = `#[cfg(test)]
mod tests {
    fn masked_body(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn the_writer_is_still_wired() {
        let source = include_str!("../../other/src/writer.rs");
        let masked = masked_body(&source);
        assert!(masked.contains("record_audit("));
    }
}
`;

runCase("a shared normalizer in another crate does not launder this file's homonym", {
  files: {
    'crates/alpha/tests/integration/common/scan.rs': `mod rust_source {
    include!("../../../../../tests/support/rust_source.rs");
}

pub fn masked_body(text: &str) -> String {
    rust_source::production_rust_code_only(text)
}
`,
  },
  body: HOMONYM_GUARD,
  expect: { red: true, mentions: ['handed to `masked_body`'] },
});

// And the control it is only meaningful against: with no such shared module in
// the fixture at all, the very same guard is red for the very same reason. A
// case that pinned only the first half would pass with the reader blind to
// both.
runCase('the same homonym guard with no shared module at all is reported', {
  body: HOMONYM_GUARD,
  expect: { red: true, mentions: ['handed to `masked_body`'] },
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
    + 'the bytes, the string-bearing view finishes nothing while the two-view idiom '
    + 'built on it stays silent, a shared `fn new` launders no `Vec::new()` while a '
    + 'constructor of a type the file implements launders like the view it calls and its '
    + 'same-named neighbour on another type launders nothing, a walk a '
    + 'function spells itself is still a walk, a passthrough transform is wound past to the '
    + 'assertion behind it whether it is spelled as a chain, as a rename or on what a '
    + 'string-bearing view handed back, while the same transform on a code view accuses nobody, '
    + 'the test-inclusive view is an intent rather than an '
    + 'exclusion, a view name is read as the view only where it resolves to one — a local `fn` '
    + 'wearing it, a call site that merely spells it and a shared wrapper that stopped '
    + 'delegating all launder nothing, while the one-line delegation an integration tree '
    + 'imports still does, a shared name in one crate launders no homonym in another, '
    + 'and a reader that stops seeing the corpus is refused',
);
