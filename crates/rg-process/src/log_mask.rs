//! Masking secrets out of a job log, whole or while it streams
//! (card_dffba24313ed).
//!
//! A log used to be masked once, as one string, after the job ended. A log
//! that travels in pieces while the job runs cannot be masked piece by piece:
//! a secret cut by the border of two pieces is whole in neither, and both
//! halves would reach the database and every WebSocket subscriber. So the
//! masking is one rule with two drivers:
//!
//! - **What is masked.** Every byte that lies inside an occurrence of any form
//!   of any secret ([`masked_forms`]) is covered, overlapping occurrences
//!   included; each maximal run of covered bytes becomes one [`MASK`]. The
//!   rule speaks about bytes, not about an order of replacements, so it has
//!   no "which secret wins" question: two secrets that overlap in the output
//!   leave nothing of either.
//! - **When a byte is decided.** An occurrence covering a byte starts at most
//!   `longest - 1` bytes before it and ends at most `longest - 1` bytes after
//!   it, where `longest` is the longest form. A stream ([`MaskStream`]) emits
//!   a byte only once `longest - 1` bytes have arrived after it, and keeps
//!   `longest - 1` bytes before the first byte it has yet to emit, so every
//!   occurrence that covers an emitted byte was seen whole. Whether the
//!   previous emitted byte was masked carries over, so a run of covered bytes
//!   cut by a border is still one [`MASK`].
//!
//! [`LogMask::mask`] is the stream fed the whole text at once, which is why
//! the two give the same text for every way of cutting it — a property the
//! tests check at every cut position, not a hope.
//!
//! What a stream holds is bounded by the forms, not by the log: at most
//! `2 * (longest - 1)` bytes plus the piece being pushed.

use std::sync::Arc;

use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};

/// What a masked run of the log reads as.
pub const MASK: &str = "***";

/// Fragments of a secret shorter than this are not masked: replacing them
/// would corrupt ordinary output without keeping anything secret.
const MIN_FRAGMENT_BYTES: usize = 4;

/// GNU `base64` wraps its output at 76 columns by default.
const BASE64_WRAP_COLUMNS: usize = 76;

/// Every form of `secrets` a job log might show: each secret, each line of a
/// multiline one, the padded, unpadded and URL-safe base64 of each of those
/// (with and without the newline `echo` adds, and wrapped at 76 columns), and
/// their percent encodings in both hex cases. Forms shorter than four bytes
/// are left out.
pub fn masked_forms(secrets: &[String]) -> Vec<String> {
    let mut forms = std::collections::BTreeSet::new();
    for secret in secrets {
        for value in std::iter::once(secret.as_str()).chain(secret.lines()) {
            if value.len() < MIN_FRAGMENT_BYTES {
                continue;
            }
            forms.insert(value.to_owned());
            for bytes in [value.as_bytes().to_vec(), format!("{value}\n").into_bytes()] {
                for encoded in [
                    STANDARD.encode(&bytes),
                    STANDARD_NO_PAD.encode(&bytes),
                    URL_SAFE.encode(&bytes),
                    URL_SAFE_NO_PAD.encode(&bytes),
                ] {
                    if encoded.len() > BASE64_WRAP_COLUMNS {
                        forms.insert(
                            encoded
                                .as_bytes()
                                .chunks(BASE64_WRAP_COLUMNS)
                                .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                    forms.insert(encoded);
                }
            }
            let encoded = urlencoding::encode(value).into_owned();
            // Both `%2F` and `%2f` are valid percent encodings.
            let mut lower_hex = encoded.as_bytes().to_vec();
            for index in 0..lower_hex.len().saturating_sub(2) {
                if lower_hex[index] == b'%' {
                    lower_hex[index + 1] = lower_hex[index + 1].to_ascii_lowercase();
                    lower_hex[index + 2] = lower_hex[index + 2].to_ascii_lowercase();
                }
            }
            forms.insert(String::from_utf8(lower_hex).expect("percent encoding is ASCII"));
            forms.insert(encoded);
        }
    }
    forms.into_iter().collect()
}

/// Finds every occurrence of every form, overlapping ones included.
enum Matcher {
    Automaton(aho_corasick::AhoCorasick),
    /// The automaton could not be built (more forms than its state ids
    /// address): slower, and the same answer.
    Naive(Vec<String>),
}

impl Matcher {
    fn occurrences(&self, haystack: &str, mut found: impl FnMut(usize, usize)) {
        match self {
            Matcher::Automaton(automaton) => {
                for occurrence in automaton.find_overlapping_iter(haystack) {
                    found(occurrence.start(), occurrence.end());
                }
            }
            Matcher::Naive(forms) => {
                for form in forms {
                    let mut from = 0;
                    while let Some(at) = haystack[from..].find(form.as_str()) {
                        let start = from + at;
                        found(start, start + form.len());
                        from = next_char_boundary(haystack, start + 1);
                        if from >= haystack.len() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// The secrets of one job, ready to mask its log with.
///
/// Cheap to clone: the matcher is shared.
#[derive(Clone)]
pub struct LogMask {
    matcher: Option<Arc<Matcher>>,
    longest: usize,
}

impl LogMask {
    /// The mask for `secrets`, in every form [`masked_forms`] lists.
    pub fn new(secrets: &[String]) -> Self {
        let forms = masked_forms(secrets);
        let longest = forms.iter().map(String::len).max().unwrap_or(0);
        if forms.is_empty() {
            return Self {
                matcher: None,
                longest,
            };
        }
        let matcher = match aho_corasick::AhoCorasick::builder()
            .match_kind(aho_corasick::MatchKind::Standard)
            .build(&forms)
        {
            Ok(automaton) => Matcher::Automaton(automaton),
            Err(error) => {
                tracing::warn!(
                    forms = forms.len(),
                    %error,
                    "the log mask automaton could not be built; masking form by form"
                );
                Matcher::Naive(forms)
            }
        };
        Self {
            matcher: Some(Arc::new(matcher)),
            longest,
        }
    }

    /// The length in bytes of the longest form masked: how far a stream has
    /// to look ahead before it may emit a byte.
    pub fn longest_form(&self) -> usize {
        self.longest
    }

    /// `input` masked as a whole.
    pub fn mask(&self, input: &str) -> String {
        let mut stream = self.stream();
        let mut masked = stream.push(input);
        masked.push_str(&stream.finish());
        masked
    }

    /// A stream that masks a log arriving in pieces, with the same result as
    /// [`Self::mask`] over their concatenation.
    pub fn stream(&self) -> MaskStream {
        MaskStream {
            mask: self.clone(),
            buffer: String::new(),
            unemitted: 0,
            masking: false,
        }
    }
}

/// A log being masked while it arrives (see the module docs).
///
/// Cloning one forks it: a caller that may have to retry a piece keeps the
/// stream as it was and takes the clone only once the piece is written.
#[derive(Clone)]
pub struct MaskStream {
    mask: LogMask,
    /// Already emitted context, then the bytes not yet emitted.
    buffer: String,
    /// Where in `buffer` the bytes not yet emitted begin.
    unemitted: usize,
    /// Whether the last byte emitted was masked.
    masking: bool,
}

impl MaskStream {
    /// Take the next piece of the log; returns what of the log can now be
    /// emitted, masked. The rest waits for the next piece or [`Self::finish`].
    pub fn push(&mut self, piece: &str) -> String {
        self.buffer.push_str(piece);
        let look_ahead = self.mask.longest.saturating_sub(1);
        let cut = floor_char_boundary(&self.buffer, self.buffer.len().saturating_sub(look_ahead))
            .max(self.unemitted);
        self.emit_until(cut)
    }

    /// The end of the log: emit everything still held.
    pub fn finish(&mut self) -> String {
        let masked = self.emit_until(self.buffer.len());
        self.buffer.clear();
        self.unemitted = 0;
        self.masking = false;
        masked
    }

    /// A new stream with this one's mask, holding nothing: for text that does
    /// not follow what this one was fed.
    pub fn restarted(&self) -> MaskStream {
        self.mask.stream()
    }

    /// Bytes held between calls: the context and what is not emitted yet.
    pub fn held_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn emit_until(&mut self, cut: usize) -> String {
        let start = self.unemitted;
        let mut masked = String::with_capacity(cut - start);
        let mut covered = Vec::new();
        if let Some(matcher) = &self.mask.matcher {
            matcher.occurrences(&self.buffer, |from, to| {
                if to > start && from < cut {
                    covered.push((from.max(start), to.min(cut)));
                }
            });
        }
        covered.sort_unstable();
        let mut position = start;
        for (from, to) in covered {
            if to <= position {
                continue;
            }
            if from > position {
                masked.push_str(&self.buffer[position..from]);
                self.masking = false;
            }
            if !self.masking {
                masked.push_str(MASK);
                self.masking = true;
            }
            position = to;
        }
        if position < cut {
            masked.push_str(&self.buffer[position..cut]);
            self.masking = false;
        }
        // Keep what an occurrence covering the next byte to emit could start
        // in, and nothing before it.
        let keep_from = floor_char_boundary(
            &self.buffer,
            cut.saturating_sub(self.mask.longest.saturating_sub(1)),
        );
        self.buffer.drain(..keep_from);
        self.unemitted = cut - keep_from;
        masked
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Turns the bytes of a pipe, read in arbitrary pieces, into text: the same
/// text `String::from_utf8_lossy` makes of all of them at once. A character
/// whose bytes are split between two reads waits for the rest.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    /// Longest valid prefix of an incomplete character held between reads.
    const MAX_PENDING: usize = 3;

    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::with_capacity(self.pending.len());
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).expect("checked valid"));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[invalid..];
                        }
                        // An incomplete character at the end: wait for more.
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        debug_assert!(rest.len() <= Self::MAX_PENDING);
        self.pending = rest.to_vec();
        text
    }

    /// The end of the pipe: an incomplete character left over is invalid.
    pub fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streamed(mask: &LogMask, pieces: &[&str]) -> String {
        let mut stream = mask.stream();
        let mut out = String::new();
        for piece in pieces {
            out.push_str(&stream.push(piece));
        }
        out.push_str(&stream.finish());
        out
    }

    fn every_form(secret: &str) -> Vec<String> {
        let mut forms = vec![
            secret.to_string(),
            STANDARD.encode(secret),
            STANDARD_NO_PAD.encode(secret),
            URL_SAFE.encode(secret),
            URL_SAFE_NO_PAD.encode(format!("{secret}\n")),
            urlencoding::encode(secret).into_owned(),
        ];
        forms.dedup();
        forms
    }

    #[test]
    fn a_secret_cut_at_every_position_never_survives_the_stream() {
        let secret = "s3cr3t/with+chars=and ünïcödé";
        let mask = LogMask::new(&[secret.to_string()]);
        for form in every_form(secret) {
            let log = format!("before {form} after\n");
            let whole = mask.mask(&log);
            assert!(!whole.contains(&form), "{whole}");
            for cut in (0..=log.len()).filter(|cut| log.is_char_boundary(*cut)) {
                let (head, tail) = log.split_at(cut);
                let out = streamed(&mask, &[head, tail]);
                assert_eq!(out, whole, "cut at {cut} of {log:?}");
                assert!(!out.contains(&form), "cut at {cut}: {out}");
            }
        }
    }

    #[test]
    fn a_secret_fed_one_character_at_a_time_is_masked_like_the_whole_text() {
        let secrets = vec![
            "-----BEGIN KEY-----\nlong-private-material\n-----END KEY-----".to_string(),
            "token?".to_string(),
        ];
        let mask = LogMask::new(&secrets);
        let log = format!(
            "a {} b {} c long-private-material d token?token? e {}",
            secrets[0],
            STANDARD.encode(&secrets[0]),
            urlencoding::encode(&secrets[1]),
        );
        let pieces: Vec<String> = log.chars().map(String::from).collect();
        let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
        let out = streamed(&mask, &pieces);
        assert_eq!(out, mask.mask(&log));
        assert!(!out.contains("private-material"), "{out}");
        assert!(!out.contains("token?"), "{out}");
    }

    #[test]
    fn three_way_cuts_agree_with_the_whole_text() {
        let secret = "abcdefgh";
        let mask = LogMask::new(&[secret.to_string(), "ghijklmn".to_string()]);
        let log = "xxabcdefghijklmnyy abcdefgh abcdefghabcdefgh zz";
        let whole = mask.mask(log);
        assert_eq!(whole, "xx***yy *** *** zz");
        for first in 0..=log.len() {
            for second in first..=log.len() {
                let out = streamed(&mask, &[&log[..first], &log[first..second], &log[second..]]);
                assert_eq!(out, whole, "cuts at {first} and {second}");
            }
        }
    }

    #[test]
    fn overlapping_secrets_leave_nothing_of_either() {
        let mask = LogMask::new(&["abcd".to_string(), "cdefghij".to_string()]);
        assert_eq!(mask.mask("abcdefghij!"), "***!");
    }

    #[test]
    fn what_a_stream_holds_is_bounded_by_the_longest_form_not_by_the_log() {
        let secret = "x".repeat(300);
        let mask = LogMask::new(&[secret]);
        let longest = mask.longest_form();
        let mut stream = mask.stream();
        let piece = "y".repeat(1000);
        let mut emitted = 0;
        for _ in 0..1000 {
            emitted += stream.push(&piece).len();
            assert!(
                stream.held_bytes() <= 2 * longest,
                "held {} bytes, longest form {longest}",
                stream.held_bytes()
            );
        }
        emitted += stream.finish().len();
        assert_eq!(emitted, 1000 * 1000);
    }

    #[test]
    fn a_log_with_no_secrets_passes_through_as_it_arrives() {
        let mask = LogMask::new(&[]);
        let mut stream = mask.stream();
        assert_eq!(stream.push("abc"), "abc");
        assert_eq!(stream.held_bytes(), 0);
        assert_eq!(stream.finish(), "");
    }

    #[test]
    fn the_naive_matcher_finds_what_the_automaton_finds() {
        let forms = masked_forms(&["abcd".to_string(), "aaaa".to_string()]);
        let automaton = Matcher::Automaton(
            aho_corasick::AhoCorasick::builder()
                .match_kind(aho_corasick::MatchKind::Standard)
                .build(&forms)
                .unwrap(),
        );
        let naive = Matcher::Naive(forms);
        let haystack = "aaaaaabcdabcd ü aaaa";
        let collect = |matcher: &Matcher| {
            let mut found = Vec::new();
            matcher.occurrences(haystack, |from, to| found.push((from, to)));
            found.sort_unstable();
            found
        };
        assert_eq!(collect(&automaton), collect(&naive));
    }

    #[test]
    fn split_utf8_reads_decode_like_the_whole_pipe() {
        let bytes: Vec<u8> = "añb€c😀d".bytes().chain([0xff, b'e', 0xe2, 0x82]).collect();
        let whole = String::from_utf8_lossy(&bytes).into_owned();
        for cut in 0..=bytes.len() {
            let mut stream = Utf8Stream::default();
            let mut text = stream.push(&bytes[..cut]);
            text.push_str(&stream.push(&bytes[cut..]));
            text.push_str(&stream.finish());
            assert_eq!(text, whole, "cut at {cut}");
        }
    }
}
