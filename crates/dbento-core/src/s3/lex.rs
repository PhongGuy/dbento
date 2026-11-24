//! S3 command scripts, split into statements of shell-like words.

use std::ops::Range;

/// One command: its words, unquoted, and its range from the first word's start to the last's end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct S3Statement {
    pub(crate) words: Vec<String>,
    pub(crate) range: Range<usize>,
}

/// A statement that does not lex: what is wrong, its byte offset from `range.start`, and the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    pub(crate) message: String,
    pub(crate) offset: usize,
    pub(crate) range: Range<usize>,
}

const TOUCHING: &str = "a quoted word must be separated from the next word by a space";
const UNTERMINATED: &str = "unterminated quote";

/// One result per statement with a word, in order.
pub(crate) fn parse_script(text: &str) -> Vec<Result<S3Statement, SyntaxError>> {
    let mut lexer = Lexer { text, pos: 0 };
    let mut out = Vec::new();
    while let Some(statement) = lexer.statement() {
        let ends_script = statement.ends_script;
        if let Some(result) = statement.into_result() {
            out.push(result);
        }
        if ends_script {
            break;
        }
    }
    out
}

/// The range of every `parse_script` result, parsed or not.
pub(crate) fn statement_ranges(text: &str) -> Vec<Range<usize>> {
    parse_script(text)
        .into_iter()
        .map(|r| match r {
            Ok(s) => s.range,
            Err(e) => e.range,
        })
        .collect()
}

/// `'text'` with each `'` doubled.
pub(crate) fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Whitespace inside a statement; `\n` is not, since it ends one.
fn is_blank(c: char) -> bool {
    c != '\n' && c.is_whitespace()
}

/// What one statement lexed to, before it becomes a result.
struct Lexed {
    words: Vec<String>,
    span: Option<Range<usize>>,
    /// The first fault: its message and absolute byte position.
    fault: Option<(&'static str, usize)>,
    ends_script: bool,
}

impl Lexed {
    fn into_result(self) -> Option<Result<S3Statement, SyntaxError>> {
        let range = self.span?;
        Some(match self.fault {
            None => Ok(S3Statement {
                words: self.words,
                range,
            }),
            Some((message, at)) => Err(SyntaxError {
                message: message.to_owned(),
                offset: at - range.start,
                range,
            }),
        })
    }
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text
            .get(self.pos..)
            .and_then(|rest| rest.chars().next())
    }

    fn bump(&mut self, c: char) {
        self.pos += c.len_utf8();
    }

    /// Lex words up to a `\n` or `;` outside quotes; `None` once the text is used up.
    fn statement(&mut self) -> Option<Lexed> {
        if self.pos >= self.text.len() {
            return None;
        }
        let mut lexed = Lexed {
            words: Vec::new(),
            span: None,
            fault: None,
            ends_script: false,
        };
        loop {
            while let Some(c) = self.peek().filter(|&c| is_blank(c)) {
                self.bump(c);
            }
            let Some(c) = self.peek() else {
                return Some(lexed);
            };
            match c {
                '\n' | ';' => {
                    self.bump(c);
                    return Some(lexed);
                }
                // A comment runs to the line's end but leaves the `\n` to end the statement.
                '#' => {
                    while let Some(c) = self.peek().filter(|&c| c != '\n') {
                        self.bump(c);
                    }
                }
                _ => {
                    let start = self.pos;
                    let word = if c == '\'' {
                        match self.quoted() {
                            Some(word) => word,
                            // An unterminated quote swallows the rest of the text as this statement.
                            None => {
                                let first = lexed.span.map_or(start, |s| s.start);
                                lexed.span = Some(first..self.text.len());
                                lexed.fault.get_or_insert((UNTERMINATED, start));
                                lexed.ends_script = true;
                                return Some(lexed);
                            }
                        }
                    } else {
                        self.bare()
                    };
                    let first = lexed.span.as_ref().map_or(start, |s| s.start);
                    lexed.span = Some(first..self.pos);
                    lexed.words.push(word);
                    // A word must end at a blank, a statement end or the text's end; a quote
                    // on either side of the boundary would glue two words together.
                    if let Some(next) = self.peek()
                        && !is_blank(next)
                        && next != '\n'
                        && next != ';'
                        && (c == '\'' || next == '\'')
                    {
                        lexed.fault.get_or_insert((TOUCHING, self.pos));
                    }
                }
            }
        }
    }

    /// A bare word: everything up to a blank, `\n`, `;` or `'`.
    fn bare(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self
            .peek()
            .filter(|&c| !c.is_whitespace() && c != ';' && c != '\'')
        {
            self.bump(c);
        }
        self.text[start..self.pos].to_owned()
    }

    /// A `'`-quoted word with `''` as one quote, or `None` when the closing quote is missing.
    fn quoted(&mut self) -> Option<String> {
        self.bump('\'');
        let mut word = String::new();
        loop {
            let c = self.peek()?;
            self.bump(c);
            if c != '\'' {
                word.push(c);
            } else if self.peek() == Some('\'') {
                self.bump('\'');
                word.push('\'');
            } else {
                return Some(word);
            }
        }
    }
}

#[cfg(test)]
mod tests;
