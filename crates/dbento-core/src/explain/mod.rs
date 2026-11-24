//! Explain requests: the one statement a shell asks to plan, checked once, and the exact
//! text each engine is sent for it. The app and the MCP server share these checks.

mod my_json;
mod my_tree;
mod plan;
pub use plan::*;

use crate::Engine;
use crate::classify::{leading_keyword, mysql_classification, pg_classification};
use crate::ranges::statement_ranges;
use crate::session::{Classification, Writes};

/// The statements whose plan can be shown. Anything else, `ANALYZE`, an option list in
/// parentheses or a nested `EXPLAIN` included, is refused before connecting.
const EXPLAINABLE: &[&str] = &[
    "SELECT", "WITH", "VALUES", "TABLE", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE",
];

/// How much the database does for a plan.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainMode {
    /// The planner's estimate; the statement never runs.
    Plan,
    /// The statement runs and reports actual timings and buffers.
    Analyze,
}

/// One checked statement and the text the engine is sent to explain it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRequest {
    /// The engine the text is built for.
    pub engine: Engine,
    /// The mode sent; `Analyze` becomes `Plan` on SQLite, and on MySQL unless the statement
    /// is confirmed to read.
    pub mode: ExplainMode,
    /// `Analyze` was asked and fell back to `Plan`: SQLite has no ANALYZE form, and MySQL
    /// runs ANALYZE for real, so it is kept only for confirmed reads.
    pub analyze_unavailable: bool,
    /// The user's text as given, untrimmed.
    pub statement: String,
    /// The canonical EXPLAIN text: JSON EXPLAIN on Postgres and MySQL plans, the tree form
    /// for MySQL `Analyze`, EXPLAIN QUERY PLAN on SQLite. `Session::explain_sql` gives what
    /// is actually sent, which differs for MariaDB's `ANALYZE FORMAT=JSON`.
    pub sql: String,
    /// The inner statement's classification, set when `Analyze` was asked on Postgres or
    /// MySQL, even when MySQL fell back to `Plan`.
    pub classification: Option<Classification>,
}

/// Why a statement cannot be explained.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExplainError {
    /// The engine has no EXPLAIN support in this build.
    #[error("EXPLAIN is not available on this engine yet")]
    UnsupportedEngine,
    /// The text is blank or holds more than one statement.
    #[error("select one statement to explain")]
    NotOneStatement,
    /// The statement already starts with EXPLAIN.
    #[error("already an EXPLAIN; run it normally")]
    AlreadyExplain,
    /// The statement's leading keyword is outside the allowlist, or a block comment hides it.
    #[error(
        "only SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE, MERGE or REPLACE can be explained, without a leading block comment"
    )]
    NotExplainable,
}

/// Checks `statement` and builds its JSON EXPLAIN on Postgres, its JSON or ANALYZE tree
/// EXPLAIN on MySQL, or EXPLAIN QUERY PLAN on SQLite; any other engine is
/// `UnsupportedEngine`. Fails with the first check that refuses it, in the order of
/// `ExplainError`.
pub fn explain_request(
    engine: Engine,
    statement: &str,
    mode: ExplainMode,
) -> Result<ExplainRequest, ExplainError> {
    let inner = check(engine, statement)?;
    let asked = mode == ExplainMode::Analyze;
    // MySQL's ANALYZE runs the statement with no rollback, so only a statement the classifier
    // confirms reads keeps it. SQLite has no ANALYZE form. Both fall back to the plan and say so.
    let mysql_class = (engine == Engine::Mysql && asked).then(|| mysql_classification(inner));
    let (mode, analyze_unavailable) = match engine {
        Engine::Sqlite => (ExplainMode::Plan, asked),
        Engine::Postgres => (mode, false),
        Engine::Mysql => match &mysql_class {
            Some(c) if c.writes == Writes::No => (ExplainMode::Analyze, false),
            Some(_) => (ExplainMode::Plan, true),
            None => (ExplainMode::Plan, false),
        },
        Engine::Mssql => return Err(ExplainError::UnsupportedEngine),
        Engine::Duckdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Mongodb => return Err(ExplainError::UnsupportedEngine),
        Engine::Surrealdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Redis => return Err(ExplainError::UnsupportedEngine),
        Engine::Dynamodb => return Err(ExplainError::UnsupportedEngine),
        Engine::S3 => return Err(ExplainError::UnsupportedEngine),
    };
    let prefix = match engine {
        Engine::Sqlite => "EXPLAIN QUERY PLAN",
        Engine::Postgres => match mode {
            ExplainMode::Plan => "EXPLAIN (FORMAT JSON)",
            ExplainMode::Analyze => "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)",
        },
        // The tree form is named so the session's `explain_format` cannot change the output.
        Engine::Mysql => match mode {
            ExplainMode::Plan => "EXPLAIN FORMAT=JSON",
            ExplainMode::Analyze => "EXPLAIN ANALYZE FORMAT=TREE",
        },
        Engine::Mssql => return Err(ExplainError::UnsupportedEngine),
        Engine::Duckdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Mongodb => return Err(ExplainError::UnsupportedEngine),
        Engine::Surrealdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Redis => return Err(ExplainError::UnsupportedEngine),
        Engine::Dynamodb => return Err(ExplainError::UnsupportedEngine),
        Engine::S3 => return Err(ExplainError::UnsupportedEngine),
    };
    let sql = prefixed(engine, prefix, inner)?;
    // ANALYZE runs the statement, so the caller needs to know what it writes. The classifier
    // reads a top-level EXPLAIN as a read, so it sees only the inner statement.
    let classification = match engine {
        Engine::Postgres => {
            (mode == ExplainMode::Analyze).then(|| pg_classification(inner).classification)
        }
        Engine::Mysql => mysql_class,
        Engine::Sqlite
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Surrealdb
        | Engine::Redis
        | Engine::Dynamodb
        | Engine::S3 => None,
    };
    Ok(ExplainRequest {
        engine,
        mode,
        analyze_unavailable,
        statement: statement.to_owned(),
        sql,
        classification,
    })
}

/// The plain text form MCP sends: `EXPLAIN <s>` on Postgres and MySQL, or
/// `EXPLAIN QUERY PLAN <s>` on SQLite, after the same checks as `explain_request`.
pub fn explain_text(engine: Engine, statement: &str) -> Result<String, ExplainError> {
    let inner = check(engine, statement)?;
    let prefix = match engine {
        Engine::Sqlite => "EXPLAIN QUERY PLAN",
        Engine::Postgres | Engine::Mysql => "EXPLAIN",
        Engine::Mssql => return Err(ExplainError::UnsupportedEngine),
        Engine::Duckdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Mongodb => return Err(ExplainError::UnsupportedEngine),
        Engine::Surrealdb => return Err(ExplainError::UnsupportedEngine),
        Engine::Redis => return Err(ExplainError::UnsupportedEngine),
        Engine::Dynamodb => return Err(ExplainError::UnsupportedEngine),
        Engine::S3 => return Err(ExplainError::UnsupportedEngine),
    };
    prefixed(engine, prefix, inner)
}

/// Runs the checks in their fixed order and returns the trimmed statement.
fn check(engine: Engine, statement: &str) -> Result<&str, ExplainError> {
    if !engine.capabilities().explain {
        return Err(ExplainError::UnsupportedEngine);
    }
    if statement_ranges(engine, statement).len() != 1 {
        return Err(ExplainError::NotOneStatement);
    }
    if leading_keyword(statement).eq_ignore_ascii_case("EXPLAIN") {
        return Err(ExplainError::AlreadyExplain);
    }
    if !explainable(statement) {
        return Err(ExplainError::NotExplainable);
    }
    Ok(statement.trim())
}

/// `prefix` before `inner`, refused if the prefix changes how the text splits.
fn prefixed(engine: Engine, prefix: &str, inner: &str) -> Result<String, ExplainError> {
    let sql = format!("{prefix} {inner}");
    if statement_ranges(engine, &sql).len() != 1 {
        return Err(ExplainError::NotOneStatement);
    }
    Ok(sql)
}

/// Whether `sql` opens with an EXPLAINABLE keyword as a whole word, after only ASCII
/// whitespace and line comments, each ended at the first CR or LF as Postgres ends them.
/// Block comments are refused there: Postgres nests them, so skipping one without its lexer
/// could hide an `ANALYZE` that would land right after `EXPLAIN`.
fn explainable(sql: &str) -> bool {
    let blank = |s: &str| {
        s.trim_start_matches(|c: char| c.is_ascii_whitespace())
            .to_owned()
    };
    let mut rest = blank(sql);
    while let Some(after) = rest.strip_prefix("--") {
        rest = blank(after.find(['\n', '\r']).map_or("", |i| &after[i..]));
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(rest.len());
    EXPLAINABLE
        .iter()
        .any(|k| rest[..end].eq_ignore_ascii_case(k))
}
