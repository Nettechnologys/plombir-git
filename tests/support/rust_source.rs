// Shared, dependency-free Rust source helpers for workspace policy guards.
//
// This file is `include!`d by unit-test modules in more than one crate.  Keep
// it limited to lexical source questions: it is not a Rust parser, but every
// structural decision it does make comes from a byte-aligned code-only view.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallSite {
    pub(crate) line: usize,
    pub(crate) open_paren: usize,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StringField {
    pub(crate) line: usize,
    pub(crate) value: String,
}

fn blank_range(masked: &mut [u8], start: usize, end: usize) {
    for byte in &mut masked[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

fn starts_rust_token(bytes: &[u8], at: usize) -> bool {
    at == 0
        || !bytes[at - 1].is_ascii_alphanumeric() && bytes[at - 1] != b'_' && bytes[at - 1] < 0x80
}

fn char_literal_end(text: &str, quote: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = quote + 1;
    let next = *bytes.get(at)?;

    if next == b'\\' {
        at += 1;
        match *bytes.get(at)? {
            b'x' => at += 3,
            b'u' if bytes.get(at + 1) == Some(&b'{') => {
                let close = bytes[at + 2..].iter().position(|byte| *byte == b'}')?;
                at += close + 3;
            }
            b'\n' | b'\r' => return None,
            _ => at += 1,
        }
    } else {
        let ch = text.get(at..)?.chars().next()?;
        if matches!(ch, '\n' | '\r' | '\'') {
            return None;
        }
        at += ch.len_utf8();
    }

    (bytes.get(at) == Some(&b'\'')).then_some(at + 1)
}

/// `text` with Rust comments and literals replaced byte-for-byte by spaces.
///
/// Newlines and total byte length are preserved, so offsets and line numbers
/// in the result address the original source.  Nested block comments and
/// normal/byte/C/raw strings plus char/byte-char literals are recognized.
fn rust_source_view(text: &str, keep_doc_comments: bool) -> String {
    let bytes = text.as_bytes();
    let mut masked = bytes.to_vec();
    let mut at = 0;

    while at < bytes.len() {
        if bytes[at..].starts_with(b"//") {
            let end = bytes[at..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| at + relative);
            let is_doc_comment = bytes[at..].starts_with(b"//!")
                || bytes[at..].starts_with(b"///") && !bytes[at..].starts_with(b"////");
            if !keep_doc_comments || !is_doc_comment {
                blank_range(&mut masked, at, end);
            }
            at = end;
            continue;
        }

        if bytes[at..].starts_with(b"/*") {
            let is_doc_comment = bytes[at..].starts_with(b"/*!")
                || bytes[at..].starts_with(b"/**") && !bytes[at..].starts_with(b"/***");
            let mut depth = 1usize;
            let mut end = at + 2;
            while end < bytes.len() && depth > 0 {
                if bytes[end..].starts_with(b"/*") {
                    depth += 1;
                    end += 2;
                } else if bytes[end..].starts_with(b"*/") {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            if !keep_doc_comments || !is_doc_comment {
                blank_range(&mut masked, at, end);
            }
            at = end;
            continue;
        }

        let starts_token = starts_rust_token(bytes, at);
        let raw_prefix = if starts_token && bytes[at] == b'r' {
            Some(1usize)
        } else if starts_token
            && matches!(bytes[at], b'b' | b'c')
            && bytes.get(at + 1) == Some(&b'r')
        {
            Some(2)
        } else {
            None
        };
        if let Some(prefix_len) = raw_prefix {
            let mut hashes = 0usize;
            while bytes.get(at + prefix_len + hashes) == Some(&b'#') {
                hashes += 1;
            }
            if bytes.get(at + prefix_len + hashes) == Some(&b'"') {
                let mut end = at + prefix_len + hashes + 1;
                while end < bytes.len() {
                    if bytes[end] == b'"'
                        && end + 1 + hashes <= bytes.len()
                        && bytes[end + 1..end + 1 + hashes]
                            .iter()
                            .all(|byte| *byte == b'#')
                    {
                        end += hashes + 1;
                        break;
                    }
                    end += 1;
                }
                blank_range(&mut masked, at, end);
                at = end;
                continue;
            }
        }

        let string_prefix = if bytes[at] == b'"' {
            Some(0usize)
        } else if starts_token
            && matches!(bytes[at], b'b' | b'c')
            && bytes.get(at + 1) == Some(&b'"')
        {
            Some(1)
        } else {
            None
        };
        if let Some(prefix_len) = string_prefix {
            let mut end = at + prefix_len + 1;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end = (end + 2).min(bytes.len()),
                    b'"' => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            blank_range(&mut masked, at, end);
            at = end;
            continue;
        }

        let quote = if bytes[at] == b'\'' {
            Some(at)
        } else if starts_token && bytes[at] == b'b' && bytes.get(at + 1) == Some(&b'\'') {
            Some(at + 1)
        } else {
            None
        };
        if let Some(quote) = quote {
            if let Some(end) = char_literal_end(text, quote) {
                blank_range(&mut masked, at, end);
                at = end;
                continue;
            }
        }

        at += 1;
    }

    String::from_utf8(masked).expect("blanking UTF-8 bytes with ASCII preserves UTF-8")
}

pub(crate) fn rust_code_only(text: &str) -> String {
    rust_source_view(text, false)
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn is_function_declaration(code: &str, name_at: usize) -> bool {
    let line_start = code[..name_at].rfind('\n').map_or(0, |newline| newline + 1);
    code[line_start..name_at].split_whitespace().next_back() == Some("fn")
}

fn skip_code_whitespace(code: &str, mut at: usize) -> usize {
    while let Some(ch) = code[at..].chars().next() {
        if !ch.is_whitespace() {
            break;
        }
        at += ch.len_utf8();
    }
    at
}

/// The comma-separated arguments of `name(...)`, when `predicate` is exactly
/// that call.
///
/// Splitting on top-level commas only is what keeps `all(test, any(unix,
/// windows))` from being read as three siblings.
fn cfg_list_arguments<'a>(predicate: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let rest = predicate.trim().strip_prefix(name)?;
    let inner = rest.trim_start().strip_prefix('(')?.strip_suffix(')')?;

    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (at, ch) in inner.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments.push(inner[start..at].trim());
                start = at + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        arguments.push(tail);
    }

    Some(arguments)
}

/// Whether a `cfg` predicate is false in every build that is not a test build.
///
/// `all(test, unix)` is: dropping `test` drops the item. `any(test, unix)` is
/// not — the item still compiles on unix without `cfg(test)` — and neither is
/// `not(test)`, which is the production half of a pair. Anything this cannot
/// read is treated as production, so an unknown spelling keeps code visible to
/// a census rather than hiding it.
fn cfg_predicate_is_test_only(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    if let Some(arguments) = cfg_list_arguments(predicate, "all") {
        return arguments.iter().copied().any(cfg_predicate_is_test_only);
    }
    if let Some(arguments) = cfg_list_arguments(predicate, "any") {
        return !arguments.is_empty() && arguments.iter().copied().all(cfg_predicate_is_test_only);
    }
    false
}

/// Whether `attribute` gates the item that follows it to test builds.
///
/// Reads the `cfg` predicate rather than comparing the line to the literal
/// `#[cfg(test)]`: the folded form `#[cfg(all(test, unix))]` is exactly as
/// test-only, and a view that missed it entered every workspace census as
/// production code (card_38d725506ec6).
///
/// Answered here for readers outside this module too: a scan that walks bytes
/// rather than lines — `rg-http`'s `common/source_scan.rs` — asked the same
/// question with its own literal comparison, and answering it twice is how the
/// two readers drifted apart (card_0a6ec0937f91).
pub(crate) fn is_test_only_cfg_attribute(attribute: &str) -> bool {
    let Some(inner) = attribute
        .trim()
        .strip_prefix("#[")
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return false;
    };
    let Some(arguments) = cfg_list_arguments(inner, "cfg") else {
        return false;
    };

    arguments.len() == 1 && cfg_predicate_is_test_only(arguments[0])
}

/// The index of the last line of the attribute opened on `line`, if that line
/// opens one.
///
/// A `cfg` predicate long enough for rustfmt to wrap spans several lines, and a
/// reader that only ever looks at one line would take the opening `#[cfg(all(`
/// for a predicate naming nothing.
fn attribute_span_end(lines: &[&str], line: usize) -> Option<usize> {
    if !lines[line].trim_start().starts_with("#[") {
        return None;
    }

    let mut depth = 0usize;
    for (candidate, text) in lines.iter().enumerate().skip(line) {
        for byte in text.bytes() {
            match byte {
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if depth == 0 {
            return Some(candidate);
        }
    }

    None
}

/// The 1-based, inclusive line ranges occupied by test-only `#[cfg(…)]` items.
///
/// Braces are counted on the code-only view, so comments and literals cannot
/// close a test module early.  Each range ends with its item instead of turning
/// the first inline test module into a false "rest of file is tests" marker.
///
/// `code` is a byte-aligned code-only view, so a consumer that already holds
/// one — [`without_test_items`] here, `rg-http`'s `common/source_scan.rs` next
/// door — pays for a single pass and shares this reader instead of keeping a
/// second copy of it (card_0a6ec0937f91).
pub(crate) fn test_item_ranges(code: &str) -> Vec<std::ops::RangeInclusive<usize>> {
    let lines: Vec<&str> = code.lines().collect();
    let mut ranges = Vec::new();
    let mut line = 0;

    while line < lines.len() {
        let Some(attribute_end) = attribute_span_end(&lines, line) else {
            line += 1;
            continue;
        };
        if !is_test_only_cfg_attribute(&lines[line..=attribute_end].join("\n")) {
            line += 1;
            continue;
        }

        let mut depth = 0usize;
        let mut opened = false;
        let mut end = lines.len().saturating_sub(1);
        for (candidate, text) in lines.iter().enumerate().skip(attribute_end + 1) {
            for byte in text.bytes() {
                match byte {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if opened && depth == 0 || !opened && text.trim_end().ends_with(';') {
                end = candidate;
                break;
            }
        }

        ranges.push(line + 1..=end + 1);
        line = end + 1;
    }

    ranges
}

fn test_item_byte_ranges(code: &str) -> Vec<std::ops::Range<usize>> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    test_item_ranges(code)
        .into_iter()
        .map(|range| {
            let start = line_starts[range.start() - 1];
            let end = line_starts.get(*range.end()).copied().unwrap_or(code.len());
            start..end
        })
        .collect()
}

fn without_test_items(code: &str, view: String) -> String {
    let mut masked = view.into_bytes();
    for range in test_item_byte_ranges(code) {
        blank_range(&mut masked, range.start, range.end);
    }

    String::from_utf8(masked).expect("blanking UTF-8 bytes with ASCII preserves UTF-8")
}

/// The byte-aligned source with complete `#[cfg(test)]` items blanked.
///
/// Comments and literals outside test items are preserved for guards that need
/// to decode a real Rust attribute or expression after locating its boundary
/// in a code-only view. Newlines and byte offsets still address `text`.
pub(crate) fn production_rust_source(text: &str) -> String {
    let code = rust_code_only(text);
    without_test_items(&code, text.to_owned())
}

/// The byte-aligned code-only view with complete `#[cfg(test)]` items blanked.
///
/// This is the source view for production censuses: comments and literals
/// cannot manufacture facts, and inline test modules cannot keep a production
/// completeness guard green. Newlines and byte offsets still address `text`.
pub(crate) fn production_rust_code_only(text: &str) -> String {
    let code = rust_code_only(text);
    without_test_items(&code, code.clone())
}

/// Production Rust code plus production doc comments, byte-aligned to `text`.
///
/// This view is for guards over generated `--help` prose: normal comments and
/// every string-like literal are blanked, while `///`, `//!`, `/**` and `/*!`
/// comments remain visible. Complete test items, including their doc comments,
/// are blanked without hiding production items that follow them.
pub(crate) fn production_rust_code_with_doc_comments(text: &str) -> String {
    let code = rust_code_only(text);
    without_test_items(&code, rust_source_view(text, true))
}

/// The offset just past the `>` closing the angle-bracket group opened at `at`.
///
/// `->` is not a closing bracket, which is what keeps a return type inside a
/// generic argument list from ending it early.
fn generic_group_end(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if bytes.get(open) != Some(&b'<') {
        return None;
    }

    let mut depth = 0usize;
    for (relative, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' if bytes.get((open + relative).wrapping_sub(1)) != Some(&b'-') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + relative + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The offset just past the bracket closing the group opened at `open`.
///
/// `(`, `[` and `{` are counted together: they nest but never interleave in
/// Rust, and counting one kind alone would walk out of `foo(bar[(x)])`.
fn bracket_group_end(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if !matches!(bytes.get(open), Some(b'(' | b'[' | b'{')) {
        return None;
    }

    let mut depth = 0usize;
    for (relative, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + relative + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn call_open_paren(code: &str, name_end: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut at = skip_code_whitespace(code, name_end);

    // Generic Rust functions may be called through a turbofish.  Counting only
    // `name(` would make the policy guard bypassable with `name::<Type>(`.
    if bytes.get(at..)?.starts_with(b"::<") {
        at = skip_code_whitespace(code, generic_group_end(code, at + 2)?);
    }

    (bytes.get(at) == Some(&b'(')).then_some(at)
}

/// Calls to any of `names`, located only in executable Rust source.
///
/// A name must start on an identifier boundary and be followed (after optional
/// whitespace) by `(`.  Function declarations are excluded.  The returned
/// opening-parenthesis offset is valid in `source` as well as in the masked
/// view.
pub(crate) fn call_sites(source: &str, names: &[&str]) -> Vec<CallSite> {
    let code = rust_code_only(source);
    let mut calls = Vec::new();

    for name in names {
        for (name_at, _) in code.match_indices(name) {
            if code[..name_at]
                .chars()
                .next_back()
                .is_some_and(is_ident_char)
                || is_function_declaration(&code, name_at)
            {
                continue;
            }

            let Some(open_paren) = call_open_paren(&code, name_at + name.len()) else {
                continue;
            };

            calls.push(CallSite {
                line: code[..name_at]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
                open_paren,
            });
        }
    }

    calls.sort_unstable_by_key(|call| (call.open_paren, call.line));
    calls.dedup_by_key(|call| call.open_paren);
    calls
}

/// Calls to any of `names` in production Rust source.
///
/// [`call_sites`] applied to [`production_rust_code_only`], which is the view a
/// completeness census has to count on: a comment or a call-shaped string
/// literal contributes nothing, and a `#[cfg(test)]` item cannot hold a
/// production floor green after the last real call site is gone.  Blanking is
/// byte-aligned and idempotent, so the returned offsets and line numbers still
/// address the original `source` — which is where the arguments are decoded
/// from.
#[allow(dead_code)]
pub(crate) fn production_call_sites(source: &str, names: &[&str]) -> Vec<CallSite> {
    call_sites(&production_rust_code_only(source), names)
}

fn production_function_range(code: &str, name: &str) -> Option<std::ops::Range<usize>> {
    let mut matches = code.match_indices(name).filter_map(|(name_at, _)| {
        let name_end = name_at + name.len();
        if code[..name_at]
            .chars()
            .next_back()
            .is_some_and(is_ident_char)
            || code[name_end..].chars().next().is_some_and(is_ident_char)
            || !is_function_declaration(code, name_at)
        {
            return None;
        }

        // A generic declaration puts its parameter list between the name and
        // the arguments (`fn merge<'repo>(…)`). Stopping at the first character
        // that is not `(` would report *no such function* rather than a wrong
        // answer, which is the silent kind of miss: a guard asserting that some
        // call is absent from it would then pass without having read anything.
        let mut open_paren = skip_code_whitespace(code, name_end);
        if code.as_bytes().get(open_paren) == Some(&b'<') {
            open_paren = skip_code_whitespace(code, generic_group_end(code, open_paren)?);
        }
        if code.as_bytes().get(open_paren) != Some(&b'(') {
            return None;
        }

        let mut parentheses = 0usize;
        let mut brackets = 0usize;
        let mut body_open = None;
        for (relative, byte) in code.as_bytes()[open_paren..].iter().enumerate() {
            match byte {
                b'(' => parentheses += 1,
                b')' => parentheses = parentheses.saturating_sub(1),
                b'[' => brackets += 1,
                b']' => brackets = brackets.saturating_sub(1),
                b'{' if parentheses == 0 && brackets == 0 => {
                    body_open = Some(open_paren + relative);
                    break;
                }
                b';' if parentheses == 0 && brackets == 0 => return None,
                _ => {}
            }
        }
        let body_open = body_open?;

        let mut braces = 0usize;
        for (relative, byte) in code.as_bytes()[body_open..].iter().enumerate() {
            match byte {
                b'{' => braces += 1,
                b'}' => {
                    braces = braces.saturating_sub(1);
                    if braces == 0 {
                        return Some(name_at..body_open + relative + 1);
                    }
                }
                _ => {}
            }
        }
        None
    });

    let only = matches.next()?;
    assert!(
        matches.next().is_none(),
        "expected exactly one production function named `{name}`"
    );
    Some(only)
}

/// Calls to `names` inside the one production function named `function`.
///
/// Both the function boundary and calls are found in the byte-aligned
/// production code view. Comments, every Rust string-like literal, and complete
/// `#[cfg(test)]` items therefore cannot manufacture either the function or a
/// call. Returned offsets and line numbers still address the original source.
#[allow(dead_code)]
pub(crate) fn production_function_call_sites(
    source: &str,
    function: &str,
    names: &[&str],
) -> Vec<CallSite> {
    let code = production_rust_code_only(source);
    let Some(range) = production_function_range(&code, function) else {
        return Vec::new();
    };
    let line_offset = code[..range.start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let mut calls = call_sites(&code[range.clone()], names);
    for call in &mut calls {
        call.line += line_offset;
        call.open_paren += range.start;
    }
    calls
}

/// Whether `inner` is lexically inside the argument list of `outer`.
///
/// Parentheses are matched in the production code-only view, so a `)` in a
/// comment, literal, or test-only item cannot close the outer call early.
#[allow(dead_code)]
pub(crate) fn call_site_contains(source: &str, outer: CallSite, inner: CallSite) -> bool {
    if inner.open_paren <= outer.open_paren {
        return false;
    }

    let code = production_rust_code_only(source);
    let mut depth = 0usize;
    for (relative, byte) in code.as_bytes()[outer.open_paren..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return inner.open_paren < outer.open_paren + relative;
                }
            }
            _ => {}
        }
    }
    false
}

pub(crate) fn skip_whitespace_and_comments(source: &str, mut at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    loop {
        while let Some(ch) = source.get(at..)?.chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            at += ch.len_utf8();
        }

        if bytes.get(at..)?.starts_with(b"//") {
            at = bytes[at..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| at + relative);
            continue;
        }
        if bytes.get(at..)?.starts_with(b"/*") {
            let mut depth = 1usize;
            at += 2;
            while at < bytes.len() && depth > 0 {
                if bytes[at..].starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
            }
            continue;
        }
        return Some(at);
    }
}

fn raw_string_value(source: &str, at: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let prefix_len = if bytes.get(at) == Some(&b'r') {
        1
    } else if matches!(bytes.get(at), Some(b'b' | b'c')) && bytes.get(at + 1) == Some(&b'r') {
        2
    } else {
        return None;
    };
    let mut hashes = 0usize;
    while bytes.get(at + prefix_len + hashes) == Some(&b'#') {
        hashes += 1;
    }
    let quote = at + prefix_len + hashes;
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }

    let body = quote + 1;
    let mut end = body;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && end + 1 + hashes <= bytes.len()
            && bytes[end + 1..end + 1 + hashes]
                .iter()
                .all(|byte| *byte == b'#')
        {
            return Some(source[body..end].to_owned());
        }
        end += 1;
    }
    None
}

fn escaped_string_value(source: &str, at: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let quote = if bytes.get(at) == Some(&b'"') {
        at
    } else if matches!(bytes.get(at), Some(b'b' | b'c')) && bytes.get(at + 1) == Some(&b'"') {
        at + 1
    } else {
        return None;
    };

    let mut value = String::new();
    let mut cursor = quote + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => return Some(value),
            b'\\' => {
                cursor += 1;
                match *bytes.get(cursor)? {
                    b'\\' => value.push('\\'),
                    b'"' => value.push('"'),
                    b'\'' => value.push('\''),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    b'0' => value.push('\0'),
                    b'x' => {
                        let digits = source.get(cursor + 1..cursor + 3)?;
                        value.push(char::from(u8::from_str_radix(digits, 16).ok()?));
                        cursor += 2;
                    }
                    b'u' if bytes.get(cursor + 1) == Some(&b'{') => {
                        let close = bytes[cursor + 2..].iter().position(|byte| *byte == b'}')?;
                        let digits = source.get(cursor + 2..cursor + 2 + close)?;
                        let scalar = u32::from_str_radix(&digits.replace('_', ""), 16).ok()?;
                        value.push(char::from_u32(scalar)?);
                        cursor += close + 2;
                    }
                    b'\n' => {
                        cursor += 1;
                        while let Some(ch) = source.get(cursor..)?.chars().next() {
                            if !ch.is_whitespace() {
                                break;
                            }
                            cursor += ch.len_utf8();
                        }
                        continue;
                    }
                    b'\r' if bytes.get(cursor + 1) == Some(&b'\n') => {
                        cursor += 2;
                        while let Some(ch) = source.get(cursor..)?.chars().next() {
                            if !ch.is_whitespace() {
                                break;
                            }
                            cursor += ch.len_utf8();
                        }
                        continue;
                    }
                    _ => return None,
                }
                cursor += 1;
            }
            _ => {
                let ch = source.get(cursor..)?.chars().next()?;
                value.push(ch);
                cursor += ch.len_utf8();
            }
        }
    }
    None
}

/// Where argument `index` of `call` starts, in the code-only view `code`.
///
/// Only the commas of the call's own argument list separate arguments: nested
/// `(…)`, `[…]`, `{…}` groups and turbofish generics are skipped whole, and
/// commas inside comments and literals are already blanked.  `None` means the
/// list ended first — a call with fewer arguments than asked for.
fn argument_start(code: &str, call: CallSite, index: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut at = call.open_paren + 1;
    let mut argument = 0usize;

    while argument < index {
        match *bytes.get(at)? {
            b'(' | b'[' | b'{' => at = bracket_group_end(code, at)?,
            b')' => return None,
            b',' => {
                argument += 1;
                at += 1;
            }
            b':' if bytes.get(at..).is_some_and(|rest| rest.starts_with(b"::<")) => {
                at = generic_group_end(code, at + 2)?;
            }
            _ => at += 1,
        }
    }

    Some(at)
}

/// The decoded string-like argument at position `index` of `call`.
///
/// The argument boundary is counted in the code-only view and the value is then
/// decoded from the original source at that offset, so the answer is the
/// argument the compiler sees rather than the nearest quoted text.  A parameter
/// slot holding anything but a literal — a variable, a call, an expression —
/// reads as `None`: a source guard abstains rather than inventing provenance,
/// and the census that calls this notices the loss through its own count floor.
///
/// One limit worth naming: a bare `<…>` outside a turbofish (a generic spelled
/// in a closure parameter's type annotation) is not tracked, so an argument
/// after one is unreadable rather than misread.
#[allow(dead_code)]
pub(crate) fn nth_string_argument(source: &str, call: CallSite, index: usize) -> Option<String> {
    let start = if index == 0 {
        call.open_paren + 1
    } else {
        argument_start(&rust_code_only(source), call, index)?
    };

    let mut at = skip_whitespace_and_comments(source, start)?;
    if source.as_bytes().get(at) == Some(&b'&') {
        at = skip_whitespace_and_comments(source, at + 1)?;
    }
    raw_string_value(source, at).or_else(|| escaped_string_value(source, at))
}

/// The decoded first string-like argument of `call`, read from the original
/// source at the boundary established in the code-only view.
pub(crate) fn first_string_argument(source: &str, call: CallSite) -> Option<String> {
    nth_string_argument(source, call, 0)
}

fn string_const_value(source: &str, name: &str) -> Option<String> {
    let code = rust_code_only(source);
    let mut values = code.match_indices(name).filter_map(|(name_at, _)| {
        let name_end = name_at + name.len();
        if code[..name_at]
            .chars()
            .next_back()
            .is_some_and(is_ident_char)
            || code[name_end..].chars().next().is_some_and(is_ident_char)
        {
            return None;
        }

        let statement_start = code[..name_at]
            .rfind([';', '{', '}', '\n'])
            .map_or(0, |boundary| boundary + 1);
        if code[statement_start..name_at]
            .split_whitespace()
            .next_back()
            != Some("const")
        {
            return None;
        }

        let statement_end = name_end + code[name_end..].find(';')?;
        let equals = name_end + code[name_end..statement_end].find('=')?;
        let value_at = skip_whitespace_and_comments(source, equals + 1)?;
        raw_string_value(source, value_at).or_else(|| escaped_string_value(source, value_at))
    });

    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// The decoded first string-like argument of `call`, accepting a same-file
/// string constant as one explicit level of indirection.
///
/// The call and identifier path are established in the code-only view. The
/// constant declaration must be unique, and its value is decoded from the
/// original source at the matching byte offset. Arbitrary expressions are not
/// evaluated: a source guard should abstain rather than invent provenance.
#[allow(dead_code)]
pub(crate) fn first_string_or_const_argument(source: &str, call: CallSite) -> Option<String> {
    if let Some(value) = first_string_argument(source, call) {
        return Some(value);
    }

    let code = rust_code_only(source);
    let mut at = skip_code_whitespace(&code, call.open_paren + 1);
    if code.as_bytes().get(at) == Some(&b'&') {
        at = skip_code_whitespace(&code, at + 1);
    }

    let start = at;
    while code
        .as_bytes()
        .get(at)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'))
    {
        at += 1;
    }
    let path = code.get(start..at)?;
    if path.is_empty()
        || path.split("::").any(str::is_empty)
        || !matches!(
            code.as_bytes().get(skip_code_whitespace(&code, at)),
            Some(b',' | b')')
        )
    {
        return None;
    }

    string_const_value(source, path.rsplit("::").next()?)
}

/// String literal values assigned to `field` in production Rust items.
///
/// The field identifier and `:` boundary are established in the byte-aligned
/// code-only view.  The value is then decoded from the original source at the
/// same offset, preserving both multiline fields and useful line diagnostics.
/// Comments, literal-shaped decoys and complete `#[cfg(test)]` items do not
/// contribute values.
#[allow(dead_code)]
pub(crate) fn string_field_literals(source: &str, field: &str) -> Vec<StringField> {
    let code = production_rust_code_only(source);
    let mut fields = Vec::new();

    for (field_at, _) in code.match_indices(field) {
        let field_end = field_at + field.len();
        if code[..field_at]
            .chars()
            .next_back()
            .is_some_and(is_ident_char)
            || code[field_end..].chars().next().is_some_and(is_ident_char)
        {
            continue;
        }

        let colon = skip_code_whitespace(&code, field_end);
        if code.as_bytes().get(colon) != Some(&b':') {
            continue;
        }

        let line = code[..field_at]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;

        let Some(mut value_at) = skip_whitespace_and_comments(source, colon + 1) else {
            continue;
        };
        if source.as_bytes().get(value_at) == Some(&b'&') {
            let Some(after_borrow) = skip_whitespace_and_comments(source, value_at + 1) else {
                continue;
            };
            value_at = after_borrow;
        }
        let Some(value) =
            raw_string_value(source, value_at).or_else(|| escaped_string_value(source, value_at))
        else {
            continue;
        };

        fields.push(StringField { line, value });
    }

    fields
}

#[allow(dead_code)]
pub(crate) fn source_line(source: &str, line: usize) -> &str {
    source.lines().nth(line.saturating_sub(1)).unwrap_or("")
}
