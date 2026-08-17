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

/// The 1-based, inclusive line ranges occupied by `#[cfg(test)]` items.
///
/// Braces are counted on the code-only view, so comments and literals cannot
/// close a test module early.  Each range ends with its item instead of turning
/// the first inline test module into a false "rest of file is tests" marker.
fn test_item_ranges(code: &str) -> Vec<std::ops::RangeInclusive<usize>> {
    let lines: Vec<&str> = code.lines().collect();
    let mut ranges = Vec::new();
    let mut line = 0;

    while line < lines.len() {
        if lines[line].trim() != "#[cfg(test)]" {
            line += 1;
            continue;
        }

        let mut depth = 0usize;
        let mut opened = false;
        let mut end = lines.len().saturating_sub(1);
        for (candidate, text) in lines.iter().enumerate().skip(line + 1) {
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

fn call_open_paren(code: &str, name_end: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut at = skip_code_whitespace(code, name_end);

    // Generic Rust functions may be called through a turbofish.  Counting only
    // `name(` would make the policy guard bypassable with `name::<Type>(`.
    if bytes.get(at..)?.starts_with(b"::<") {
        at += 3;
        let mut depth = 1usize;
        while at < bytes.len() && depth > 0 {
            match bytes[at] {
                b'<' => depth += 1,
                b'>' if bytes.get(at.wrapping_sub(1)) != Some(&b'-') => depth -= 1,
                _ => {}
            }
            at += 1;
        }
        if depth != 0 {
            return None;
        }
        at = skip_code_whitespace(code, at);
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

/// The decoded first string-like argument of `call`, read from the original
/// source at the boundary established in the code-only view.
pub(crate) fn first_string_argument(source: &str, call: CallSite) -> Option<String> {
    let mut at = skip_whitespace_and_comments(source, call.open_paren + 1)?;
    if source.as_bytes().get(at) == Some(&b'&') {
        at = skip_whitespace_and_comments(source, at + 1)?;
    }
    raw_string_value(source, at).or_else(|| escaped_string_value(source, at))
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
