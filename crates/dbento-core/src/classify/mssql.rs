//! SQL Server batch classification over sqlparser's MsSqlDialect.

use sqlparser::{
    ast::{SetExpr, Statement},
    dialect::MsSqlDialect,
    parser::Parser,
};

use super::{body_writes, mysql_unguarded};
use crate::{
    session::{Classification, Unguarded, Writes},
    tsql::{TsqlKind, tsql_tokens},
};

/// Classifies one T-SQL batch (the text between GO lines) with sqlparser's MsSqlDialect.
pub(crate) fn mssql_classification(batch: &str) -> Classification {
    let mut class = Classification {
        writes: Writes::Unknown,
        unguarded: Vec::new(),
        transaction_control: transaction_control(batch),
        categories: Vec::new(),
    };
    // A batch the parser rejects keeps `Unknown`, and its findings come from a keyword scan.
    match Parser::parse_sql(&MsSqlDialect {}, batch) {
        Ok(statements) => {
            class.writes = if statements.iter().any(writes) {
                Writes::Yes
            } else {
                Writes::No
            };
            for statement in &statements {
                find_unguarded(statement, &mut class);
            }
        }
        Err(_) => class.unguarded = keyword_unguarded(batch),
    }
    class
}

/// Whether MsSqlDialect parses `batch` as exactly one query, the shape whose trailing items
/// (such as a profile set) are drained rather than handed out as results.
pub(crate) fn mssql_single_query(batch: &str) -> bool {
    matches!(
        Parser::parse_sql(&MsSqlDialect {}, batch).as_deref(),
        Ok([Statement::Query(_)])
    )
}

/// Words that start a new statement, so an UPDATE or DELETE without `;` ends before them.
const STATEMENT_STARTS: &str = "SELECT INSERT UPDATE DELETE MERGE DECLARE IF ELSE WHILE BEGIN END \
    EXEC EXECUTE CREATE ALTER DROP TRUNCATE PRINT RAISERROR THROW RETURN COMMIT ROLLBACK SAVE USE \
    WAITFOR GRANT DENY REVOKE";

/// Words before which UPDATE or DELETE names an action or permission, not a statement.
const NOT_A_STATEMENT_AFTER: &str = "ON THEN AFTER FOR OF GRANT DENY REVOKE";

const DROPPABLE: &str = "TABLE VIEW PROCEDURE PROC FUNCTION TRIGGER DATABASE SCHEMA INDEX \
    SYNONYM SEQUENCE TYPE USER LOGIN ROLE";

/// The no-WHERE, DROP and TRUNCATE findings of a batch the parser rejects, read from its tokens
/// at paren depth 0; a batch that defines a routine reports nothing, since it never runs the body.
fn keyword_unguarded(batch: &str) -> Vec<Unguarded> {
    // Each significant token with the paren depth it sits at; comments and spaces drop out.
    let mut depth = 0usize;
    let tokens: Vec<(TsqlKind, &str, usize)> = tsql_tokens(batch)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TsqlKind::Space | TsqlKind::LineComment | TsqlKind::BlockComment
            )
        })
        .map(|t| {
            let text = &batch[t.range];
            // A paren sits at the depth outside it.
            let at = match (t.kind, text) {
                (TsqlKind::Punct, "(") => {
                    depth += 1;
                    depth - 1
                }
                (TsqlKind::Punct, ")") => {
                    depth = depth.saturating_sub(1);
                    depth
                }
                _ => depth,
            };
            (t.kind, text, at)
        })
        .collect();
    // Whether token `i` is a word from the space-separated list `any`, in any case.
    let word = |i: usize, any: &str| {
        tokens.get(i).is_some_and(|&(kind, text, _)| {
            kind == TsqlKind::Word && any.split_whitespace().any(|w| w.eq_ignore_ascii_case(text))
        })
    };
    let punct = |i: usize, p: &str| {
        tokens
            .get(i)
            .is_some_and(|&(k, t, _)| k == TsqlKind::Punct && t == p)
    };
    let routine = "PROC PROCEDURE FUNCTION TRIGGER";
    let defines = (word(0, "CREATE ALTER") && word(1, routine))
        || (word(0, "CREATE") && word(1, "OR") && word(2, "ALTER") && word(3, routine));
    if defines {
        return Vec::new();
    }

    // Whether each token sits inside a CASE expression, its closing END included, so that
    // CASE's ELSE and END are not read as statement boundaries.
    let mut case_depth = 0usize;
    let in_case: Vec<bool> = (0..tokens.len())
        .map(|i| {
            if word(i, "CASE") {
                case_depth += 1;
            } else if word(i, "END") && case_depth > 0 {
                case_depth -= 1;
                return true;
            }
            case_depth > 0
        })
        .collect();

    let mut found = Vec::new();
    for (i, &(_, _, d)) in tokens.iter().enumerate() {
        if d != 0 {
            continue;
        }
        if word(i, "DROP") && word(i + 1, DROPPABLE) {
            found.push(Unguarded::Drop);
        } else if word(i, "TRUNCATE") && word(i + 1, "TABLE") {
            found.push(Unguarded::Truncate);
        } else if word(i, "UPDATE DELETE") {
            let after_clause = i > 0 && (word(i - 1, NOT_A_STATEMENT_AFTER) || punct(i - 1, ","));
            if after_clause || (word(i, "UPDATE") && word(i + 1, "STATISTICS")) {
                continue;
            }
            // The statement runs to a depth-0 `;` or statement-starting word; WHERE must sit
            // at depth 0 inside it, so a subquery's WHERE does not count.
            let end = (i + 1..tokens.len())
                .find(|&j| {
                    let starts = word(j, STATEMENT_STARTS) && !(in_case[j] && word(j, "ELSE END"));
                    tokens[j].2 == 0 && (punct(j, ";") || starts)
                })
                .unwrap_or(tokens.len());
            if !(i + 1..end).any(|j| tokens[j].2 == 0 && word(j, "WHERE")) {
                found.push(if word(i, "UPDATE") {
                    Unguarded::UpdateWithoutWhere
                } else {
                    Unguarded::DeleteWithoutWhere
                });
            }
        }
    }
    found
}

/// The statements an IF, WHILE or bare `BEGIN ... END` runs, or None for any other statement.
fn body(statement: &Statement) -> Option<Vec<&Statement>> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Statement has too many variants to list; the rest share one arm"
    )]
    match statement {
        Statement::If(s) => {
            let blocks = std::iter::once(&s.if_block)
                .chain(&s.elseif_blocks)
                .chain(&s.else_block);
            Some(blocks.flat_map(|b| b.statements()).collect())
        }
        Statement::While(s) => Some(s.while_block.statements().iter().collect()),
        // sqlparser reads a bare `BEGIN ... END` block as a transaction start that has an END.
        Statement::StartTransaction {
            has_end_keyword: true,
            statements,
            ..
        } => Some(statements.iter().collect()),
        _ => None,
    }
}

/// Reads are queries without DML or `INTO`, and the session statements; control flow writes
/// when anything inside it does; everything else writes.
fn writes(statement: &Statement) -> bool {
    if let Some(inner) = body(statement) {
        return inner.into_iter().any(writes);
    }
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Statement has too many variants to list; the rest share one arm"
    )]
    match statement {
        Statement::Query(query) => body_writes(&query.body) || selects_into(&query.body),
        Statement::Declare { .. }
        | Statement::Set(_)
        | Statement::Use(_)
        | Statement::Print(_)
        | Statement::WaitFor(_) => false,
        _ => true,
    }
}

fn selects_into(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.into.is_some(),
        SetExpr::Query(query) => selects_into(&query.body),
        SetExpr::SetOperation { left, right, .. } => selects_into(left) || selects_into(right),
        SetExpr::Values(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => false,
    }
}

/// MySQL's leaf rules, applied through control-flow bodies but never into a CREATE body.
fn find_unguarded(statement: &Statement, class: &mut Classification) {
    match body(statement) {
        Some(inner) => inner.into_iter().for_each(|s| find_unguarded(s, class)),
        None => class.unguarded.extend(mysql_unguarded(statement)),
    }
}

/// SET options a read-only batch must not touch: each can end implicit transactions, which keep
/// a write after a hidden ROLLBACK inside the fence, or turn the close-out into a plan.
const FENCE_BREAKING_OPTIONS: &[&str] = &[
    "IMPLICIT_TRANSACTIONS",
    "ANSI_DEFAULTS",
    "SHOWPLAN_XML",
    "SHOWPLAN_TEXT",
    "SHOWPLAN_ALL",
];

/// Whether the batch's words begin, end or save a transaction, or set an option the read-only
/// fence depends on; comments, strings and quoted names between the words are skipped.
fn transaction_control(batch: &str) -> bool {
    // Punctuation stays in the list, so `BEGIN; TRAN` is not read as one pair and a SET's comma
    // list can be walked; only the comma is kept as itself.
    let words: Vec<&str> = tsql_tokens(batch)
        .into_iter()
        .filter_map(|t| match t.kind {
            TsqlKind::Word => Some(&batch[t.range]),
            TsqlKind::Punct => Some(if &batch[t.range] == "," { "," } else { "" }),
            TsqlKind::Str
            | TsqlKind::Ident
            | TsqlKind::LineComment
            | TsqlKind::BlockComment
            | TsqlKind::Space => None,
        })
        .collect();
    let is = |word: &str, any: &[&str]| any.iter().any(|w| w.eq_ignore_ascii_case(word));
    // SET names a comma list of options; SET STATISTICS names a list of its own, where only XML
    // counts.
    let sets_fence_option = |i: usize| {
        let statistics = words.get(i + 1).is_some_and(|w| is(w, &["STATISTICS"]));
        let (mut at, options) = if statistics {
            (i + 2, &["XML"][..])
        } else {
            (i + 1, FENCE_BREAKING_OPTIONS)
        };
        loop {
            if words.get(at).is_some_and(|w| is(w, options)) {
                return true;
            }
            if words.get(at + 1) != Some(&",") {
                return false;
            }
            at += 2;
        }
    };
    words.iter().enumerate().any(|(i, &word)| {
        let next = words.get(i + 1).copied().unwrap_or("");
        is(word, &["COMMIT", "ROLLBACK"])
            || (is(word, &["BEGIN"]) && is(next, &["TRAN", "TRANSACTION", "DISTRIBUTED"]))
            || (is(word, &["SAVE"]) && is(next, &["TRAN", "TRANSACTION"]))
            || (is(word, &["SET"]) && sets_fence_option(i))
    })
}

/// Whether the batch switches database with USE or changes a session option with SET, at
/// paren depth 0; strings, quoted names, comments and `SET @variable` never count.
pub(crate) fn mssql_changes_settings(batch: &str) -> bool {
    let tokens: Vec<(TsqlKind, &str)> = tsql_tokens(batch)
        .into_iter()
        .filter(|t| {
            t.kind == TsqlKind::Word || t.kind == TsqlKind::Punct || t.kind == TsqlKind::Ident
        })
        .map(|t| (t.kind, &batch[t.range]))
        .collect();
    let mut depth = 0usize;
    (0..tokens.len()).any(|i| {
        match tokens[i] {
            (TsqlKind::Punct, "(") => depth += 1,
            (TsqlKind::Punct, ")") => depth = depth.saturating_sub(1),
            (TsqlKind::Word, w) if depth == 0 => {
                return w.eq_ignore_ascii_case("USE")
                    || (w.eq_ignore_ascii_case("SET") && sets_session_option(&tokens[i + 1..]));
            }
            _ => {}
        }
        false
    })
}

/// Whether the tokens after a SET name a session option. Every option counts, those Dbento
/// itself sets included; a clause SET (`UPDATE ... SET a = 1`, `ON DELETE SET NULL`) does not.
fn sets_session_option(after: &[(TsqlKind, &str)]) -> bool {
    let punct =
        |k: usize, p: &[&str]| matches!(after.get(k), Some((TsqlKind::Punct, t)) if p.contains(t));
    let Some((TsqlKind::Word, word)) = after.first() else {
        return false;
    };
    let assigns = punct(1, &["=", "."])
        || (punct(1, &["+", "-", "*", "/", "%", "&", "^", "|"]) && punct(2, &["="]));
    !word.starts_with('@')
        && !word.eq_ignore_ascii_case("NULL")
        && !word.eq_ignore_ascii_case("DEFAULT")
        && !assigns
}

#[cfg(test)]
mod tests;
