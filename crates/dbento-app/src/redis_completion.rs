//! Completion for Redis command lines from the server's COMMAND DOCS: command names on the
//! first word, subcommands on the second, and argument tokens such as `NX` after that.

use std::ops::Range;

use dbento_core::{ArgDoc, CommandDoc};

use crate::catalog::CatalogQuery;
use crate::completion::{Candidate, CandidateKind, Completion, MAX_ITEMS, starts};
use crate::schema_tree::CatalogTree;

/// One command's documentation as completion uses it, with nested arguments flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct RedisDoc {
    /// The server's lowercase name; a subcommand is `container|sub`, such as `config|get`.
    pub name: String,
    /// One line on what the command does.
    pub summary: String,
    /// The arguments in order, choices and block parts in line with their parents.
    pub args: Vec<RedisArg>,
    /// The subcommands, sorted by name.
    pub subcommands: Vec<RedisDoc>,
}

/// One argument of a [`RedisDoc`].
#[derive(Debug, Clone, PartialEq)]
pub struct RedisArg {
    /// The argument's name, such as `key` or `seconds`.
    pub name: String,
    /// The word typed before the value, such as `EX`.
    pub token: Option<String>,
    /// The argument may be left out.
    pub optional: bool,
    /// The argument may repeat.
    pub multiple: bool,
}

impl From<&CommandDoc> for RedisDoc {
    fn from(doc: &CommandDoc) -> Self {
        let mut args = Vec::new();
        flatten(&doc.arguments, false, &mut args);
        RedisDoc {
            name: doc.name.clone(),
            summary: doc.summary.clone(),
            args,
            subcommands: doc.subcommands.iter().map(RedisDoc::from).collect(),
        }
    }
}

// Depth first, so a `oneof` or `block` offers its parts' tokens (SET's EX sits under
// `expiration`). A part is optional when any ancestor is; a parent with parts stays only
// when it has a token of its own, as `LIMIT` does.
fn flatten(args: &[ArgDoc], optional: bool, out: &mut Vec<RedisArg>) {
    for a in args {
        let optional = optional || a.optional;
        if a.arguments.is_empty() || a.token.is_some() {
            out.push(RedisArg {
                name: a.name.clone(),
                token: a.token.clone(),
                optional,
                multiple: a.multiple,
            });
        }
        flatten(&a.arguments, optional, out);
    }
}

/// The usage line of `doc`, such as `SET key value [NX] [EX seconds]` or `DEL key [key ...]`.
pub fn signature(doc: &RedisDoc) -> String {
    let mut out = doc.name.replace('|', " ").to_uppercase();
    for a in &doc.args {
        let word = match &a.token {
            Some(t) if t.eq_ignore_ascii_case(&a.name) => t.clone(),
            Some(t) => format!("{t} {}", a.name),
            None => a.name.clone(),
        };
        let word = if a.multiple {
            format!("{word} [{word} ...]")
        } else {
            word
        };
        out.push(' ');
        if a.optional {
            out.push_str(&format!("[{word}]"));
        } else {
            out.push_str(&word);
        }
    }
    out
}

/// One word of a command line: its bytes in the line and whether it was quoted.
struct Word {
    span: Range<usize>,
    quoted: bool,
}

/// Splits `line` as redis-cli does, on whitespace with `"` and `'` quotes; None when the
/// line ends inside an open quote.
fn words(line: &str) -> Option<Vec<Word>> {
    let mut out = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut quoted = false;
        let mut end = line.len();
        while let Some(&(j, c)) = chars.peek() {
            if c.is_whitespace() {
                end = j;
                break;
            }
            chars.next();
            if c == '"' || c == '\'' {
                quoted = true;
                // A double-quoted backslash escapes the next character; single quotes only
                // escape their own quote, which skipping it covers too.
                let mut closed = false;
                while let Some((_, q)) = chars.next() {
                    if q == '\\' {
                        chars.next();
                    } else if q == c {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return None;
                }
            }
        }
        out.push(Word {
            span: i..end,
            quoted,
        });
    }
    Some(out)
}

/// Completes the Redis command line `line`, which starts at byte `start` of the buffer, at
/// buffer byte `offset`, from `conn`'s COMMAND DOCS in `tree`; asks for them when missing.
pub(crate) fn complete_redis(
    line: &str,
    start: usize,
    offset: usize,
    tree: &CatalogTree,
    conn: &str,
) -> Completion {
    let none = Completion {
        replace: offset..offset,
        items: Vec::new(),
        wants: Vec::new(),
    };
    let Some(off) = offset.checked_sub(start) else {
        return none;
    };
    if off > line.len() || !line.is_char_boundary(off) {
        return none;
    }
    let before = &line[..off];
    if before.trim_start().starts_with('#') {
        return none;
    }
    let Some(mut words) = words(before) else {
        return none;
    };
    // The word under the caret is the last one unless whitespace already ended it.
    let current = words.pop_if(|w| w.span.end == off).unwrap_or(Word {
        span: off..off,
        quoted: false,
    });
    if current.quoted {
        return none;
    }
    let Some(docs) = tree.command_docs(conn) else {
        return Completion {
            wants: vec![CatalogQuery::CommandDocs],
            ..none
        };
    };
    let prefix = &before[current.span.clone()];
    let replace = start + current.span.start..offset;
    let typed: Vec<&str> = words.iter().map(|w| &before[w.span.clone()]).collect();
    let items = match typed.as_slice() {
        // An empty line offers nothing rather than every command the server knows.
        [] if prefix.is_empty() => Vec::new(),
        [] => (docs.iter())
            .filter(|d| starts(&d.name, prefix))
            .map(|d| keyword(d.name.to_uppercase(), signature(d)))
            .collect(),
        [cmd, rest @ ..] => {
            let Some(doc) = docs.iter().find(|d| d.name.eq_ignore_ascii_case(cmd)) else {
                return none;
            };
            match rest {
                [] if !doc.subcommands.is_empty() => (doc.subcommands.iter())
                    .filter_map(|d| {
                        let sub = d.name.split_once('|').map_or(d.name.as_str(), |(_, s)| s);
                        starts(sub, prefix).then(|| keyword(sub.to_uppercase(), signature(d)))
                    })
                    .collect(),
                _ => tokens(doc, rest.first().copied(), prefix),
            }
        }
    };
    Completion {
        replace,
        items: items.into_iter().take(MAX_ITEMS).collect(),
        wants: Vec::new(),
    }
}

// The argument tokens of `doc`, or of its subcommand named by `sub` when it has any, that
// start with `prefix`, each once.
fn tokens(doc: &RedisDoc, sub: Option<&str>, prefix: &str) -> Vec<Candidate> {
    let target = match (doc.subcommands.is_empty(), sub) {
        (true, _) => doc,
        (false, Some(sub)) => {
            let found = doc.subcommands.iter().find(|d| {
                (d.name.split_once('|')).is_some_and(|(_, s)| s.eq_ignore_ascii_case(sub))
            });
            match found {
                Some(d) => d,
                None => return Vec::new(),
            }
        }
        (false, None) => return Vec::new(),
    };
    let mut out: Vec<Candidate> = Vec::new();
    for t in target.args.iter().filter_map(|a| a.token.as_deref()) {
        if starts(t, prefix) && !out.iter().any(|c| c.label == t) {
            out.push(Candidate {
                label: t.to_owned(),
                insert: t.to_owned(),
                kind: CandidateKind::Keyword,
                detail: "argument".into(),
            });
        }
    }
    out
}

fn keyword(label: String, detail: String) -> Candidate {
    Candidate {
        insert: label.clone(),
        label,
        kind: CandidateKind::Keyword,
        detail,
    }
}
