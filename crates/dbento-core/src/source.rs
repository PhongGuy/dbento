//! The one table a free SELECT reads, so a query result can offer to open it for editing.

use pg_query::{NodeEnum, NodeRef};
use sqlite3_parser::{
    Bump, FallibleIterator as _,
    ast::{Cmd, Name, OneSelect, SelectTable, Stmt},
    lexer::{
        Scanner,
        sql::{Parser, TokenType, Tokenizer},
    },
};
use sqlparser::{
    ast::{Ident, SetExpr, Statement, TableFactor},
    dialect::{Dialect, MsSqlDialect, MySqlDialect},
    keywords::Keyword,
    parser::Parser as SqlParser,
    tokenizer::{Token, Tokenizer as SqlTokenizer},
};

use crate::{
    Engine, RelationRef,
    tsql::{TsqlKind, tsql_tokens},
};

/// The table `sql` reads when it is exactly one SELECT over one plain table: no join,
/// subquery, CTE, set operation, table function or second statement. An unqualified name
/// gets the engine's default schema (`public`, `main`); on MySQL it takes `default_schema`,
/// the connection's configured database (a `USE` in the session is not tracked), and on SQL
/// Server the connection's default schema; Postgres and SQLite ignore it. None when undecided
/// or unparsable.
pub fn single_source(
    engine: Engine,
    sql: &str,
    default_schema: Option<&str>,
) -> Option<RelationRef> {
    match engine {
        Engine::Postgres => pg_source(sql),
        Engine::Sqlite => sqlite_source(sql),
        Engine::Mysql => my_source(sql, default_schema),
        Engine::Mssql => ms_source(sql, default_schema),
        Engine::Duckdb => None,
        Engine::Mongodb => None,
        Engine::Surrealdb => None,
        Engine::Redis => None,
        Engine::Dynamodb => None,
    }
}

/// Postgres: the top-level SelectStmt must have one RangeVar in FROM, and the whole tree
/// must hold no subquery, function or join; the parser has already folded unquoted names.
fn pg_source(sql: &str) -> Option<RelationRef> {
    let parsed = pg_query::parse(sql, pg_query::ParserOptions::DEFAULT).ok()?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return None;
    };
    let NodeEnum::SelectStmt(select) = raw.stmt.as_ref()?.node.as_ref()? else {
        return None;
    };
    if select.with_clause.is_some()
        || select.into_clause.is_some()
        || select.larg.is_some()
        || select.rarg.is_some()
    {
        return None;
    }
    let [from] = select.from_clause.as_slice() else {
        return None;
    };
    let NodeEnum::RangeVar(table) = from.node.as_ref()? else {
        return None;
    };
    let nested = parsed.protobuf.nodes().into_iter().any(|(node, ..)| {
        matches!(
            node,
            NodeRef::SubLink(_)
                | NodeRef::RangeSubselect(_)
                | NodeRef::RangeFunction(_)
                | NodeRef::JoinExpr(_)
        )
    });
    if nested || !table.catalogname.is_empty() {
        return None;
    }
    let schema = if table.schemaname.is_empty() {
        "public"
    } else {
        &table.schemaname
    };
    Some(RelationRef::new(schema, &table.relname))
}

/// SQLite: one plain SELECT whose FROM is a single table without joins. The AST does not
/// expose nested SELECTs cheaply, so any second SELECT keyword in the text refuses.
fn sqlite_source(sql: &str) -> Option<RelationRef> {
    if select_keywords(sql)? != 1 {
        return None;
    }
    let bump = Bump::new();
    let mut parser = Parser::new(&bump, sql.as_bytes());
    let Ok(Some(Cmd::Stmt(Stmt::Select(select)))) = parser.next() else {
        return None;
    };
    if !matches!(parser.next(), Ok(None))
        || select.with.is_some()
        || select.body.compounds.is_some()
    {
        return None;
    }
    let OneSelect::Select {
        from: Some(from), ..
    } = &select.body.select
    else {
        return None;
    };
    let (Some(SelectTable::Table(name, ..)), None) = (from.select, &from.joins) else {
        return None;
    };
    let schema = name.db_name.as_ref().map_or("main".to_owned(), unquote);
    Some(RelationRef::new(schema, unquote(&name.name)))
}

/// MySQL: one plain SELECT over one table, parsed with sqlparser's MySQL dialect. Versioned
/// comments can hide a join the server runs, so any `/*!` or `/*M!` refuses, as does a second
/// SELECT keyword anywhere in the text.
fn my_source(sql: &str, default_schema: Option<&str>) -> Option<RelationRef> {
    if sql.contains("/*!") || sql.contains("/*M!") {
        return None;
    }
    let tokens = SqlTokenizer::new(&MySqlDialect {}, sql).tokenize().ok()?;
    let selects = tokens
        .iter()
        .filter(|t| matches!(t, Token::Word(w) if w.keyword == Keyword::SELECT))
        .count();
    if selects != 1 {
        return None;
    }
    let parts = plain_table(&MySqlDialect {}, sql)?;
    match parts.as_slice() {
        [db, table] => Some(RelationRef::new(&db.value, &table.value)),
        // A bare DUAL is MySQL's pseudo-table, not a relation; a backticked one can be real.
        [table] if table.quote_style.is_none() && table.value.eq_ignore_ascii_case("dual") => None,
        [table] => Some(RelationRef::new(default_schema?, &table.value)),
        _ => None,
    }
}

/// SQL Server: one plain SELECT over one table. T-SQL needs no `;` between statements, so the
/// SELECT words are counted with the lexer that reads text as the server does. A bare `#` or
/// `@` name is a session's own table, never a catalog relation; a three-part name is elsewhere.
fn ms_source(sql: &str, default_schema: Option<&str>) -> Option<RelationRef> {
    let selects = tsql_tokens(sql)
        .iter()
        .filter(|t| t.kind == TsqlKind::Word && sql[t.range.clone()].eq_ignore_ascii_case("select"))
        .count();
    if selects != 1 {
        return None;
    }
    let parts = plain_table(&MsSqlDialect {}, sql)?;
    let local = |name: &Ident| name.quote_style.is_none() && name.value.starts_with(['#', '@']);
    match parts.as_slice() {
        [.., table] if local(table) => None,
        [schema, table] => Some(RelationRef::new(&schema.value, &table.value)),
        [table] => Some(RelationRef::new(default_schema?, &table.value)),
        _ => None,
    }
}

/// The name parts of the one table `sql` selects from, when it parses in `dialect` as a single
/// SELECT with no CTE, INTO, FOR clause, join or table function.
fn plain_table(dialect: &dyn Dialect, sql: &str) -> Option<Vec<Ident>> {
    let statements = SqlParser::parse_sql(dialect, sql).ok()?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    if query.with.is_some() || query.for_clause.is_some() || select.into.is_some() {
        return None;
    }
    let [from] = select.from.as_slice() else {
        return None;
    };
    let TableFactor::Table {
        name, args: None, ..
    } = &from.relation
    else {
        return None;
    };
    if !from.joins.is_empty() {
        return None;
    }
    name.0.iter().map(|p| p.as_ident().cloned()).collect()
}

/// How many SELECT keyword tokens `sql` holds; None when the lexer rejects the text.
fn select_keywords(sql: &str) -> Option<usize> {
    let mut scanner = Scanner::new(Tokenizer::new());
    let mut count = 0;
    loop {
        match scanner.scan(sql.as_bytes()).ok()? {
            (_, None, _) => return Some(count),
            (_, Some((_, TokenType::TK_SELECT)), _) => count += 1,
            _ => {}
        }
    }
}

/// A name as SQLite stores it: outer `"`, `` ` ``, `'` or `[]` removed, doubled quotes undoubled.
pub(crate) fn unquote(name: &Name) -> String {
    let text = name.0;
    let mut chars = text.chars();
    match (chars.next(), chars.next_back()) {
        (Some('['), Some(']')) => chars.as_str().to_owned(),
        (Some(open), Some(close)) if open == close && matches!(open, '"' | '`' | '\'') => {
            let mut doubled = String::with_capacity(2);
            doubled.extend([open, open]);
            chars.as_str().replace(&doubled, &open.to_string())
        }
        _ => text.to_owned(),
    }
}
