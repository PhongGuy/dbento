//! What a SurrealQL unit does, by a scan of its tokens. This is a courtesy, not a boundary:
//! the fence is what holds. The driver spec's "Write detection" has the rules, and fence
//! probe sections F6 and F7 the evidence.

use super::lex::{Kind, Tok, alternate, lex};
use crate::session::{Classification, Unguarded, Writes};

/// The verbs that write wherever an expression can stand.
const WRITE_VERBS: [&str; 10] = [
    "create", "update", "upsert", "delete", "insert", "relate", "define", "remove", "alter",
    "rebuild",
];

/// Verbs that are plain names anywhere but a statement's start.
const STATEMENT_VERBS: [&str; 3] = ["live", "kill", "access"];

/// `::` heads whose functions neither write nor run stored code.
const PURE_PATHS: [&str; 19] = [
    "array", "bytes", "crypto", "duration", "encoding", "geo", "math", "object", "parse", "rand",
    "record", "search", "session", "set", "string", "time", "type", "value", "vector",
];

/// File methods; each also has `_..` forms such as `put_if_not_exists`.
const FILE_METHODS: [&str; 4] = ["put", "copy", "rename", "delete"];

/// The operations a `PERMISSIONS FOR` list names.
const PERMISSION_WORDS: [&str; 4] = ["select", "create", "update", "delete"];

/// What `REMOVE` takes for its target to count as a drop.
const DROPPED_OBJECTS: [&str; 5] = ["table", "database", "db", "namespace", "ns"];

/// The words that end a target.
const CLAUSE_WORDS: [&str; 12] = [
    "set", "content", "merge", "replace", "patch", "unset", "where", "return", "timeout",
    "parallel", "explain", "with",
];

/// What the unit `text` does, known before it runs. Both lexer readings count, so a wrong
/// guess at a `/` cannot hide a verb.
pub(crate) fn surreal_classification(text: &str) -> Classification {
    let (toks, error) = lex(text);
    let first = match error {
        // The server refuses the whole call, so nothing in this reading runs.
        Some(_) => Classification {
            writes: Writes::Unknown,
            unguarded: Vec::new(),
            transaction_control: false,
        },
        None => scan(&toks),
    };
    match alternate(text) {
        Some(second) => union(first, scan(&second)),
        None => first,
    }
}

fn union(mut first: Classification, second: Classification) -> Classification {
    first.writes = higher(first.writes, second.writes);
    first.transaction_control |= second.transaction_control;
    for kind in second.unguarded {
        add_kind(&mut first.unguarded, kind);
    }
    first
}

// Each kind appears once, in first-seen order, because a confirmation prompt names each.
fn add_kind(kinds: &mut Vec<Unguarded>, kind: Unguarded) {
    if !kinds.contains(&kind) {
        kinds.push(kind);
    }
}

fn higher(a: Writes, b: Writes) -> Writes {
    match (a, b) {
        (Writes::Yes, _) | (_, Writes::Yes) => Writes::Yes,
        (Writes::Unknown, _) | (_, Writes::Unknown) => Writes::Unknown,
        _ => Writes::No,
    }
}

// One reading of the text. `define` is the bracket depth of the open `DEFINE`, whose body
// runs later: nothing inside it is unguarded until a `;` at that depth or its closer.
fn scan(toks: &[Tok]) -> Classification {
    let mut out = Classification {
        writes: Writes::No,
        unguarded: Vec::new(),
        transaction_control: false,
    };
    let mut depth = 0usize;
    let mut define = None;
    let mut i = 0;
    while let Some(tok) = toks.get(i) {
        match tok.kind {
            Kind::Punct('(' | '[' | '{') => depth += 1,
            Kind::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            _ => {}
        }
        if define.is_some_and(|open| depth < open || (depth == open && tok.is_punct(';'))) {
            define = None;
        }
        out.writes = higher(out.writes, name_writes(toks, i));
        if tok.kind == Kind::Script || is_script_head(toks, i) {
            out.writes = higher(out.writes, Writes::Unknown);
        }
        if tok.word().is_none() || is_name(toks, i) {
            i += 1;
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        if tok.is_word("for") {
            i = past_permissions(toks, i);
            continue;
        }
        if is_verb(tok) {
            out.writes = Writes::Yes;
            if tok.is_word("define") && define.is_none() {
                define = Some(depth);
            }
            if define.is_none()
                && let Some(kind) = unguarded_kind(toks, i)
            {
                add_kind(&mut out.unguarded, kind);
            }
        }
        let starts_block = prev.is_none_or(|p| p.is_punct(';') || p.is_punct('{'));
        if starts_block && tok.is_any_word(&STATEMENT_VERBS) {
            out.writes = Writes::Yes;
        }
        let starts_script = depth == 0 && prev.is_none_or(|p| p.is_punct(';'));
        if starts_script && tok.is_any_word(&["begin", "commit", "cancel"]) {
            out.transaction_control = true;
        }
        i += 1;
    }
    out
}

// What the name at `i` says by itself, quoted or not: a `::` path head or a file method.
fn name_writes(toks: &[Tok], i: usize) -> Writes {
    let Some(name) = toks[i].name() else {
        return Writes::No;
    };
    let name = name.to_ascii_lowercase();
    let prev = i.checked_sub(1).map(|p| &toks[p]);
    let next = toks.get(i + 1);
    let head = next.is_some_and(|n| n.kind == Kind::PathSep)
        && prev.is_none_or(|p| p.kind != Kind::PathSep);
    if head {
        return path_writes(&name, toks.get(i + 2));
    }
    let called = next.is_some_and(|n| n.is_punct('('));
    if called && prev.is_some_and(|p| p.is_punct('.')) && is_file_method(&name) {
        return Writes::Yes;
    }
    Writes::No
}

fn path_writes(head: &str, member: Option<&Tok>) -> Writes {
    let nextval = || {
        member
            .and_then(Tok::name)
            .is_some_and(|name| name.eq_ignore_ascii_case("nextval"))
    };
    match head {
        "http" | "file" => Writes::Yes,
        "sequence" if nextval() => Writes::Yes,
        _ if PURE_PATHS.contains(&head) => Writes::No,
        _ => Writes::Unknown,
    }
}

fn is_file_method(name: &str) -> bool {
    FILE_METHODS.iter().any(|method| {
        name.strip_prefix(method)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    })
}

fn is_script_head(toks: &[Tok], i: usize) -> bool {
    toks[i].is_word("function")
        && !is_name(toks, i)
        && toks.get(i + 1).is_some_and(|next| next.is_punct('('))
}

// Whether the word at `i` is a name by its position, whatever it spells: a field, a path
// segment, an alias, an `ORDER BY` column, an object key, or `ON DUPLICATE KEY UPDATE`.
fn is_name(toks: &[Tok], i: usize) -> bool {
    let back = |n: usize| i.checked_sub(n).map(|p| &toks[p]);
    let after_name_mark = back(1).is_some_and(|p| {
        p.kind == Kind::PathSep || p.is_punct('.') || p.is_word("as") || p.is_word("by")
    });
    let duplicate_key = back(1).is_some_and(|p| p.is_word("key"))
        && back(2).is_some_and(|p| p.is_word("duplicate"));
    after_name_mark || duplicate_key || toks.get(i + 1).is_some_and(|next| next.is_punct(':'))
}

fn is_verb(tok: &Tok) -> bool {
    tok.is_any_word(&WRITE_VERBS)
}

// Steps over the `select, update, ..` of `FOR select, update`, which name operations. A
// loop's `FOR $i IN ..` has no such list.
fn past_permissions(toks: &[Tok], for_word: usize) -> usize {
    let listed = |i: usize| {
        toks.get(i)
            .is_some_and(|tok| tok.is_any_word(&PERMISSION_WORDS))
    };
    let mut at = for_word + 1;
    while listed(at) {
        at += 1;
        if toks.get(at).is_some_and(|tok| tok.is_punct(',')) && listed(at + 1) {
            at += 1;
        }
    }
    at
}

fn unguarded_kind(toks: &[Tok], verb: usize) -> Option<Unguarded> {
    let tok = &toks[verb];
    if tok.is_word("remove") {
        let object = toks.get(verb + 1)?;
        return object
            .is_any_word(&DROPPED_OBJECTS)
            .then_some(Unguarded::Drop);
    }
    let kind = if tok.is_word("update") {
        Unguarded::UpdateWithoutWhere
    } else if tok.is_word("delete") {
        Unguarded::DeleteWithoutWhere
    } else {
        return None;
    };
    let mut target = verb + 1;
    for word in ["from", "only"] {
        if toks.get(target).is_some_and(|tok| tok.is_word(word)) {
            target += 1;
        }
    }
    (!names_records(toks, target) && !has_own_where(toks, target)).then_some(kind)
}

// Whether the target at `at` is only named records. Lists flatten: each `[` must find its
// `]` by the end, and anything that is not a record makes the whole target a table.
fn names_records(toks: &[Tok], mut at: usize) -> bool {
    let mut lists = 0usize;
    loop {
        while toks.get(at).is_some_and(|tok| tok.is_punct('[')) {
            lists += 1;
            at += 1;
        }
        if !is_record(toks, at) {
            return false;
        }
        at = target_end(toks, at + 1);
        while lists > 0 && toks.get(at).is_some_and(|tok| tok.is_punct(']')) {
            lists -= 1;
            at += 1;
        }
        if !toks.get(at).is_some_and(|tok| tok.is_punct(',')) {
            return lists == 0;
        }
        at += 1;
    }
}

// `table:key`, `r'..'` or `type::record(..)`. An unbounded `g:..` starts at the first
// record, so it counts as the table.
fn is_record(toks: &[Tok], at: usize) -> bool {
    let Some(head) = toks.get(at) else {
        return false;
    };
    let colon = toks.get(at + 1).is_some_and(|next| next.is_punct(':'));
    let open_range = toks.get(at + 2).is_some_and(|key| key.kind == Kind::DotDot);
    let named = |tok: Option<&Tok>, name: &str| {
        tok.and_then(Tok::name)
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    let record_call = named(Some(head), "type")
        && toks
            .get(at + 1)
            .is_some_and(|sep| sep.kind == Kind::PathSep)
        && named(toks.get(at + 2), "record");
    head.kind == (Kind::Str { prefix: Some(b'r') })
        || (head.name().is_some() && colon && !open_range)
        || record_call
}

// The token that ends the item running from `at`: a `,`, `;`, closer or clause word outside
// any bracket of its own.
fn target_end(toks: &[Tok], mut at: usize) -> usize {
    let mut depth = 0usize;
    while let Some(tok) = toks.get(at) {
        match tok.kind {
            Kind::Punct('(' | '[' | '{') => depth += 1,
            Kind::Punct(')' | ']' | '}') if depth == 0 => break,
            Kind::Punct(')' | ']' | '}') => depth -= 1,
            Kind::Punct(',' | ';') if depth == 0 => break,
            _ if depth == 0 && tok.is_any_word(&CLAUSE_WORDS) => break,
            _ => {}
        }
        at += 1;
    }
    at
}

// Whether a `WHERE` belongs to the statement whose target is at `at`. The verb may sit
// inside another statement, so a `FROM` or a later verb ends the search: what follows is theirs.
fn has_own_where(toks: &[Tok], at: usize) -> bool {
    let mut depth = 0usize;
    for (i, tok) in toks.iter().enumerate().skip(at) {
        match tok.kind {
            Kind::Punct('(' | '[' | '{') => depth += 1,
            Kind::Punct(')' | ']' | '}') if depth == 0 => return false,
            Kind::Punct(')' | ']' | '}') => depth -= 1,
            Kind::Punct(';') if depth == 0 => return false,
            _ if depth > 0 || is_name(toks, i) => {}
            _ if tok.is_word("where") => return true,
            _ if tok.is_word("from") || is_verb(tok) => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests;
