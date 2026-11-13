//! What the result body shows for the run's subject statement, and the failed body's parts.

use dbento_core::{Engine, ExecuteError, leading_keyword};

use crate::keymap::plus_keys;
use crate::results_pane::{displayed, format_rows};
use crate::runner::{StatementEnd, StopReason};
use crate::tab_model::StatementResult;

/// What fills the area under the statement tabs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultBody {
    /// The result grid.
    Grid,
    /// The subject statement failed: its error in place of the grid.
    Failed,
    /// The subject completed without columns: what it changed, as `dml_text` words it.
    Dml(String),
    /// The subject never ran: its run stopped at its confirmation.
    Skipped,
    /// The user stopped the subject before it reported any columns.
    Stopped,
}

/// The statement the body and footer describe: the selected one when valid, else the last
/// with columns, else the last.
pub fn body_subject(results: &[StatementResult], selected: Option<usize>) -> Option<usize> {
    displayed(results, selected).or(results.len().checked_sub(1))
}

/// The body for `body_subject`; Grid without one.
pub fn result_body(results: &[StatementResult], selected: Option<usize>) -> ResultBody {
    let Some(r) = body_subject(results, selected).map(|ix| &results[ix]) else {
        return ResultBody::Grid;
    };
    match r.store.end() {
        Some(StatementEnd::Failed(_)) => ResultBody::Failed,
        Some(StatementEnd::Skipped) => ResultBody::Skipped,
        Some(StatementEnd::Stopped {
            reason: StopReason::User,
            ..
        }) if r.store.columns().is_empty() => ResultBody::Stopped,
        Some(StatementEnd::Complete(o)) if r.store.columns().is_empty() => {
            ResultBody::Dml(dml_text(&r.sql, o.rows_affected))
        }
        _ => ResultBody::Grid,
    }
}

/// "N row(s) updated|inserted|deleted" by `leading_keyword`; another statement:
/// "N row(s) affected" when N > 0, else "Statement completed".
pub fn dml_text(sql: &str, rows_affected: Option<u64>) -> String {
    let keyword = leading_keyword(sql);
    let verb = [
        ("UPDATE", "updated"),
        ("INSERT", "inserted"),
        ("DELETE", "deleted"),
    ]
    .into_iter()
    .find_map(|(k, verb)| k.eq_ignore_ascii_case(keyword).then_some(verb));
    match (verb, rows_affected) {
        (Some(verb), Some(n)) => format!("{} {verb}", format_rows(n)),
        (None, Some(n)) if n > 0 => format!("{} affected", format_rows(n)),
        _ => "Statement completed".to_owned(),
    }
}

/// No results and not `busy` (`running() || browses()`, so a loading table tab stays quiet):
/// "Run a statement with <run_keys>" ("Run a statement" when unbound); an ended result with
/// columns and no rows "No rows", or "No rows match <filter>" with a filter; else None.
pub fn empty_text(
    results: &[StatementResult],
    selected: Option<usize>,
    busy: bool,
    filter: Option<&str>,
    run_keys: Option<&str>,
) -> Option<String> {
    if results.is_empty() {
        return (!busy).then(|| match run_keys {
            Some(k) => format!("Run a statement with {}", plus_keys(k)),
            None => "Run a statement".to_owned(),
        });
    }
    let r = &results[body_subject(results, selected)?];
    let empty = r.store.end().is_some() && !r.store.columns().is_empty() && r.store.rows() == 0;
    empty.then(|| match filter.filter(|f| !f.is_empty()) {
        Some(f) => format!("No rows match {f}"),
        None => "No rows".to_owned(),
    })
}

/// A failed statement's error as the body shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorView {
    /// The database's message, or the error's text when it is not a database error.
    pub message: String,
    /// The database's code (SQLSTATE on Postgres, the extended result code on SQLite, the
    /// error number on MySQL).
    pub code: Option<String>,
    /// The SQLSTATE beside a code that is not one (MySQL's).
    pub sqlstate: Option<String>,
    /// The server's detail line.
    pub detail: Option<String>,
    /// The server's hint.
    pub hint: Option<String>,
    /// Byte offset of the error into `sql`.
    pub position: Option<usize>,
    /// The statement's text.
    pub sql: String,
}

/// The error of a failed statement; None for any other end.
pub fn error_view(r: &StatementResult) -> Option<ErrorView> {
    let Some(StatementEnd::Failed(e)) = r.store.end() else {
        return None;
    };
    let mut v = ErrorView {
        message: r
            .engine
            .map_or_else(|| e.to_string(), |engine| e.message_for(engine)),
        code: None,
        sqlstate: None,
        detail: None,
        hint: None,
        position: None,
        sql: r.sql.clone(),
    };
    if let ExecuteError::Database {
        code,
        message,
        detail,
        hint,
        position,
        sqlstate,
        ..
    } = e
    {
        v.message = message.clone();
        // An empty code (DuckDB text without a class) is no code, so no label or copy line.
        v.code = (!code.is_empty()).then(|| code.clone());
        v.sqlstate = sqlstate.clone();
        v.detail = detail.clone();
        v.hint = hint.clone();
        v.position = *position;
    }
    Some(v)
}

/// The code as the body labels it for `engine`: `SQLSTATE c` on Postgres, `error c` with
/// any SQLSTATE after a dot on MySQL, the class as `c Error` on DuckDB, the AWS code alone on
/// DynamoDB, `code c` otherwise;
/// None without a code.
pub fn code_label(engine: Option<Engine>, v: &ErrorView) -> Option<String> {
    let code = v.code.as_deref()?;
    Some(match (engine, &v.sqlstate) {
        (Some(Engine::Postgres), _) => format!("SQLSTATE {code}"),
        (Some(Engine::Mysql), Some(s)) => format!("error {code} \u{b7} SQLSTATE {s}"),
        (Some(Engine::Mysql), None) | (Some(Engine::Mssql), _) => format!("error {code}"),
        (Some(Engine::Duckdb), _) => format!("{code} Error"),
        // A statement-level SurrealDB error carries code 0, which names nothing.
        (Some(Engine::Surrealdb), _) if code == "0" => return None,
        (Some(Engine::Surrealdb), _) => format!("error {code}"),
        (Some(Engine::Dynamodb), _) => code.to_owned(),
        _ => format!("code {code}"),
    })
}

/// The message, then "Code: ", "SQLSTATE: ", "Detail: " and "Hint: " lines when present, a
/// blank line, and the SQL.
pub fn error_copy_text(v: &ErrorView) -> String {
    let mut out = v.message.clone();
    let lines = [
        ("Code", &v.code),
        ("SQLSTATE", &v.sqlstate),
        ("Detail", &v.detail),
        ("Hint", &v.hint),
    ];
    for (name, value) in lines {
        if let Some(value) = value {
            out.push_str(&format!("\n{name}: {value}"));
        }
    }
    out.push_str("\n\n");
    out.push_str(&v.sql);
    out
}

/// The editor offset of `r`'s error, `source.start + position` clamped to the source; None
/// without both, or once `editor_text` no longer holds `r.sql` at `source`.
pub fn jump_target(editor_text: &str, r: &StatementResult) -> Option<usize> {
    let position = error_view(r)?.position?;
    let source = r.source.clone()?;
    (editor_text.get(source.clone()) == Some(r.sql.as_str()))
        .then(|| (source.start + position).min(source.end))
}
