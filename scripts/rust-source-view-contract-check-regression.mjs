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

if (failed > 0) {
  console.error(`❌ rust-source-view mutation stand: ${failed} case(s) failed`);
  process.exit(1);
}
console.log(
  '✅ rust-source-view mutation stand: a raw read is reported bound and unbound, a local helper '
    + 'launders only by reaching a named view, the test-inclusive view is an intent rather than an '
    + 'exclusion, and a reader that stops seeing the corpus is refused',
);
