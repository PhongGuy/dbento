//! The SurrealQL lexer. Its comment, quote, regex and script rules match the server's observed
//! behaviour.

use std::ops::Range;

/// The letters that make a following quote a prefixed string.
const STRING_PREFIXES: &[u8] = b"rdubsf";

/// The infix operators and clause words: after a value, each of these wants another one.
const INFIX_WORDS: [&str; 44] = [
    "allinside",
    "and",
    "anyinside",
    "assert",
    "at",
    "by",
    "comment",
    "contains",
    "containsall",
    "containsany",
    "containsnone",
    "containsnot",
    "content",
    "default",
    "else",
    "fetch",
    "from",
    "if",
    "in",
    "inside",
    "intersects",
    "is",
    "limit",
    "merge",
    "noneinside",
    "not",
    "notinside",
    "omit",
    "or",
    "order",
    "outside",
    "patch",
    "replace",
    "return",
    "select",
    "set",
    "split",
    "start",
    "then",
    "throw",
    "timeout",
    "value",
    "when",
    "where",
];

/// Where a value belongs, these open an expression rather than name a field. They are
/// reserved at an expression start (fence probe F6), so none of them can be a bare name.
const OPENING_WORDS: [&str; 4] = ["if", "return", "select", "throw"];

/// Every char that ends a line comment; VT and FF do not.
const LINE_ENDS: [char; 5] = ['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A name, with any quoting removed; `quoted` names are never keywords.
    Ident {
        name: String,
        quoted: bool,
    },
    /// A string; `prefix` is the letter of `r'..'`, `d'..'` and the like.
    Str {
        prefix: Option<u8>,
    },
    Num,
    Param,
    Regex,
    /// The `{ .. }` body of a `function(..)`.
    Script,
    /// `::`, whatever the spacing around it.
    PathSep,
    /// `..`, which unlike `.` does not make the next word a field.
    DotDot,
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tok {
    pub(crate) kind: Kind,
    pub(crate) range: Range<usize>,
}

impl Tok {
    /// The name of an unquoted identifier, which may be a keyword.
    pub(crate) fn word(&self) -> Option<&str> {
        match &self.kind {
            Kind::Ident {
                name,
                quoted: false,
            } => Some(name),
            Kind::Ident { .. }
            | Kind::Str { .. }
            | Kind::Num
            | Kind::Param
            | Kind::Regex
            | Kind::Script
            | Kind::PathSep
            | Kind::DotDot
            | Kind::Punct(_) => None,
        }
    }

    /// The name of any identifier, quoted or not.
    pub(crate) fn name(&self) -> Option<&str> {
        match &self.kind {
            Kind::Ident { name, .. } => Some(name),
            Kind::Str { .. }
            | Kind::Num
            | Kind::Param
            | Kind::Regex
            | Kind::Script
            | Kind::PathSep
            | Kind::DotDot
            | Kind::Punct(_) => None,
        }
    }

    /// Whether this is the unquoted keyword `word`, in any case.
    pub(crate) fn is_word(&self, word: &str) -> bool {
        self.word().is_some_and(|w| w.eq_ignore_ascii_case(word))
    }

    /// Whether this is the unquoted keyword of any of `words`, in any case.
    pub(crate) fn is_any_word(&self, words: &[&str]) -> bool {
        words.iter().any(|word| self.is_word(word))
    }

    pub(crate) fn is_punct(&self, c: char) -> bool {
        self.kind == Kind::Punct(c)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LexError {
    pub(crate) message: &'static str,
    pub(crate) at: usize,
}

/// How far a `function(..)` head has got: only the `{` straight after its argument list
/// opens a script body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    Idle,
    Named,
    Args(usize),
    Closed,
}

impl Head {
    // `fn::function` and `$a.function` are plain names, so a block after them stays code.
    fn after(self, tok: &Tok, prev: Option<&Tok>) -> Self {
        match self {
            Self::Named if tok.is_punct('(') => Self::Args(1),
            Self::Args(1) if tok.is_punct(')') => Self::Closed,
            Self::Args(depth) if tok.is_punct(')') => Self::Args(depth - 1),
            Self::Args(depth) if tok.is_punct('(') => Self::Args(depth + 1),
            Self::Args(_) => self,
            _ if tok.is_word("function") && !on_path(prev) => Self::Named,
            Self::Idle | Self::Named | Self::Closed => Self::Idle,
        }
    }
}

/// Whether a value could start here, the primary reading of a `/`. It is a guess after a
/// word, where the server's choice is grammatical, so `alternate` gives the other reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    ExpectValue,
    AfterValue,
}

impl Position {
    fn after(self, tok: &Tok, prev: Option<&Tok>) -> Self {
        match tok.kind {
            Kind::Ident { quoted: false, .. } => self.after_word(tok, prev),
            Kind::PathSep | Kind::DotDot => Self::ExpectValue,
            // A `*` where a value belongs is the wildcard of `SELECT *` or `a.*`, unless
            // it completes a `**`.
            Kind::Punct('*')
                if self == Self::ExpectValue && !prev.is_some_and(|p| p.is_punct('*')) =>
            {
                Self::AfterValue
            }
            Kind::Punct(')' | ']' | '}') => Self::AfterValue,
            Kind::Punct(_) => Self::ExpectValue,
            Kind::Ident { .. }
            | Kind::Str { .. }
            | Kind::Num
            | Kind::Param
            | Kind::Regex
            | Kind::Script => Self::AfterValue,
        }
    }

    // A word is a name unless its place makes it a keyword. The second word of `IS NOT`,
    // `NOT IN`, `SELECT VALUE`, `ORDER BY` and `LIMIT BY` still has the operand to come, so
    // it keeps expecting one.
    fn after_word(self, tok: &Tok, prev: Option<&Tok>) -> Self {
        let follows = |word| prev.is_some_and(|p: &Tok| p.is_word(word));
        let wants_value = match self {
            _ if on_path(prev) => false,
            Self::AfterValue => tok.is_any_word(&INFIX_WORDS),
            Self::ExpectValue => {
                tok.is_any_word(&OPENING_WORDS)
                    || (tok.is_word("not") && follows("is"))
                    || (tok.is_word("in") && follows("not"))
                    || (tok.is_word("value") && follows("select"))
                    || (tok.is_word("by") && (follows("order") || follows("limit")))
            }
        };
        if wants_value {
            Self::ExpectValue
        } else {
            Self::AfterValue
        }
    }
}

// Whether the next word is a path segment or field, which is a name whatever it spells.
pub(crate) fn on_path(prev: Option<&Tok>) -> bool {
    prev.is_some_and(|p| p.kind == Kind::PathSep || p.is_punct('.'))
}

/// Tokens in order, comments and whitespace skipped; on a bad token, those before it and the error.
pub(crate) fn lex(src: &str) -> (Vec<Tok>, Option<LexError>) {
    scan(src, false)
}

/// The tokens with every doubtful `/` read the other way, or `None` if that changes nothing.
/// A `/` is doubtful directly after a word that is not a path segment or field. Whoever
/// judges a text judges both readings, so a wrong guess cannot hide a statement.
pub(crate) fn alternate(text: &str) -> Option<Vec<Tok>> {
    let (primary, _) = lex(text);
    let (second, _) = scan(text, true);
    (second != primary).then_some(second)
}

fn scan(src: &str, flip_doubtful: bool) -> (Vec<Tok>, Option<LexError>) {
    let bytes = src.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut head = Head::Idle;
    let mut position = Position::ExpectValue;
    let mut i = 0;
    while let Some(c) = src[i..].chars().next() {
        let start = i;
        let rest = &src[i..];
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        // Comments come before the regex check, so `//` is never an empty regex.
        if rest.starts_with("--") || rest.starts_with("//") || c == '#' {
            i = line_end(src, i);
            continue;
        }
        if rest.starts_with("/*") {
            let Some(end) = block_comment_end(bytes, i) else {
                let message = "unterminated comment";
                return (toks, Some(LexError { message, at: start }));
            };
            i = end;
            continue;
        }
        let flip = flip_doubtful && after_free_word(&toks);
        match token(src, start, c, position, head, flip) {
            Ok((kind, end)) => {
                i = end;
                let tok = Tok {
                    kind,
                    range: start..end,
                };
                head = head.after(&tok, toks.last());
                position = position.after(&tok, toks.last());
                toks.push(tok);
            }
            Err(message) => return (toks, Some(LexError { message, at: start })),
        }
    }
    (toks, None)
}

/// `name` as a backticked SurrealQL identifier, which takes every name.
pub(crate) fn quote_ident(name: &str) -> String {
    quote(name, '`')
}

/// `text` as a single-quoted SurrealQL string; backslash is the only escape.
pub(crate) fn quote_string(text: &str) -> String {
    quote(text, '\'')
}

fn quote(text: &str, mark: char) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(mark);
    for c in text.chars() {
        if c == mark || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push(mark);
    out
}

// The token starting with `c` at `start`, and the byte after it.
fn token(
    src: &str,
    start: usize,
    c: char,
    position: Position,
    head: Head,
    flip: bool,
) -> Result<(Kind, usize), &'static str> {
    let bytes = src.as_bytes();
    let rest = &src[start..];
    let string = |prefix, open| {
        let end = quoted_end(bytes, open).ok_or("unterminated string")?;
        Ok((Kind::Str { prefix }, end))
    };
    match c {
        '{' if head == Head::Closed => {
            let end = script_end(src, start).ok_or("unterminated script")?;
            Ok((Kind::Script, end))
        }
        '\'' | '"' => string(None, start),
        '`' => {
            let end = quoted_end(bytes, start).ok_or("unterminated identifier")?;
            let name = unescape_ident(&src[start + 1..end - 1]);
            Ok((Kind::Ident { name, quoted: true }, end))
        }
        // The server decodes escapes here, but none writes a `⟩`, so the first one closes the
        // name, which keeps any escape as written.
        '⟨' => {
            let body = start + c.len_utf8();
            let close = src[body..].find('⟩').ok_or("unterminated identifier")?;
            let name = src[body..body + close].to_owned();
            let end = body + close + '⟩'.len_utf8();
            Ok((Kind::Ident { name, quoted: true }, end))
        }
        '/' => slash(bytes, start, position, flip),
        '$' if bytes.get(start + 1).is_some_and(|&b| is_word_byte(b)) => {
            Ok((Kind::Param, word_end(bytes, start + 1)))
        }
        '0'..='9' => Ok((Kind::Num, number_end(bytes, start))),
        '.' if rest.starts_with("..") => Ok((Kind::DotDot, start + 2)),
        ':' if rest.starts_with("::") => Ok((Kind::PathSep, start + 2)),
        _ if c.is_ascii_alphabetic() || c == '_' => {
            let end = word_end(bytes, start);
            let quote_next = matches!(bytes.get(end), Some(b'\'' | b'"'));
            if end == start + 1 && quote_next && STRING_PREFIXES.contains(&bytes[start]) {
                return string(Some(bytes[start]), end);
            }
            let name = src[start..end].to_owned();
            Ok((
                Kind::Ident {
                    name,
                    quoted: false,
                },
                end,
            ))
        }
        _ => Ok((Kind::Punct(c), start + c.len_utf8())),
    }
}

// Reads the `/` at `start`. A flip to a regex needs its closing `/`: without one this is
// plain arithmetic and stays a divide, so only an unflipped regex can be unterminated.
fn slash(
    bytes: &[u8],
    start: usize,
    position: Position,
    flip: bool,
) -> Result<(Kind, usize), &'static str> {
    let divide = (Kind::Punct('/'), start + 1);
    if (position == Position::ExpectValue) == flip {
        return Ok(divide);
    }
    match quoted_end(bytes, start) {
        Some(end) => Ok((Kind::Regex, end)),
        None if flip => Ok(divide),
        None => Err("unterminated regex"),
    }
}

// Whether the last token is a word the grammar may yet make a keyword.
fn after_free_word(toks: &[Tok]) -> bool {
    toks.split_last()
        .is_some_and(|(last, rest)| last.word().is_some() && !on_path(rest.last()))
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn word_end(bytes: &[u8], start: usize) -> usize {
    let tail = &bytes[start..];
    start
        + tail
            .iter()
            .position(|&b| !is_word_byte(b))
            .unwrap_or(tail.len())
}

// Digits with any unit or type suffix; a `.` joins only when a digit follows, so that
// `1..3` is a range and `seed:1.delete` a field.
fn number_end(bytes: &[u8], start: usize) -> usize {
    let end = word_end(bytes, start);
    let fraction =
        bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit);
    if fraction {
        word_end(bytes, end + 1)
    } else {
        end
    }
}

// The byte after the mark that closes the one at `open`; a backslash hides the next byte.
// Every mark is ASCII, so the walk cannot stop inside a multi-byte char.
fn quoted_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while let Some(&b) = bytes.get(i) {
        if b == bytes[open] {
            return Some(i + 1);
        }
        i += if b == b'\\' { 2 } else { 1 };
    }
    None
}

fn line_end(src: &str, start: usize) -> usize {
    src[start..]
        .find(LINE_ENDS)
        .map_or(src.len(), |n| start + n)
}

// Block comments do not nest: the first `*/` closes this one.
fn block_comment_end(bytes: &[u8], open: usize) -> Option<usize> {
    let close = bytes[open + 2..].windows(2).position(|w| w == b"*/")?;
    Some(open + 2 + close + 2)
}

// Balances the braces of a script body at `open`. Strings, templates and `//` and `/* */`
// comments hide braces; `--` and `#` do not, since a JavaScript `a--;` is no comment.
fn script_end(src: &str, open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = open;
    let mut depth = 0usize;
    while let Some(&b) = bytes.get(i) {
        let rest = &bytes[i..];
        i = match b {
            b'\'' | b'"' | b'`' => quoted_end(bytes, i)?,
            b'/' if rest.starts_with(b"//") => line_end(src, i),
            b'/' if rest.starts_with(b"/*") => block_comment_end(bytes, i)?,
            b'{' => {
                depth += 1;
                i + 1
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
                i + 1
            }
            _ => i + 1,
        };
    }
    None
}

// Decodes only its own mark and the backslash, which are all a name needs for matching.
// The server also unescapes `\n` and the like and rejects `\q`; those stay as written here.
fn unescape_ident(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&next @ ('`' | '\\')) if c == '\\' => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
