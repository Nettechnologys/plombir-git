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
pub(crate) fn rust_code_only(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut masked = bytes.to_vec();
    let mut at = 0;

    while at < bytes.len() {
        if bytes[at..].starts_with(b"//") {
            let end = bytes[at..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| at + relative);
            blank_range(&mut masked, at, end);
            at = end;
            continue;
        }

        if bytes[at..].starts_with(b"/*") {
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
            blank_range(&mut masked, at, end);
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

fn skip_whitespace_and_comments(source: &str, mut at: usize) -> Option<usize> {
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

#[allow(dead_code)]
pub(crate) fn source_line(source: &str, line: usize) -> &str {
    source.lines().nth(line.saturating_sub(1)).unwrap_or("")
}
