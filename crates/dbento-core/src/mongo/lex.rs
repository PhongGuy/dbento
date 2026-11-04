//! The mongosh-subset lexer: JavaScript-style identifiers, strings, numbers, regular
//! expression literals and single-byte punctuation, with `//` and `/* */` comments skipped.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Ident,
    Str,
    Num,
    Regex,
    Punct(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tok {
    pub(crate) kind: Kind,
    pub(crate) range: Range<usize>,
    pub(crate) line_break_before: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LexError {
    pub(crate) message: &'static str,
    pub(crate) at: usize,
}

/// Tokens in order, comments and whitespace skipped; on a bad token, those before it and the error.
pub(crate) fn lex(src: &str) -> (Vec<Tok>, Option<LexError>) {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut line_break = false;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        let bad = |message| Some(LexError { message, at: start });
        // Whitespace and comments only note whether they crossed a line.
        let c = src[i..].chars().next().unwrap_or_default();
        if c.is_whitespace() {
            line_break |= c == '\n';
            i += c.len_utf8();
            continue;
        }
        if src[i..].starts_with("//") {
            i = src[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if src[i..].starts_with("/*") {
            let Some(end) = src[i + 2..].find("*/") else {
                return (toks, bad("unterminated comment"));
            };
            line_break |= src[i + 2..i + 2 + end].contains('\n');
            i += 2 + end + 2;
            continue;
        }
        let kind = match b {
            b'\'' | b'"' => {
                let Some(end) = string_end(bytes, i) else {
                    return (toks, bad("unterminated string"));
                };
                i = end;
                if decode(&src[start + 1..end - 1]).is_none() {
                    return (toks, bad("bad escape in string"));
                }
                Kind::Str
            }
            b'/' => {
                let Some(end) = regex_end(bytes, i) else {
                    return (toks, bad("unterminated regular expression"));
                };
                i = end;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                Kind::Regex
            }
            b'0'..=b'9' => {
                i = number_end(bytes, i);
                Kind::Num
            }
            _ if is_ident_char(c) => {
                i += src[i..]
                    .find(|c: char| !(is_ident_char(c) || c.is_ascii_digit()))
                    .unwrap_or(bytes.len() - i);
                Kind::Ident
            }
            _ => {
                i += 1;
                Kind::Punct(b)
            }
        };
        toks.push(Tok {
            kind,
            range: start..i,
            line_break_before: line_break && !toks.is_empty(),
        });
        line_break = false;
    }
    (toks, None)
}

/// A lexed `Str`, quotes included, with its escapes decoded.
pub(crate) fn string_value(lit: &str) -> String {
    decode(&lit[1..lit.len() - 1]).unwrap_or_default()
}

/// A lexed `Regex` split into its pattern as written and its flags.
pub(crate) fn regex_parts(lit: &str) -> (&str, &str) {
    let close = lit.rfind('/').unwrap_or(0);
    (&lit[1..close], &lit[close + 1..])
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || !c.is_ascii()
}

// The byte after the closing quote; a backslash hides the next byte, so an escaped
// line break continues the string while a bare `\n` or `\r` ends it unterminated.
fn string_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += if bytes[i + 1..].starts_with(b"\r\n") {
                    2
                } else {
                    1
                }
            }
            b'\n' | b'\r' => return None,
            b if b == bytes[open] => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

// The byte after the closing `/`, which must lie outside a `[...]` class and on the same line.
fn regex_end(bytes: &[u8], open: usize) -> Option<usize> {
    let (mut i, mut class) = (open + 1, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if bytes.get(i + 1).is_some_and(|&b| b != b'\n') => i += 1,
            b'\n' => return None,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

// Digits, a fraction only when a digit follows the `.`, and an exponent only when it has digits.
fn number_end(bytes: &[u8], start: usize) -> usize {
    let digits = |mut i: usize| {
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let mut i = digits(start);
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i = digits(i + 1);
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(i + 1), Some(b'+' | b'-')));
        if bytes.get(i + 1 + sign).is_some_and(u8::is_ascii_digit) {
            i = digits(i + 1 + sign);
        }
    }
    i
}

// JavaScript string escapes; `None` for a malformed `\x` or `\u`, or a lone surrogate.
fn decode(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            'x' => out.push(char::from(u8::try_from(hex(&mut chars, 2)?).ok()?)),
            'u' => out.push(unicode_escape(&mut chars)?),
            // A backslash before a line break continues the string.
            '\n' | '\u{2028}' | '\u{2029}' => {}
            '\r' => {
                if chars.clone().next() == Some('\n') {
                    chars.next();
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

fn hex(chars: &mut std::str::Chars<'_>, n: usize) -> Option<u32> {
    let digits: String = chars.by_ref().take(n).collect();
    let all_hex = digits.len() == n && digits.bytes().all(|b| b.is_ascii_hexdigit());
    all_hex
        .then(|| u32::from_str_radix(&digits, 16).ok())
        .flatten()
}

// `\u{H...}` or `\uHHHH`, where a high surrogate must pair with a following `\uHHHH` low one.
fn unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<char> {
    if chars.clone().next() == Some('{') {
        let rest = &chars.as_str()[1..];
        let (digits, after) = rest.split_once('}')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *chars = after.chars();
        return char::from_u32(u32::from_str_radix(digits, 16).ok()?);
    }
    let unit = hex(chars, 4)?;
    if (0xD800..0xDC00).contains(&unit) {
        let (Some('\\'), Some('u')) = (chars.next(), chars.next()) else {
            return None;
        };
        let low = hex(chars, 4)?;
        if !(0xDC00..0xE000).contains(&low) {
            return None;
        }
        return char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00));
    }
    char::from_u32(unit)
}

#[cfg(test)]
mod tests;
