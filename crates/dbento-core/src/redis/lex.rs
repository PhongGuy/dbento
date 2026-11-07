//! Redis command lines, split into words the way redis-cli splits them.

use std::ops::Range;

/// One command line: its words as bytes, and the line's trimmed range in the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisStatement {
    pub(crate) args: Vec<Vec<u8>>,
    pub(crate) range: Range<usize>,
}

/// A line that does not split: what is wrong, its byte offset from `range.start`, and the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    pub(crate) message: String,
    pub(crate) offset: usize,
    pub(crate) range: Range<usize>,
}

/// One result per non-blank line that is not a `#` comment, in order.
pub(crate) fn parse_script(text: &str) -> Vec<Result<RedisStatement, SyntaxError>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut line_start = 0;
    // Each `\n` ends a line; the final piece is a line too, even without one.
    for line in bytes.split(|&b| b == b'\n') {
        let start = line_start + line.iter().take_while(|&&b| is_separator(b)).count();
        let end =
            line_start + line.len() - line.iter().rev().take_while(|&&b| is_separator(b)).count();
        line_start += line.len() + 1;
        if start >= end || bytes[start] == b'#' {
            continue;
        }
        let range = start..end;
        out.push(match split_words(&bytes[range.clone()]) {
            Ok(args) => Ok(RedisStatement { args, range }),
            Err((message, offset)) => Err(SyntaxError {
                message: message.to_owned(),
                offset,
                range,
            }),
        });
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

/// Space, tab, CR, VT and FF: redis-cli's separators widened by VT and FF.
fn is_separator(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c)
}

/// Redis 7.2's `sdssplitargs` over one trimmed line. A quote may open mid-word, and a
/// closing quote must be followed by a separator or the line's end. An error is its
/// message and the offending byte's offset in `line`.
fn split_words(line: &[u8]) -> Result<Vec<Vec<u8>>, (&'static str, usize)> {
    let n = line.len();
    let mut words = Vec::new();
    let mut i = 0;
    loop {
        while i < n && is_separator(line[i]) {
            i += 1;
        }
        if i == n {
            return Ok(words);
        }
        let mut word = Vec::new();
        while i < n && !is_separator(line[i]) {
            let quote = line[i];
            if quote != b'"' && quote != b'\'' {
                word.push(quote);
                i += 1;
                continue;
            }
            let open = i;
            i += 1;
            // Inside the quote until its closing byte; the escapes differ by quote kind.
            loop {
                let Some(&c) = line.get(i) else {
                    return Err(("unterminated quote", open));
                };
                if c == quote {
                    i += 1;
                    break;
                }
                if c == b'\\' && quote == b'"' && i + 1 < n {
                    let escaped = line[i + 1];
                    if escaped == b'x'
                        && let (Some(high), Some(low)) =
                            (hex_digit(line, i + 2), hex_digit(line, i + 3))
                    {
                        word.push(high << 4 | low);
                        i += 4;
                    } else {
                        word.push(match escaped {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            b'b' => 0x08,
                            b'a' => 0x07,
                            other => other,
                        });
                        i += 2;
                    }
                } else if c == b'\\' && quote == b'\'' && line.get(i + 1) == Some(&b'\'') {
                    word.push(b'\'');
                    i += 2;
                } else {
                    word.push(c);
                    i += 1;
                }
            }
            if i < n && !is_separator(line[i]) {
                return Err((
                    "a closing quote must be followed by a space or the line's end",
                    i,
                ));
            }
            break;
        }
        words.push(word);
    }
}

/// The value of the hex digit at `at`, if there is one.
fn hex_digit(line: &[u8], at: usize) -> Option<u8> {
    let digit = char::from(*line.get(at)?).to_digit(16)?;
    u8::try_from(digit).ok()
}

#[cfg(test)]
mod tests;
