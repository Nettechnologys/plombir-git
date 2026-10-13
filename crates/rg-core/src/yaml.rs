//! The one door YAML comes through.
//!
//! Every YAML document the server reads arrives from someone it does not
//! trust: workflow files and issue templates from pushed repositories, gemspec
//! metadata and Helm charts from package uploads. This module is the only
//! caller of the parser (`clippy.toml` disallows the parser's entry points
//! everywhere else), so the hardening below holds for all of them at once:
//!
//! * **Bounded work.** An alias bomb ("billion laughs") is a few hundred bytes
//!   that expand into 10⁹ nodes. The alias replay ceiling stops it after
//!   [`MAX_REPLAYED_EVENTS`] replayed events, long before the tree is built.
//! * **YAML 1.2 scalars, as before.** `yes`, `no`, `on`, `off` stay strings —
//!   the meaning `serde_yaml` gave them and the one workflow authors rely on
//!   (`on:` is the trigger key of every workflow, not `true:`).
//! * **No construction from tags, no interpolation, no includes.** Tags are
//!   kept for callers that read them (gemspecs are tagged `!ruby/object:…`) and
//!   never build anything; `${…}` stays literal text (workflows are full of
//!   `${{ … }}`); `!include` is not compiled in.
//! * **Merge keys are ordinary keys**, as they were under `serde_yaml`: `<<`
//!   is read as a key named `<<`, not spliced into its mapping.
//!
//! The parser is `serde-saphyr` (pure Rust). It replaced `serde_yaml`, which
//! sits on C-translated, unmaintained libyaml (RUSTSEC-2024-0449).

#![allow(clippy::disallowed_methods)] // the one module allowed to call the parser

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Ceiling on events replayed from aliases across one document.
///
/// Hand-written YAML replays a handful of anchors a handful of times; the
/// default (1 000 000) would let a 300-byte bomb build a million-node tree
/// before failing.
pub const MAX_REPLAYED_EVENTS: usize = 100_000;

/// Ceiling on scalar text in one document, after alias replay. The largest
/// YAML inputs are 16 MiB in total (all workflows or issue templates of one
/// repository together), and no single document comes near it.
pub const MAX_SCALAR_BYTES: usize = 16 * 1024 * 1024;

/// A value together with the YAML tag written on it (`!ruby/object:Gem::Version`
/// → `Some("!ruby/object:Gem::Version")`), for callers that dispatch on tags.
/// Reading a tag never constructs anything.
pub use serde_saphyr::Tagged;

/// A YAML document that could not be read or written, with the parser's
/// message (line and column included when it has them).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct YamlError(String);

fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        legacy_octal_numbers: false,
        // The snippet repeats the offending input around the error; messages
        // here end up in API errors and logs, where one line reads better.
        with_snippet: false,
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: MAX_REPLAYED_EVENTS,
        },
        budget: serde_saphyr::budget! {
            max_total_scalar_bytes: MAX_SCALAR_BYTES,
        },
    }
}

/// Parse one YAML document into `T`.
pub fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, YamlError> {
    serde_saphyr::from_str_with_options(input, options()).map_err(|e| YamlError(e.to_string()))
}

/// Serialize `value` as one YAML document. Strings a YAML 1.1 reader would
/// take for a boolean, number or null (`yes`, `on`, `1.10`, `null`) come out
/// quoted, so Helm and other 1.1-era readers get them back as strings.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, YamlError> {
    serde_saphyr::to_string(value).map_err(|e| YamlError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn parse(input: &str) -> Value {
        from_str(input).unwrap_or_else(|e| panic!("{input:?} did not parse: {e}"))
    }

    #[test]
    fn yaml_1_1_booleans_stay_strings() {
        let doc = parse("on: push\na: yes\nb: no\nc: off\nd: y\ne: true\nf: false\n");
        assert_eq!(
            doc,
            json!({"on": "push", "a": "yes", "b": "no", "c": "off", "d": "y", "e": true, "f": false})
        );
    }

    #[test]
    fn numbers_keep_their_yaml_1_2_meaning() {
        let doc = parse("yaml12_octal: 0o17\nhex: 0x1F\nver: 1.10\nquoted: '1.10'\nexp: 1e3\n");
        assert_eq!(doc["yaml12_octal"], json!(15));
        assert_eq!(doc["hex"], json!(31));
        assert_eq!(
            doc["ver"],
            json!(1.1),
            "an unquoted version is a float, as before"
        );
        assert_eq!(doc["quoted"], json!("1.10"));
        assert_eq!(doc["exp"], json!(1000.0));
    }

    /// `serde_yaml` read an untyped `0755` as the string `"0755"`; serde-saphyr
    /// 1.3 skips the integer reading for a redundant leading zero and then
    /// takes the float one, `755.0`. A typed `String` field keeps the spelling
    /// either way, so only callers reading into an untyped value see this —
    /// workflow `env` / `with` / `matrix`, Helm values, gemspec metadata. They
    /// own the decision (card_d2db3ea7caee). If this assertion starts failing,
    /// the parser changed its mind: re-check those callers.
    #[test]
    fn leading_zero_decimal_is_a_known_divergence() {
        #[derive(serde::Deserialize)]
        struct Typed {
            mode: String,
        }
        let typed: Typed = from_str("mode: 0755\n").unwrap();
        assert_eq!(typed.mode, "0755");
        assert_eq!(parse("mode: 0755\n")["mode"], json!(755.0));
    }

    #[test]
    fn nulls_and_null_like_strings() {
        let doc = parse("a: ~\nb: null\nc:\nd: 'null'\ne: !!str null\n");
        assert_eq!(
            doc,
            json!({"a": null, "b": null, "c": null, "d": "null", "e": "null"})
        );
    }

    #[test]
    fn merge_keys_are_ordinary_keys() {
        let doc = parse("base: &b {x: 1}\njob:\n  <<: *b\n  y: 2\n");
        assert_eq!(doc["job"], json!({"<<": {"x": 1}, "y": 2}));
    }

    #[test]
    fn interpolation_syntax_stays_literal() {
        let doc = parse("run: echo ${{ github.ref }} ${HOME}\n");
        assert_eq!(doc["run"], json!("echo ${{ github.ref }} ${HOME}"));
    }

    /// Gemspec metadata is tagged Ruby objects. Untyped reading drops the
    /// tags and keeps the data; `Tagged` hands the tag to callers that want it.
    #[test]
    fn tags_are_kept_for_readers_and_never_construct() {
        let gemspec = "--- !ruby/object:Gem::Specification\nname: demo\nversion: !ruby/object:Gem::Version\n  version: '1.10'\n";
        let doc = parse(gemspec);
        assert_eq!(doc, json!({"name": "demo", "version": {"version": "1.10"}}));

        let tagged: Tagged<Value> = from_str(gemspec).unwrap();
        assert_eq!(tagged.1.as_deref(), Some("!ruby/object:Gem::Specification"));
        assert_eq!(tagged.0["name"], json!("demo"));
    }

    #[test]
    fn duplicate_keys_are_refused() {
        assert!(from_str::<Value>("a: 1\na: 2\n").is_err());
    }

    #[test]
    fn errors_name_the_line() {
        let err = from_str::<Value>("a: [1, 2\nb: 3\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("line"), "{err}");
        assert!(!err.contains('\n'), "one-line message expected: {err:?}");
    }

    /// Nine levels of ten aliases each: 10⁹ nodes from 400 bytes.
    fn billion_laughs() -> String {
        let mut doc = String::from("a0: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
        for level in 1..=9 {
            let prev = format!("*a{}", level - 1);
            let items = [prev.as_str(); 10].join(", ");
            doc.push_str(&format!("a{level}: &a{level} [{items}]\n"));
        }
        doc
    }

    /// Peak resident set of this process (nextest runs each test alone).
    #[cfg(target_os = "linux")]
    fn peak_rss_kib() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    #[test]
    fn alias_bomb_is_refused_quickly_and_small() {
        let doc = billion_laughs();
        #[cfg(target_os = "linux")]
        let before = peak_rss_kib();
        let started = std::time::Instant::now();
        let result = from_str::<Value>(&doc);
        let elapsed = started.elapsed();
        let err = result
            .expect_err("an alias bomb must not parse")
            .to_string();
        #[cfg(target_os = "linux")]
        {
            let grown_mib = (peak_rss_kib() - before) / 1024;
            assert!(grown_mib < 32, "peak RSS grew by {grown_mib} MiB");
        }
        assert!(err.to_lowercase().contains("alias"), "{err}");
        // Measured on a debug build: ~0.6 s and ~20 MiB at the 100 000 ceiling,
        // ~1.5 s and ~48 MiB at the parser's default (then the node budget, not
        // the alias ceiling, is what stops it). Memory is the assertion with
        // teeth; the clock only guards against a hang.
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "took {elapsed:?}"
        );
    }

    #[test]
    fn honest_anchors_still_work() {
        let doc = parse("defaults: &d {image: alpine, retries: 2}\na: *d\nb: *d\n");
        assert_eq!(doc["a"], doc["b"]);
        assert_eq!(doc["a"]["image"], json!("alpine"));
    }

    #[test]
    fn round_trip_keeps_strings_that_look_like_other_types() {
        let value = json!({"a": "yes", "b": "1.10", "c": "null", "d": "on", "e": "0755", "f": "~"});
        let text = to_string(&value).unwrap();
        assert_eq!(parse(&text), value, "serialized as:\n{text}");
        for word in ["yes", "on"] {
            assert!(
                !text.contains(&format!(": {word}\n")),
                "{word} left bare:\n{text}"
            );
        }
    }

    // ── Parity with `serde_yaml`, the parser this replaced ─────────────────
    // Goes away with `serde_yaml` itself (card_5d675575968e); the pinned
    // expectations above stay.

    const EDGE_CASES: &[&str] = &[
        "on: push\nenabled: yes\n",
        "b: 0o17\nc: 0x1F\nd: 1.10\ne: 1e3\nf: -0\n",
        "a: ~\nb: null\nc:\nd: 'null'\n",
        "a: 2001-12-14t21:59:43.10-05:00\nb: 12:30:00\n",
        "a: |\n  line one\n  line two\nb: >\n  folded\n  text\n",
        "a: \"quoted \\\"escapes\\\" \\u00e9\"\nb: 'single ''quote'''\n",
        "x: &x {k: v}\ny: *x\n",
        "- 1\n- two\n- {three: 3}\n- [4, 5]\n",
        "1: one\n2: two\n",
        "run: echo ${{ github.ref }}\n",
        "",
        "# only a comment\n",
        "a: !!str 123\nb: !!int '42'\n",
        "steps:\n  - uses: actions/checkout@v4\n    with:\n      fetch-depth: 0\n",
    ];

    fn old_parser(input: &str) -> Result<Value, String> {
        serde_yaml::from_str(input).map_err(|e| e.to_string())
    }

    fn assert_same_as_old(label: &str, input: &str) {
        let new = from_str::<Value>(input).map_err(|e| e.to_string());
        let old = old_parser(input);
        match (&new, &old) {
            (Ok(n), Ok(o)) => assert_eq!(n, o, "{label}"),
            (Err(_), Err(_)) => {}
            _ => panic!("{label}: new {new:?} vs old {old:?}"),
        }
    }

    #[test]
    fn edge_cases_parse_as_serde_yaml_did() {
        for input in EDGE_CASES {
            assert_same_as_old(&format!("{input:?}"), input);
        }
    }

    /// Every YAML file the repository ships, read by both parsers.
    #[test]
    fn repository_yaml_parses_as_serde_yaml_did() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in [".github", "deploy"] {
            collect_yaml(&root.join(dir), &mut files);
        }
        assert!(files.len() >= 10, "found only {files:?}");
        for path in files {
            let input = std::fs::read_to_string(&path).unwrap();
            assert_same_as_old(&path.display().to_string(), &input);
        }
    }

    fn collect_yaml(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_yaml(&path, out);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            ) {
                out.push(path);
            }
        }
    }
}
