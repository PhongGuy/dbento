//! Statement classification: writes, unguarded statements and transaction control per engine.

use sqlite3_parser::{
    Bump, FallibleIterator as _,
    ast::{Cmd, Stmt},
    lexer::sql::Parser,
};

use std::collections::VecDeque;

use pg_query::{
    NodeEnum, NodeRef,
    protobuf::{SubLinkType, VariableSetKind},
};
use serde_json::Value;
use sqlparser::{
    ast::{Set, SetExpr, Statement},
    dialect::MySqlDialect,
    parser::Parser as SqlParser,
};

use crate::session::{Classification, Unguarded, Writes};

pub(crate) mod mssql;

/// Writes from SQLite's own `sqlite3_stmt_readonly`; unguarded statements from sqlite3-parser.
pub(crate) fn sqlite_classification(sql: &str, readonly: bool) -> Classification {
    Classification {
        writes: if readonly { Writes::No } else { Writes::Yes },
        unguarded: unguarded(sql),
        transaction_control: keyword_in(
            sql,
            &["BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"],
        ),
        categories: Vec::new(),
    }
}

/// Whether `sql`'s leading keyword is one of `words`, compared without allocating.
fn keyword_in(sql: &str, words: &[&str]) -> bool {
    let keyword = leading_keyword(sql);
    words.iter().any(|w| w.eq_ignore_ascii_case(keyword))
}

/// Whether a statement without columns reports a row count: DML, possibly under WITH, and
/// the Postgres commands whose tag counts rows. DDL, SET and transaction control count none.
pub(crate) fn counts_rows(sql: &str) -> bool {
    keyword_in(sql, COUNTING)
}

const COUNTING: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "WITH", "SELECT", "VALUES", "TABLE", "COPY",
    "FETCH", "MOVE", "EXECUTE",
];

/// `counts_rows` for MySQL, reading the keyword through `mysql_leading_keyword`.
pub(crate) fn mysql_counts_rows(sql: &str) -> bool {
    let keyword = mysql_leading_keyword(sql);
    COUNTING.iter().any(|w| w.eq_ignore_ascii_case(keyword))
}

/// `leading_keyword` with MySQL's comments: `#` lines, `--` only when whitespace, a control
/// character or the end follows, and `/* */` except `/*!` and `/*+`, which are code.
pub(crate) fn mysql_leading_keyword(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let b = rest.as_bytes();
        let line = b.first() == Some(&b'#')
            || (rest.starts_with("--") && crate::ranges::mysql_dashes_comment(b.get(2)));
        if line {
            rest = rest.find('\n').map_or("", |i| &rest[i..]);
        } else if rest.starts_with("/*") && !matches!(b.get(2), Some(b'!' | b'+')) {
            rest = rest[2..].find("*/").map_or("", |i| &rest[i + 4..]);
        } else {
            break;
        }
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Whether `sql`'s leading MySQL keyword is SET or USE, read as written or with every
/// versioned comment's body as code, since the server may run either reading.
pub(crate) fn mysql_changes_settings(sql: &str) -> bool {
    let set = |text: &str| {
        let keyword = mysql_leading_keyword(text);
        keyword.eq_ignore_ascii_case("SET") || keyword.eq_ignore_ascii_case("USE")
    };
    set(sql) || mysql_readings(&mysql_lexemes(sql, true), false).is_some_and(|[code, _]| set(&code))
}

/// The run of ASCII letters that opens `sql` once whitespace, `--` line comments and
/// `/* */` block comments are skipped; an unterminated block comment swallows the rest.
/// A plain scan rather than sqlite3-parser, so text the parser rejects still classifies.
/// The keyword keeps the case it has in `sql`; it is empty when no letter opens the text.
pub fn leading_keyword(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.find('\n').map_or("", |i| &after[i..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |i| &after[i + 2..]);
        } else {
            break;
        }
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Classifies one MySQL statement (or a mis-split text of several) with sqlparser's MySqlDialect.
/// Every parsed statement counts; when the parser fails, `writes` is `Unknown` and only the
/// leading keywords still find DROP, TRUNCATE, transaction control and DML without WHERE.
/// Text with versioned comments counts both with their bodies as code and with them skipped.
/// Text with a backslash in a string also counts as NO_BACKSLASH_ESCAPES would read it.
pub(crate) fn mysql_classification(sql: &str) -> Classification {
    let lexemes = mysql_lexemes(sql, true);
    // The server runs a `/*!` or `/*M!` body only from the version it names, so either may run.
    let mut class = match mysql_readings(&lexemes, false) {
        None => mysql_text_classification(sql),
        Some(readings) => {
            let [a, b] = readings.map(|r| mysql_text_classification(&r));
            let writes = match (a.writes, b.writes) {
                (Writes::Yes, _) | (_, Writes::Yes) => Writes::Yes,
                (Writes::Unknown, _) | (_, Writes::Unknown) => Writes::Unknown,
                _ => Writes::No,
            };
            let mut class = Classification { writes, ..a };
            add_findings(&mut class, b);
            class
        }
    };
    // The session's sql_mode is unknown here, so a backslash may end its string instead; that
    // reading can only add findings, since its strings are guesses the server may never see.
    let escaped =
        |l: &Lexeme<'_>| matches!(l, Lexeme::Quoted(q) if !q.starts_with('`') && q.contains('\\'));
    if lexemes.iter().any(escaped) {
        let plain = mysql_lexemes(sql, false);
        let readings =
            mysql_readings(&plain, true).map_or_else(|| vec![blanked(&plain)], Vec::from);
        for reading in readings {
            let other = mysql_text_classification(&reading);
            if other.writes == Writes::Yes {
                class.writes = Writes::Yes;
            }
            add_findings(&mut class, other);
        }
    }
    class
}

/// Adds `other`'s unguarded kinds not already in `class`, and its transaction control.
fn add_findings(class: &mut Classification, other: Classification) {
    for u in other.unguarded {
        if !class.unguarded.contains(&u) {
            class.unguarded.push(u);
        }
    }
    class.transaction_control |= other.transaction_control;
}

/// `mysql_classification` of text without versioned comments.
fn mysql_text_classification(sql: &str) -> Classification {
    let Ok(statements) = SqlParser::parse_sql(&MySqlDialect {}, sql) else {
        return mysql_keyword_classification(sql);
    };
    let mut class = Classification {
        writes: Writes::No,
        unguarded: Vec::new(),
        transaction_control: false,
        categories: Vec::new(),
    };
    for mut statement in &statements {
        // EXPLAIN ANALYZE runs the statement it explains, so that statement is what counts.
        while let Statement::Explain {
            analyze: true,
            statement: inner,
            ..
        } = statement
        {
            statement = inner;
        }
        if mysql_writes(statement) {
            class.writes = Writes::Yes;
        }
        class.unguarded.extend(mysql_unguarded(statement));
        class.transaction_control |= matches!(
            statement,
            Statement::StartTransaction { .. }
                | Statement::Commit { .. }
                | Statement::Rollback { .. }
                | Statement::Savepoint { .. }
                | Statement::ReleaseSavepoint { .. }
                | Statement::Set(Set::SetTransaction { .. })
                | Statement::LockTables { .. }
                | Statement::UnlockTables
        );
    }
    class
}

/// Reads are queries without DML in their body, SHOW, DESCRIBE, plain EXPLAIN and USE; the rest
/// writes.
fn mysql_writes(statement: &Statement) -> bool {
    match statement {
        Statement::Query(query) => body_writes(&query.body),
        Statement::Explain { .. }
        | Statement::ExplainTable { .. }
        | Statement::Use(_)
        | Statement::ShowCatalogs { .. }
        | Statement::ShowCharset { .. }
        | Statement::ShowCollation { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowObjects { .. }
        | Statement::ShowProcessList { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowTables { .. }
        | Statement::ShowVariable { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowViews { .. } => false,
        _ => true,
    }
}

fn body_writes(body: &SetExpr) -> bool {
    match body {
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => true,
        SetExpr::Query(query) => body_writes(&query.body),
        SetExpr::SetOperation { left, right, .. } => body_writes(left) || body_writes(right),
        _ => false,
    }
}

/// The confirmation a parsed statement needs, if any; a DML body under WITH counts as top level.
fn mysql_unguarded(statement: &Statement) -> Option<Unguarded> {
    match statement {
        Statement::Query(query) => match &*query.body {
            SetExpr::Update(inner) | SetExpr::Delete(inner) => mysql_unguarded(inner),
            _ => None,
        },
        Statement::Update(update) if update.selection.is_none() => {
            Some(Unguarded::UpdateWithoutWhere)
        }
        Statement::Delete(delete) if delete.selection.is_none() => {
            Some(Unguarded::DeleteWithoutWhere)
        }
        Statement::Drop { .. }
        | Statement::DropConnector { .. }
        | Statement::DropDomain { .. }
        | Statement::DropExtension { .. }
        | Statement::DropFunction { .. }
        | Statement::DropOperator { .. }
        | Statement::DropOperatorClass { .. }
        | Statement::DropOperatorFamily { .. }
        | Statement::DropPolicy { .. }
        | Statement::DropProcedure { .. }
        | Statement::DropSecret { .. }
        | Statement::DropTrigger { .. } => Some(Unguarded::Drop),
        Statement::Truncate(_) => Some(Unguarded::Truncate),
        _ => None,
    }
}

/// The classification of text sqlparser rejects, from its leading keywords alone.
fn mysql_keyword_classification(sql: &str) -> Classification {
    let keyword = mysql_leading_keyword(sql);
    let is = |word: &str| word.eq_ignore_ascii_case(keyword);
    let unguarded = if is("DROP") {
        vec![Unguarded::Drop]
    } else if is("TRUNCATE") {
        vec![Unguarded::Truncate]
    } else {
        Vec::new()
    };
    const TX: &[&str] = &[
        "START",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT",
        "RELEASE",
        "XA",
        "LOCK",
        "UNLOCK",
    ];
    let transaction_control =
        TX.iter().any(|w| is(w)) || (is("SET") && sets_transaction(sql, keyword));
    let dml = mysql_dml_without_where(sql);
    Classification {
        writes: if dml.is_some() {
            Writes::Yes
        } else {
            Writes::Unknown
        },
        unguarded: unguarded.into_iter().chain(dml).collect(),
        transaction_control,
        categories: Vec::new(),
    }
}

/// UPDATE or DELETE, also under MariaDB's executing `ANALYZE [FORMAT=x]`, with no top-level
/// WHERE word in code outside quotes and comments.
fn mysql_dml_without_where(sql: &str) -> Option<Unguarded> {
    // Each code word with the paren depth it sits at, so a subquery's WHERE guards nothing.
    let (mut words, mut depth) = (Vec::<(&str, usize)>::new(), 0usize);
    for lexeme in mysql_lexemes(sql, true) {
        let Lexeme::Code(code) = lexeme else {
            continue;
        };
        let mut start = None;
        for (k, c) in code.char_indices() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                start.get_or_insert(k);
                continue;
            }
            if let Some(st) = start.take() {
                words.push((&code[st..k], depth));
            }
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if let Some(st) = start {
            words.push((&code[st..], depth));
        }
    }
    let is = |i: usize, word: &str| words.get(i).is_some_and(|w| w.0.eq_ignore_ascii_case(word));
    let verb = match (is(0, "ANALYZE"), is(1, "FORMAT")) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 3,
    };
    if words
        .iter()
        .any(|&(w, d)| d == 0 && w.eq_ignore_ascii_case("WHERE"))
    {
        None
    } else if is(verb, "UPDATE") {
        Some(Unguarded::UpdateWithoutWhere)
    } else if is(verb, "DELETE") {
        Some(Unguarded::DeleteWithoutWhere)
    } else {
        None
    }
}

/// One run of MySQL text: code, a quoted string or identifier, a comment (`/*+` hints
/// included), or the body of a versioned `/*!` or `/*M!` comment without its version number.
enum Lexeme<'a> {
    Code(&'a str),
    Quoted(&'a str),
    Comment(&'a str),
    Versioned(&'a str),
}

/// Cuts `sql` into lexemes the way the server reads quotes and comments, a backslash escaping
/// the next character in `'...'` and `"..."` when `backslash_escapes`; an unterminated quote or
/// comment runs to the end.
fn mysql_lexemes(sql: &str, backslash_escapes: bool) -> Vec<Lexeme<'_>> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut code) = (0, 0);
    while i < b.len() {
        let (end, lexeme) = match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                let mut j = i + 1;
                while j < b.len() && b[j] != q {
                    j += if backslash_escapes && q != b'`' && b[j] == b'\\' {
                        2
                    } else {
                        1
                    };
                }
                let end = (j + 1).min(b.len());
                (end, Lexeme::Quoted(&sql[i..end]))
            }
            b'#' | b'-'
                if b[i] == b'#'
                    || (sql[i..].starts_with("--")
                        && crate::ranges::mysql_dashes_comment(b.get(i + 2))) =>
            {
                let end = sql[i..].find('\n').map_or(b.len(), |k| i + k);
                (end, Lexeme::Comment(&sql[i..end]))
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let (body_end, end) = sql[i + 2..]
                    .find("*/")
                    .map_or((b.len(), b.len()), |k| (i + 2 + k, i + 4 + k));
                let inner = &sql[i + 2..body_end];
                match inner.strip_prefix('!').or_else(|| inner.strip_prefix("M!")) {
                    Some(versioned) => {
                        let digits = versioned
                            .bytes()
                            .take(6)
                            .take_while(u8::is_ascii_digit)
                            .count();
                        (end, Lexeme::Versioned(&versioned[digits..]))
                    }
                    None => (end, Lexeme::Comment(&sql[i..end])),
                }
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if code < i {
            out.push(Lexeme::Code(&sql[code..i]));
        }
        out.push(lexeme);
        (i, code) = (end, end);
    }
    if code < b.len() {
        out.push(Lexeme::Code(&sql[code..]));
    }
    out
}

/// The text with every versioned comment's body as code, and with each skipped, `'...'` and
/// `"..."` strings written as `''` when `blank`; `None` when the text has no versioned comment.
fn mysql_readings(lexemes: &[Lexeme<'_>], blank: bool) -> Option<[String; 2]> {
    if !lexemes.iter().any(|l| matches!(l, Lexeme::Versioned(_))) {
        return None;
    }
    let (mut expanded, mut removed) = (String::new(), String::new());
    for lexeme in lexemes {
        match lexeme {
            Lexeme::Versioned(body) => {
                expanded.extend([" ", body, " "]);
                removed.push(' ');
            }
            Lexeme::Quoted(text) if blank && !text.starts_with('`') => {
                expanded.push_str("''");
                removed.push_str("''");
            }
            Lexeme::Code(text) | Lexeme::Quoted(text) | Lexeme::Comment(text) => {
                expanded.push_str(text);
                removed.push_str(text);
            }
        }
    }
    Some([expanded, removed])
}

/// The text of lexemes without versioned comments, its `'...'` and `"..."` strings as `''`.
fn blanked(lexemes: &[Lexeme<'_>]) -> String {
    lexemes
        .iter()
        .map(|l| match l {
            Lexeme::Quoted(q) if !q.starts_with('`') => "''",
            Lexeme::Code(t) | Lexeme::Quoted(t) | Lexeme::Comment(t) | Lexeme::Versioned(t) => t,
        })
        .collect()
}

/// Whether the words after `set`, a slice of `sql`, are TRANSACTION, or SESSION or GLOBAL
/// and then TRANSACTION; each word is read from where the slice before it ends.
fn sets_transaction(sql: &str, set: &str) -> bool {
    let after = |word: &str| {
        let end = word.as_ptr() as usize - sql.as_ptr() as usize + word.len();
        mysql_leading_keyword(&sql[end..])
    };
    let next = after(set);
    let next = if ["SESSION", "GLOBAL"]
        .iter()
        .any(|w| w.eq_ignore_ascii_case(next))
    {
        after(next)
    } else {
        next
    };
    next.eq_ignore_ascii_case("TRANSACTION")
}

/// UPDATE or DELETE without WHERE, or a DROP, in the first command of `sql`, also under
/// WITH and EXPLAIN; UPSERT and trigger bodies are not flagged; a parse failure reports nothing.
fn unguarded(sql: &str) -> Vec<Unguarded> {
    let bump = Bump::new();
    let mut parser = Parser::new(&bump, sql.as_bytes());
    match parser.next() {
        Ok(Some(Cmd::Stmt(s) | Cmd::Explain(s) | Cmd::ExplainQueryPlan(s))) => match s {
            Stmt::Delete {
                where_clause: None, ..
            } => vec![Unguarded::DeleteWithoutWhere],
            Stmt::Update {
                where_clause: None, ..
            } => vec![Unguarded::UpdateWithoutWhere],
            Stmt::DropTable { .. }
            | Stmt::DropIndex { .. }
            | Stmt::DropView { .. }
            | Stmt::DropTrigger { .. } => vec![Unguarded::Drop],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// Postgres classification plus the reason a statement is refused outright, if any.
pub(crate) struct PgClass {
    pub(crate) classification: Classification,
    pub(crate) refusal: Option<&'static str>,
    /// A top-level SET or RESET changes session settings (not SET TRANSACTION or SESSION
    /// CHARACTERISTICS, nor a SET clause nested in another statement).
    pub(crate) sets_session: bool,
}

/// Classifies one Postgres script with the server's own parser; a parse failure is
/// `Writes::Unknown` and never a refusal. COPY to or from the client and changes to
/// `client_encoding` are refused because the driver cannot honour them.
pub(crate) fn pg_classification(sql: &str) -> PgClass {
    let Ok(parsed) = pg_query::parse(sql, pg_query::ParserOptions::DEFAULT) else {
        return PgClass {
            classification: Classification {
                writes: Writes::Unknown,
                unguarded: vec![],
                transaction_control: false,
                categories: Vec::new(),
            },
            refusal: None,
            sets_session: false,
        };
    };
    // Top-level shape decides plain reads; the node walk finds writes hidden in CTEs.
    let mut writes = parsed.protobuf.stmts.iter().any(|s| {
        !matches!(
            s.stmt.as_ref().and_then(|n| n.node.as_ref()),
            Some(
                NodeEnum::SelectStmt(_) | NodeEnum::ExplainStmt(_) | NodeEnum::VariableShowStmt(_)
            )
        )
    });
    // Only top-level statements count: the walk below also visits the SET clause of
    // CREATE FUNCTION and ALTER ROLE, which change no setting of this session.
    let sets_session = parsed.protobuf.stmts.iter().any(|s| {
        matches!(
            s.stmt.as_ref().and_then(|n| n.node.as_ref()),
            Some(NodeEnum::VariableSetStmt(v))
                if v.name != "TRANSACTION" && v.name != "SESSION CHARACTERISTICS"
        )
    });
    let (mut unguarded, mut refusal, mut transaction_control) = (vec![], None, false);
    // pg_query's walk stops at PREPARE, so its body is queued by hand: a later EXECUTE
    // runs it without another prompt.
    let mut queue: VecDeque<NodeRef> = parsed.protobuf.nodes().into_iter().map(|n| n.0).collect();
    while let Some(node) = queue.pop_front() {
        match node {
            NodeRef::PrepareStmt(p) => {
                if let Some(body) = p.query.as_ref().and_then(|q| q.node.as_ref()) {
                    queue.extend(body.nodes().into_iter().map(|n| n.0));
                }
            }
            NodeRef::InsertStmt(_) | NodeRef::MergeStmt(_) => writes = true,
            NodeRef::UpdateStmt(s) => {
                writes = true;
                if s.where_clause.is_none() {
                    unguarded.push(Unguarded::UpdateWithoutWhere);
                }
            }
            NodeRef::DeleteStmt(s) => {
                writes = true;
                if s.where_clause.is_none() {
                    unguarded.push(Unguarded::DeleteWithoutWhere);
                }
            }
            NodeRef::TruncateStmt(_) => {
                writes = true;
                unguarded.push(Unguarded::Truncate);
            }
            NodeRef::DropStmt(_)
            | NodeRef::DropdbStmt(_)
            | NodeRef::DropRoleStmt(_)
            | NodeRef::DropOwnedStmt(_)
            | NodeRef::DropTableSpaceStmt(_)
            | NodeRef::DropSubscriptionStmt(_)
            | NodeRef::DropUserMappingStmt(_) => {
                writes = true;
                unguarded.push(Unguarded::Drop);
            }
            NodeRef::SelectStmt(s) if s.into_clause.is_some() || !s.locking_clause.is_empty() => {
                writes = true;
            }
            NodeRef::CopyStmt(c) if c.filename.is_empty() && !c.is_program => {
                refusal = Some("COPY to or from the client is not supported");
            }
            NodeRef::VariableSetStmt(v)
                if v.name == "client_encoding" && v.kind == VariableSetKind::VarSetValue as i32 =>
            {
                refusal = Some("changing client_encoding is not supported");
            }
            // SET TRANSACTION and SET SESSION CHARACTERISTICS change the wrapper's own
            // transaction, so they count as transaction control.
            NodeRef::TransactionStmt(_) => transaction_control = true,
            NodeRef::VariableSetStmt(v)
                if v.name == "TRANSACTION" || v.name == "SESSION CHARACTERISTICS" =>
            {
                transaction_control = true;
            }
            _ => {}
        }
    }
    PgClass {
        classification: Classification {
            writes: if writes { Writes::Yes } else { Writes::No },
            unguarded,
            transaction_control,
            categories: Vec::new(),
        },
        refusal,
        sets_session,
    }
}

/// The catalog objects a read can reach by name, for the volatility check; see
/// `postgres::READ_CHECK_SQL`. Each name is a (schema, name) pair, the schema empty when
/// unqualified. `attrs` are qualified column names past the first part, which Postgres may
/// resolve as a function call on the row (`t.f` as `f(t)`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PgReach {
    pub(crate) funcs: Vec<(String, String)>,
    pub(crate) ops: Vec<(String, String)>,
    pub(crate) rels: Vec<(String, String)>,
    pub(crate) types: Vec<(String, String)>,
    pub(crate) attrs: Vec<String>,
}

/// Collects every function, operator, relation, type and attribute name `sql` reaches, or
/// `None` when it does not parse or names one in a form the check cannot look up.
pub(crate) fn pg_reach(sql: &str) -> Option<PgReach> {
    let parsed = pg_query::parse(sql, pg_query::ParserOptions::DEFAULT).ok()?;
    // pg_query's own walker skips some expression nodes, such as ARRAY[...], so the whole
    // tree is walked in its serialized form instead.
    let tree = serde_json::to_value(&parsed.protobuf).ok()?;
    let mut reach = PgReach::default();
    let mut ctes = Vec::new();
    reach_walk(&tree, &mut reach, &mut ctes)?;
    // A CTE's name in FROM is no catalog relation.
    reach
        .rels
        .retain(|(schema, name)| !(schema.is_empty() && ctes.contains(name)));
    Some(reach)
}

/// Adds what one serialized node names to `reach`, then walks every child.
fn reach_walk(value: &Value, reach: &mut PgReach, ctes: &mut Vec<String>) -> Option<()> {
    let map = match value {
        Value::Array(items) => {
            return items.iter().try_for_each(|v| reach_walk(v, reach, ctes));
        }
        Value::Object(map) => map,
        _ => return Some(()),
    };
    let eq = || (String::new(), "=".to_owned());
    for (key, node) in map {
        match key.as_str() {
            "FuncCall" => reach.funcs.push(json_qualified(&node["funcname"])?),
            "AExpr" => {
                let (schema, name) = json_qualified(&node["name"])?;
                // BETWEEN is named by its keyword and compares with >= and <=.
                if name.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    reach.ops.push((schema.clone(), ">=".to_owned()));
                    reach.ops.push((schema, "<=".to_owned()));
                } else {
                    reach.ops.push((schema, name));
                }
            }
            // `x IN (SELECT ...)` carries no operator name but compares with =.
            "SubLink" if json_strings(&node["oper_name"]).is_empty() => {
                if node["sub_link_type"] == SubLinkType::AnySublink as i32 {
                    reach.ops.push(eq());
                }
            }
            "SubLink" => reach.ops.push(json_qualified(&node["oper_name"])?),
            "SortBy" if !json_strings(&node["use_op"]).is_empty() => {
                reach.ops.push(json_qualified(&node["use_op"])?);
            }
            // USING, NATURAL and `CASE x WHEN` compare with an = they do not name.
            "JoinExpr"
                if node["is_natural"] == true
                    || node["using_clause"]
                        .as_array()
                        .is_some_and(|u| !u.is_empty()) =>
            {
                reach.ops.push(eq());
            }
            "CaseExpr" if !node["arg"].is_null() => reach.ops.push(eq()),
            "RangeVar" => reach.rels.push((
                node["schemaname"].as_str()?.to_owned(),
                node["relname"].as_str()?.to_owned(),
            )),
            "TypeName" | "type_name" if node.is_object() => {
                reach.types.push(json_qualified(&node["names"])?);
            }
            "ColumnRef" => {
                let fields = json_strings(&node["fields"]);
                reach.attrs.extend(fields.into_iter().skip(1));
            }
            "AIndirection" => reach.attrs.extend(json_strings(&node["indirection"])),
            "CommonTableExpr" => ctes.push(node["ctename"].as_str()?.to_owned()),
            _ => {}
        }
        reach_walk(node, reach, ctes)?;
    }
    Some(())
}

/// The String nodes of a serialized node list, skipping any other kind.
fn json_strings(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["node"]["String"]["sval"].as_str().map(str::to_owned))
        .collect()
}

/// A serialized dotted name as (schema, name); a catalog prefix is dropped. `None` past
/// three parts or for a part that is not a plain name.
fn json_qualified(list: &Value) -> Option<(String, String)> {
    let items = list.as_array()?;
    let parts = json_strings(list);
    if parts.len() != items.len() {
        return None;
    }
    match parts.as_slice() {
        [name] => Some((String::new(), name.clone())),
        [schema, name] | [_, schema, name] => Some((schema.clone(), name.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(s: &str, n: &str) -> (String, String) {
        (s.to_owned(), n.to_owned())
    }

    #[test]
    fn a_read_reaches_its_functions_operators_relations_and_cast_types() {
        let r = pg_reach(
            "WITH w AS (SELECT 1) SELECT s.f(a)::my_t, count(*) FROM w, public.t \
             WHERE a BETWEEN 1 AND 2 AND b = ANY (SELECT c FROM u)",
        )
        .unwrap();
        // The walk's order is the parser's, so each list is compared sorted.
        let sorted = |mut v: Vec<(String, String)>| {
            v.sort();
            v
        };
        assert_eq!(sorted(r.funcs), [pair("", "count"), pair("s", "f")]);
        assert_eq!(
            sorted(r.ops),
            [pair("", "<="), pair("", "="), pair("", ">=")]
        );
        assert_eq!(sorted(r.rels), [pair("", "u"), pair("public", "t")]);
        assert_eq!(r.types, [pair("", "my_t")]);
        assert_eq!(pg_reach("SELEC x"), None);
    }

    #[test]
    fn a_read_reaches_names_in_arrays_attributes_and_implied_equality() {
        let r = pg_reach(
            "SELECT ARRAY[g(1)], t.a, (t).h, CASE t.a WHEN 1 THEN 2 END FROM t JOIN u USING (a) \
             WHERE a IN (SELECT b FROM u) ORDER BY a USING <",
        )
        .unwrap();
        assert_eq!(r.funcs, [pair("", "g")]);
        let mut ops = r.ops;
        ops.sort();
        assert_eq!(
            ops,
            [pair("", "<"), pair("", "="), pair("", "="), pair("", "=")]
        );
        let mut attrs = r.attrs;
        attrs.sort();
        assert_eq!(attrs, ["a", "a", "h"]);
    }

    #[test]
    fn only_dml_shaped_text_counts_rows() {
        for sql in [
            "insert into t values (1)",
            "/* x */ With d AS (DELETE FROM t) SELECT 1",
        ] {
            assert!(counts_rows(sql), "{sql}");
        }
        for sql in [
            "CREATE TABLE t(a)",
            "-- c\nSET x = 1",
            "BEGIN",
            "PRAGMA foo",
            "",
        ] {
            assert!(!counts_rows(sql), "{sql}");
        }
    }

    #[test]
    fn a_parse_failure_or_empty_text_reports_nothing() {
        assert!(unguarded("SELEC x").is_empty());
        assert!(unguarded("").is_empty());
    }

    #[test]
    fn comments_and_explain_query_plan_are_seen_through() {
        assert_eq!(
            unguarded("-- note\nEXPLAIN QUERY PLAN DELETE FROM t;"),
            vec![Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(sqlite_classification("SELECT 1;", true).writes, Writes::No);
        assert_eq!(
            sqlite_classification("DELETE FROM t;", false).writes,
            Writes::Yes
        );
    }

    #[test]
    fn sqlite_transaction_control_is_the_leading_keyword_after_comments() {
        for sql in [
            "BEGIN",
            "begin immediate",
            "BEGIN EXCLUSIVE TRANSACTION",
            "COMMIT",
            "END",
            "end transaction",
            "ROLLBACK",
            "ROLLBACK TO a",
            "SAVEPOINT a",
            "RELEASE a",
            "RELEASE SAVEPOINT a",
            "  -- note\n/* x */ COMMIT",
            "/* a */\n-- b\nBEGIN;",
        ] {
            assert!(
                sqlite_classification(sql, true).transaction_control,
                "{sql}"
            );
        }
        for sql in [
            "SELECT 1",
            "SELECT 'BEGIN'",
            "beginx",
            "-- BEGIN\nSELECT 1",
            "/* COMMIT */ SELECT 1",
            "EXPLAIN BEGIN",
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN DELETE FROM u; END",
            "",
            "/* unterminated",
        ] {
            assert!(
                !sqlite_classification(sql, false).transaction_control,
                "{sql}"
            );
        }
    }

    #[test]
    fn sqlite_drops_are_unguarded_and_create_is_not() {
        for sql in [
            "DROP TABLE t",
            "drop table if exists main.t",
            "DROP INDEX i",
            "DROP VIEW IF EXISTS v",
            "DROP TRIGGER tr",
            "/* c */ DROP TABLE t",
            "EXPLAIN DROP TABLE t",
        ] {
            assert_eq!(unguarded(sql), vec![Unguarded::Drop], "{sql}");
        }
        assert!(unguarded("CREATE TABLE t(a)").is_empty());
        assert!(unguarded("DELETE FROM t WHERE a = 1").is_empty());
    }
}

#[cfg(test)]
mod pg_tests {
    use super::*;
    use Unguarded::{DeleteWithoutWhere as D, UpdateWithoutWhere as U};

    fn class(sql: &str) -> (Writes, Vec<Unguarded>) {
        let c = pg_classification(sql);
        assert!(c.refusal.is_none(), "{sql}");
        (c.classification.writes, c.classification.unguarded)
    }

    #[test]
    fn reads_are_selects_shows_and_explains_of_reads() {
        for sql in [
            "SELECT 1",
            "VALUES (1)",
            "TABLE t",
            "SHOW search_path",
            "EXPLAIN SELECT * FROM t",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "-- c\nSELECT 2;",
        ] {
            assert_eq!(class(sql), (Writes::No, vec![]), "{sql}");
        }
    }

    #[test]
    fn writes_include_ddl_locks_select_into_and_utility_statements() {
        for sql in [
            "CREATE TABLE x(a int)",
            "SELECT 1 FOR UPDATE",
            "SELECT * INTO t2 FROM t",
            "INSERT INTO t VALUES (1)",
            "MERGE INTO t USING u ON t.id = u.id WHEN MATCHED THEN DELETE",
            "SET work_mem = '8MB'",
            "DO $$ BEGIN END $$",
            "VACUUM",
        ] {
            assert_eq!(class(sql).0, Writes::Yes, "{sql}");
        }
    }

    #[test]
    fn update_or_delete_without_where_is_found_at_any_depth() {
        assert_eq!(class("UPDATE t SET a = 1"), (Writes::Yes, vec![U]));
        assert_eq!(class("DELETE FROM t"), (Writes::Yes, vec![D]));
        assert_eq!(
            class("UPDATE t SET a = 1 WHERE id = 2"),
            (Writes::Yes, vec![])
        );
        assert_eq!(class("DELETE FROM t WHERE true"), (Writes::Yes, vec![]));
        assert_eq!(
            class("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d"),
            (Writes::Yes, vec![D])
        );
        assert_eq!(
            class("EXPLAIN ANALYZE DELETE FROM t"),
            (Writes::Yes, vec![D])
        );
        assert_eq!(
            class("WITH u AS (UPDATE t SET a = 1 RETURNING *) DELETE FROM t"),
            (Writes::Yes, vec![D, U])
        );
    }

    #[test]
    fn a_prepared_delete_without_where_is_flagged_at_prepare() {
        assert_eq!(class("PREPARE p AS DELETE FROM t"), (Writes::Yes, vec![D]));
        assert_eq!(
            class("PREPARE p AS UPDATE t SET a = 1 WHERE id = $1"),
            (Writes::Yes, vec![])
        );
    }

    #[test]
    fn a_parse_failure_is_unknown_and_unchecked() {
        assert_eq!(class("SELEC x"), (Writes::Unknown, vec![]));
        // Newer-server syntax the bundled parser rejects: a bare GROUP BY ALL.
        assert_eq!(
            class("CREATE TABLE v AS SELECT a FROM t GROUP BY ALL"),
            (Writes::Unknown, vec![])
        );
    }

    #[test]
    fn a_virtual_generated_column_is_ddl_like_any_other() {
        for sql in [
            "CREATE TABLE v (a int GENERATED ALWAYS AS (1) VIRTUAL)",
            "CREATE TABLE v (a int GENERATED ALWAYS AS (1) STORED)",
            "ALTER TABLE t ADD COLUMN b int GENERATED ALWAYS AS (1) VIRTUAL",
        ] {
            assert_eq!(class(sql), (Writes::Yes, vec![]), "{sql}");
        }
    }

    #[test]
    fn transaction_control_is_flagged_but_not_refused() {
        for sql in [
            "BEGIN",
            "START TRANSACTION READ WRITE",
            "COMMIT",
            "END",
            "ROLLBACK",
            "SAVEPOINT a",
            "RELEASE a",
            "ROLLBACK TO a",
            "PREPARE TRANSACTION 'x'",
            "COMMIT PREPARED 'x'",
            "COMMIT AND CHAIN",
            "SET TRANSACTION READ WRITE",
            "SET LOCAL TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        ] {
            let c = pg_classification(sql);
            assert!(c.classification.transaction_control, "{sql}");
            assert!(c.refusal.is_none(), "{sql}");
        }
        for sql in [
            "SET transaction_read_only = off",
            "SELECT set_config('transaction_read_only', 'off', true)",
            "RESET ALL",
            "SET search_path = app",
            "SELECT 1",
            "SELEC x",
        ] {
            assert!(
                !pg_classification(sql).classification.transaction_control,
                "{sql}"
            );
        }
    }

    #[test]
    fn copy_with_the_client_and_encoding_changes_are_refused() {
        for sql in [
            "COPY t FROM STDIN",
            "COPY t TO STDOUT",
            "COPY (SELECT 1) TO STDOUT",
            "SET NAMES 'LATIN1'",
            "SET client_encoding = 'LATIN1'",
            "SET LOCAL CLIENT_ENCODING TO latin1",
        ] {
            assert!(pg_classification(sql).refusal.is_some(), "{sql}");
        }
        for sql in [
            "COPY t TO '/tmp/x'",
            "COPY t FROM PROGRAM 'cat'",
            "RESET client_encoding",
            "SET client_encoding TO DEFAULT",
            "SELECT set_config('client_encoding', 'LATIN1', false)",
            "SET search_path = app",
        ] {
            assert!(pg_classification(sql).refusal.is_none(), "{sql}");
        }
    }

    #[test]
    fn only_top_level_set_and_reset_change_session_settings() {
        for sql in [
            "SET DateStyle = 'SQL, DMY'",
            "SET LOCAL extra_float_digits = 0",
            "RESET IntervalStyle",
            "RESET ALL",
            "SET search_path = app",
            "SET TIME ZONE 'UTC'",
            "SET ROLE NONE",
        ] {
            assert!(pg_classification(sql).sets_session, "{sql}");
        }
        for sql in [
            "SELECT 1",
            "SHOW DateStyle",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            "SELECT set_config('DateStyle', 'SQL', false)",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql SET search_path = public AS 'SELECT 1'",
            "ALTER ROLE CURRENT_USER SET work_mem = '64MB'",
            "ALTER DATABASE postgres SET DateStyle = 'SQL'",
            "not sql at all",
        ] {
            assert!(!pg_classification(sql).sets_session, "{sql}");
        }
    }

    #[test]
    fn every_drop_and_truncate_form_is_unguarded() {
        for sql in [
            "DROP TABLE t",
            "DROP TABLE IF EXISTS a, b CASCADE",
            "DROP DATABASE d",
            "DROP ROLE r",
            "DROP USER u",
            "DROP GROUP g",
            "DROP OWNED BY r",
            "DROP TABLESPACE s",
            "DROP SUBSCRIPTION s",
            "DROP USER MAPPING FOR u SERVER s",
            "DROP SCHEMA s CASCADE",
            "DROP EXTENSION e",
            "DROP SERVER s",
            "DROP POLICY p ON t",
            "DROP TRIGGER tr ON t",
            "DROP RULE r ON t",
            "DROP CAST (int AS text)",
            "DROP PUBLICATION p",
            "DROP EVENT TRIGGER e",
            "DROP TRANSFORM FOR int LANGUAGE sql",
            "DROP INDEX CONCURRENTLY i",
            "DROP MATERIALIZED VIEW m",
            "DROP FUNCTION f()",
            "DROP PROCEDURE p()",
            "DROP ROUTINE r()",
            "DROP AGGREGATE a(int)",
            "DROP OPERATOR + (int, int)",
            "DROP OPERATOR CLASS c USING btree",
            "DROP OPERATOR FAMILY f USING btree",
            "DROP DOMAIN d",
            "DROP TYPE ty",
            "DROP SEQUENCE s",
            "DROP VIEW v",
            "DROP FOREIGN TABLE f",
            "DROP FOREIGN DATA WRAPPER w",
            "DROP ACCESS METHOD a",
            "DROP STATISTICS s",
            "DROP TEXT SEARCH CONFIGURATION c",
            "DROP LANGUAGE l",
            "DROP COLLATION c",
            "DROP CONVERSION c",
            "-- note\ndrop table t",
        ] {
            assert_eq!(class(sql), (Writes::Yes, vec![Unguarded::Drop]), "{sql}");
        }
        for sql in [
            "TRUNCATE t",
            "truncate table only a, b restart identity cascade",
        ] {
            assert_eq!(
                class(sql),
                (Writes::Yes, vec![Unguarded::Truncate]),
                "{sql}"
            );
        }
        assert_eq!(class("ALTER TABLE t DROP COLUMN c"), (Writes::Yes, vec![]));
        assert_eq!(
            class("CREATE RULE r AS ON DELETE TO t DO INSTEAD NOTHING"),
            (Writes::Yes, vec![])
        );
    }
}

#[cfg(test)]
mod mysql_keyword_tests {
    use super::{mysql_changes_settings, mysql_counts_rows, mysql_leading_keyword};

    #[test]
    fn the_mysql_leading_keyword_skips_mysql_comments_only() {
        assert_eq!(
            mysql_leading_keyword("  # c\n-- d\n/* e */ Delete FROM t"),
            "Delete"
        );
        assert_eq!(mysql_leading_keyword("--\tc\nSET @x = 1"), "SET");
        assert_eq!(mysql_leading_keyword("--1\nSELECT 1"), "");
        assert_eq!(mysql_leading_keyword("/*!40101 SET NAMES utf8mb4 */"), "");
        assert_eq!(mysql_leading_keyword("-- only a comment"), "");
        assert!(mysql_counts_rows("# c\nDELETE FROM t WHERE a = 1"));
        assert!(!mysql_counts_rows("# c\nCREATE TABLE t (a INT)"));
    }

    #[test]
    fn a_set_or_use_counts_bare_or_inside_a_versioned_comment() {
        for sql in [
            "SET @x = 1",
            "# c\nset names utf8mb4",
            "/*!40101 SET NAMES utf8mb4 */",
            "/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */",
            "/*M!100100 SET @x = 1 */",
            "  /*! SET autocommit = 0 */",
            "USE shop",
            "# c\nuse `shop`",
            "/*!40101 USE shop */",
        ] {
            assert!(mysql_changes_settings(sql), "{sql}");
        }
        for sql in [
            "SELECT 1",
            "/*!40101 SELECT 1 */",
            "/* SET x = 1 */ SELECT 1",
            "SELECT '/*!40101 SET NAMES utf8mb4 */'",
            "-- SET\nSELECT 1",
            "SELECT 'USE shop'",
            "/* USE shop */ SELECT 1",
        ] {
            assert!(!mysql_changes_settings(sql), "{sql}");
        }
    }
}

#[cfg(test)]
mod mysql_tests {
    use super::mysql_classification;
    use crate::session::{Unguarded, Writes};

    fn writes(sql: &str) -> Writes {
        mysql_classification(sql).writes
    }
    fn unguarded(sql: &str) -> Vec<Unguarded> {
        mysql_classification(sql).unguarded
    }
    fn tx(sql: &str) -> bool {
        mysql_classification(sql).transaction_control
    }

    #[test]
    fn reads_are_no_and_everything_else_that_parses_is_yes() {
        for sql in [
            "SELECT 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1 UNION SELECT 2",
            "SELECT * FROM t1 FOR UPDATE",
            "SHOW TABLES",
            "SHOW CREATE TABLE t1",
            "DESCRIBE t1",
            "EXPLAIN SELECT 1",
            "USE shop",
        ] {
            assert_eq!(writes(sql), Writes::No, "{sql}");
        }
        for sql in [
            "INSERT INTO t1 VALUES (1)",
            "REPLACE INTO t1 VALUES (1)",
            "UPDATE t1 SET a = 1 WHERE id = 2",
            "DELETE FROM t1 WHERE id = 2",
            "WITH d AS (SELECT 1) DELETE FROM t1 WHERE a IN (SELECT * FROM d)",
            "CREATE TABLE t2 (a INT)",
            "CALL p(1)",
            "SET GLOBAL max_connections = 10",
            "SET @x = 1",
            "LOCK TABLES t1 WRITE",
        ] {
            assert_eq!(writes(sql), Writes::Yes, "{sql}");
        }
    }

    #[test]
    fn parse_failures_are_unknown_and_never_refused_for_that_alone() {
        for sql in [
            "LOAD DATA INFILE '/x' INTO TABLE t1",
            "HANDLER t1 OPEN",
            "SELECT * FROM t1 INTO OUTFILE '/tmp/x'",
            "TABLE t1",
            "DO SLEEP(1)",
        ] {
            let c = mysql_classification(sql);
            assert_eq!(
                (c.writes, c.unguarded.len(), c.transaction_control),
                (Writes::Unknown, 0, false),
                "{sql}"
            );
        }
    }

    #[test]
    fn updates_and_deletes_without_where_are_unguarded_in_every_form() {
        assert_eq!(
            unguarded("UPDATE t1 SET a = 1"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert_eq!(
            unguarded("UPDATE t1 JOIN t2 ON t1.id = t2.id SET t1.a = 1"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert_eq!(unguarded("DELETE FROM t1"), [Unguarded::DeleteWithoutWhere]);
        assert_eq!(
            unguarded("delete from t1 order by id limit 10"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("DELETE t1, t2 FROM t1 JOIN t2 ON t1.id = t2.id"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("WITH x AS (SELECT 1) DELETE FROM t1"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("WITH d AS (SELECT 1) UPDATE t1 SET a = 1"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert!(unguarded("UPDATE t1 SET a = 1 WHERE id = 2").is_empty());
        assert!(
            unguarded("DELETE t1 FROM t1 JOIN t2 ON t1.id = t2.id WHERE t2.gone = 1").is_empty()
        );
        assert!(
            unguarded("WITH x AS (SELECT 1) DELETE FROM t1 WHERE a IN (SELECT * FROM x)")
                .is_empty()
        );
        assert!(unguarded("INSERT INTO t1 VALUES (1) ON DUPLICATE KEY UPDATE a = 2").is_empty());
    }

    #[test]
    fn drop_and_truncate_are_unguarded_also_when_the_parser_fails() {
        for sql in [
            "DROP TABLE t1",
            "DROP DATABASE shop",
            "DROP VIEW v",
            "DROP PROCEDURE p",
            "DROP FUNCTION f",
        ] {
            assert_eq!(unguarded(sql), [Unguarded::Drop], "{sql}");
        }
        assert_eq!(unguarded("TRUNCATE TABLE t1"), [Unguarded::Truncate]);
        assert_eq!(unguarded("TRUNCATE t1"), [Unguarded::Truncate]);
        // Trailing junk makes the parser fail; the leading keyword still flags the statement.
        assert_eq!(writes("DROP TABLE t1 junk junk"), Writes::Unknown);
        assert_eq!(
            unguarded("# why\nDROP TABLE t1 junk junk"),
            [Unguarded::Drop]
        );
        assert_eq!(
            unguarded("-- why\ntruncate table t1 junk junk"),
            [Unguarded::Truncate]
        );
    }

    #[test]
    fn transaction_control_is_found_with_and_without_the_parser() {
        for sql in [
            "START TRANSACTION",
            "START TRANSACTION READ ONLY",
            "BEGIN",
            "BEGIN WORK",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT s",
            "RELEASE SAVEPOINT s",
            "ROLLBACK TO SAVEPOINT s",
            "SET TRANSACTION READ WRITE",
            "SET SESSION TRANSACTION READ WRITE",
            "LOCK TABLES t1 READ",
            "UNLOCK TABLES",
            "XA START 'x'",
            "# c\nXA COMMIT 'x'",
            "-- c\n  xa rollback 'x'",
        ] {
            assert!(tx(sql), "{sql}");
        }
        for sql in [
            "SELECT 1",
            "SET @x = 1",
            "SET SESSION sql_mode = ''",
            "--1\nSELECT 1",
            "BEGINNING",
            "UPDATE t1 SET started = 1 WHERE id = 1",
        ] {
            assert!(!tx(sql), "{sql}");
        }
    }

    #[test]
    fn explain_analyze_classifies_the_statement_it_runs() {
        let c = mysql_classification("EXPLAIN ANALYZE DELETE FROM t1");
        assert_eq!(
            (c.writes, c.unguarded),
            (Writes::Yes, vec![Unguarded::DeleteWithoutWhere])
        );
        assert_eq!(
            writes("EXPLAIN ANALYZE UPDATE t1 SET a = 1 WHERE id = 2"),
            Writes::Yes
        );
        assert_eq!(writes("EXPLAIN ANALYZE SELECT * FROM t1"), Writes::No);
        assert!(unguarded("EXPLAIN ANALYZE SELECT * FROM t1").is_empty());
    }

    #[test]
    fn plain_explain_stays_a_read() {
        let c = mysql_classification("EXPLAIN DELETE FROM t1");
        assert_eq!((c.writes, c.unguarded.len()), (Writes::No, 0));
        assert_eq!(writes("EXPLAIN SELECT * FROM t1"), Writes::No);
    }

    #[test]
    fn unparsed_updates_and_deletes_without_where_are_still_unguarded() {
        for sql in [
            "UPDATE t1, t2 SET t1.a = 1",
            "UPDATE LOW_PRIORITY IGNORE t SET a = 1",
        ] {
            let c = mysql_classification(sql);
            assert_eq!(
                (c.writes, c.unguarded),
                (Writes::Yes, vec![Unguarded::UpdateWithoutWhere]),
                "{sql}"
            );
        }
        for sql in [
            "DELETE t.* FROM t",
            "ANALYZE DELETE FROM t",
            "DELETE LOW_PRIORITY QUICK IGNORE FROM t",
        ] {
            let c = mysql_classification(sql);
            assert_eq!(
                (c.writes, c.unguarded),
                (Writes::Yes, vec![Unguarded::DeleteWithoutWhere]),
                "{sql}"
            );
        }
        for sql in [
            "UPDATE t1, t2 SET t1.a = 1 WHERE t1.id = t2.id",
            "UPDATE LOW_PRIORITY IGNORE t SET a = 1 WHERE id = 2",
            "DELETE t.* FROM t WHERE id = 2",
            "ANALYZE DELETE FROM t WHERE id = 2",
            "DELETE LOW_PRIORITY QUICK IGNORE FROM t WHERE id = 2",
        ] {
            assert!(unguarded(sql).is_empty(), "{sql}");
        }
        // A WHERE inside a quote, an identifier or a comment is not one.
        assert_eq!(
            unguarded("UPDATE t1, t2 SET t1.a = 'WHERE', t1.`where` = 1 -- WHERE"),
            [Unguarded::UpdateWithoutWhere]
        );
    }

    #[test]
    fn only_a_top_level_where_guards_an_unparsed_update() {
        assert_eq!(
            unguarded("UPDATE t1, t2 SET t1.a = (SELECT 1 FROM u WHERE u.x = 1)"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert!(unguarded("UPDATE t1, t2 SET t1.a = (SELECT 1) WHERE t1.id = t2.id").is_empty());
    }

    #[test]
    fn versioned_comments_count_both_as_code_and_as_skipped() {
        assert_eq!(
            unguarded("DELETE FROM t /*!99999 WHERE id = 1 */"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("DELETE FROM t /*M!999999 WHERE id = 1 */"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("/*!40000 DELETE FROM t */"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("SELECT 1 /*M!100000 ; DELETE FROM t */"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert_eq!(
            unguarded("/*!80000 TRUNCATE t junk */"),
            [Unguarded::Truncate]
        );
        assert!(tx("/*!40000 START TRANSACTION */"));
        assert!(unguarded("DELETE FROM t WHERE id = 1 /*!99999 AND a = 2 */").is_empty());
        assert!(unguarded("SELECT '/*!40000 DELETE FROM t */'").is_empty());
        assert_eq!(writes("SELECT 1 /*!40000 , 2 */"), Writes::No);
    }

    #[test]
    fn every_statement_of_a_mis_split_text_counts() {
        let c = mysql_classification("SELECT 1; DELETE FROM t1; COMMIT");
        assert_eq!(
            (c.writes, c.unguarded, c.transaction_control),
            (Writes::Yes, vec![Unguarded::DeleteWithoutWhere], true)
        );
    }

    #[test]
    fn a_string_that_no_backslash_escapes_would_end_cannot_hide_a_statement() {
        let c = mysql_classification(r"SELECT 'a\'; DELETE FROM t; -- '");
        assert_eq!(
            (c.writes, c.unguarded, c.transaction_control),
            (Writes::Yes, vec![Unguarded::DeleteWithoutWhere], false)
        );
        assert!(mysql_classification(r#"SELECT "a\"; COMMIT; -- ""#).transaction_control);
        let c = mysql_classification(r"SELECT 'it\'s'");
        assert_eq!(
            (c.writes, c.unguarded, c.transaction_control),
            (Writes::No, vec![], false)
        );
    }
}
