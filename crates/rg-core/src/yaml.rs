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
//! * **A redundant leading zero keeps the spelling.** Plain `0755` is the
//!   string `"0755"`, as under `serde_yaml`; see [`quote_leading_zero_decimals`].
//!
//! The parser is `serde-saphyr` (pure Rust). It replaced `serde_yaml`, which
//! sits on C-translated, unmaintained libyaml (RUSTSEC-2024-0449).

#![allow(clippy::disallowed_methods)] // the one module allowed to call the parser

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_saphyr::granit_parser::{Event, Parser, ScalarStyle};
use std::borrow::Cow;

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
    let input = quote_leading_zero_decimals(input);
    serde_saphyr::from_str_with_options(&input, options()).map_err(|e| YamlError(e.to_string()))
}

/// Quote every untagged plain scalar spelled as a decimal with a redundant
/// leading zero (`0755`, `-007`), so it reads as the string it was written as.
///
/// `serde_yaml` read those as strings. serde-saphyr skips the integer reading
/// for them and then takes the float one, so an untyped `0755` became `755.0`
/// — `chmod 0755` in a workflow matrix would have run as `chmod 755.0`. No
/// option restores the old meaning, so this pass does, before the real parse:
/// it walks the event stream (aliases are events here, not expansions, so the
/// pass is linear in the input) and wraps those scalars in single quotes, which
/// is exact because they hold only a sign and digits. A document the event
/// stream cannot read is passed through untouched, for the real parse to
/// refuse with its own message.
fn quote_leading_zero_decimals(input: &str) -> Cow<'_, str> {
    let mut spans = Vec::new();
    for event in Parser::new_from_str(input) {
        let Ok((event, span)) = event else {
            return Cow::Borrowed(input);
        };
        let Event::Scalar(value, ScalarStyle::Plain, _, None) = event else {
            continue;
        };
        if !is_leading_zero_decimal(&value) {
            continue;
        }
        let (Some(start), Some(end)) = (span.start.byte_offset(), span.end.byte_offset()) else {
            return Cow::Borrowed(input);
        };
        if input.get(start..end) != Some(value.as_ref()) {
            return Cow::Borrowed(input);
        }
        spans.push(start..end);
    }
    if spans.is_empty() {
        return Cow::Borrowed(input);
    }
    let mut quoted = String::with_capacity(input.len() + 2 * spans.len());
    let mut copied = 0;
    for span in spans {
        quoted.push_str(&input[copied..span.start]);
        quoted.push('\'');
        quoted.push_str(&input[span.clone()]);
        quoted.push('\'');
        copied = span.end;
    }
    quoted.push_str(&input[copied..]);
    Cow::Owned(quoted)
}

/// `0755`, `+007`, `-00` — but not `0`, `-0`, `0x1F`, `0o17` or `0.5`.
fn is_leading_zero_decimal(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    digits.len() > 1 && digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit())
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

    #[test]
    fn a_redundant_leading_zero_keeps_the_spelling() {
        let doc = parse(
            "mode: 0755\nneg: -007\nplus: +010\nzero: 0\nnegzero: -0\nhex: 0x1F\nfrac: 0.5\n\
             quoted: '0755'\ntagged: !!str 0755\nlist: [0644, 1]\n0755: key\nanchored: &m 0700\nalias: *m\n",
        );
        assert_eq!(doc["mode"], json!("0755"));
        assert_eq!(doc["neg"], json!("-007"));
        assert_eq!(doc["plus"], json!("+010"));
        assert_eq!(doc["zero"], json!(0));
        assert_eq!(doc["negzero"], json!(0));
        assert_eq!(doc["hex"], json!(31));
        assert_eq!(doc["frac"], json!(0.5));
        assert_eq!(doc["quoted"], json!("0755"));
        assert_eq!(doc["tagged"], json!("0755"));
        assert_eq!(doc["list"], json!(["0644", 1]));
        assert_eq!(doc["0755"], json!("key"));
        assert_eq!(doc["anchored"], json!("0700"));
        assert_eq!(doc["alias"], json!("0700"));
    }

    /// A typed number field refuses `0755`, as it did under `serde_yaml`,
    /// instead of quietly taking 755; a typed string field keeps the spelling.
    #[test]
    fn typed_fields_see_a_leading_zero_as_serde_yaml_did() {
        #[derive(serde::Deserialize)]
        struct Typed {
            mode: String,
        }
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Numeric {
            mode: i64,
        }
        let typed: Typed = from_str("mode: 0755\n").unwrap();
        assert_eq!(typed.mode, "0755");
        assert!(from_str::<Numeric>("mode: 0755\n").is_err());
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
}
