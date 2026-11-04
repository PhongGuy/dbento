//! Catalog introspection types: the schemas, relations and columns a session can list.

use pg_query::protobuf::KeywordKind;

use crate::session::ExecuteError;
use crate::workspace::{ConnectionConfig, Engine};

/// One schema of a database.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaInfo {
    name: String,
}

impl SchemaInfo {
    /// A schema called `name`.
    pub fn new(name: impl Into<String>) -> Self {
        SchemaInfo { name: name.into() }
    }

    /// The schema's name, unquoted.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A relation named by its schema and its own name.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationRef {
    schema: String,
    name: String,
}

impl RelationRef {
    /// The relation `name` in `schema`.
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        RelationRef {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// The schema's name, unquoted.
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// The relation's name, unquoted.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// What kind of relation a catalog entry is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RelationKind {
    /// A table, partitioned tables included.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A foreign table.
    ForeignTable,
    /// Any other relation the engine lists.
    Other,
}

/// One relation of a schema.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    name: String,
    kind: RelationKind,
}

impl Relation {
    /// A relation called `name` of `kind`.
    pub fn new(name: impl Into<String>, kind: RelationKind) -> Self {
        Relation {
            name: name.into(),
            kind,
        }
    }

    /// The relation's name, unquoted.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The relation's kind.
    pub fn kind(&self) -> RelationKind {
        self.kind
    }
}

/// The relations of one schema, at most [`RELATION_CAP`] of them.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationList {
    items: Vec<Relation>,
    truncated: bool,
}

impl RelationList {
    /// A list of `items`; `truncated` marks that the schema has more.
    pub fn new(items: Vec<Relation>, truncated: bool) -> Self {
        RelationList { items, truncated }
    }

    /// The relations in name order.
    pub fn items(&self) -> &[Relation] {
        &self.items
    }

    /// Whether the schema has more relations than were listed.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// One column of a relation, as the catalog describes it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    name: String,
    type_name: String,
    nullable: bool,
    primary_key: Option<u16>,
    default: Option<String>,
}

impl ColumnMeta {
    /// A column description; `primary_key` is the 1-based position in the primary key.
    pub fn new(
        name: impl Into<String>,
        type_name: impl Into<String>,
        nullable: bool,
        primary_key: Option<u16>,
        default: Option<String>,
    ) -> Self {
        ColumnMeta {
            name: name.into(),
            type_name: type_name.into(),
            nullable,
            primary_key,
            default,
        }
    }

    /// The column's name, unquoted.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared type, as the engine spells it.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Whether the column accepts NULL.
    pub fn nullable(&self) -> bool {
        self.nullable
    }

    /// 1-based position in the primary key.
    pub fn primary_key(&self) -> Option<u16> {
        self.primary_key
    }

    /// The default expression's text, if the column has one. On SQL Server, a text that starts
    /// `/* withheld` stands for an expression the login may not read, and one that starts
    /// `/* cut */` is the expression past the definition cap, so neither is always runnable SQL.
    pub fn default(&self) -> Option<&str> {
        self.default.as_deref()
    }
}

/// Why listing schemas, relations or columns failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum IntrospectError {
    /// The catalog query failed the way a statement fails.
    #[error(transparent)]
    Execute(#[from] ExecuteError),
    /// The catalog answered in a shape the driver does not expect.
    #[error("unexpected catalog answer: {message}")]
    Unexpected {
        /// What was unexpected.
        message: String,
    },
    /// No table or view of that name exists in that schema.
    #[error("no table or view {relation}")]
    Missing {
        /// The relation as `schema.name`.
        relation: String,
    },
    /// The engine has no implementation of this call yet.
    #[error("{what} is not available on this engine yet")]
    Unsupported {
        /// What was asked for, as a plural noun phrase.
        what: &'static str,
    },
}

/// The schema an unqualified name resolves to: `public`, `main`, `dbo`, or a MySQL, MongoDB or
/// SurrealDB connection's `database` (None when it names none).
pub fn default_schema(engine: Engine, conn: &ConnectionConfig) -> Option<String> {
    match engine {
        Engine::Postgres => Some("public".to_owned()),
        Engine::Sqlite => Some("main".to_owned()),
        Engine::Mysql => conn.database().map(str::to_owned),
        Engine::Mssql => Some("dbo".to_owned()),
        Engine::Duckdb => Some("main".to_owned()),
        Engine::Mongodb => conn.database().map(str::to_owned),
        Engine::Surrealdb => conn.database().map(str::to_owned),
    }
}

/// Relations listed per schema; the query asks for one more to detect truncation.
pub const RELATION_CAP: usize = 10_000;

/// The first RELATION_CAP items, truncated when more came.
pub(crate) fn capped(mut items: Vec<Relation>) -> RelationList {
    let truncated = items.len() > RELATION_CAP;
    items.truncate(RELATION_CAP);
    RelationList::new(items, truncated)
}

/// `ident` as `engine` reads it inside SQL: bare when it is a plain non-keyword identifier,
/// otherwise double-quoted with embedded quotes doubled. MySQL and SurrealDB always get
/// backticks and SQL Server brackets. MongoDB has no SQL, so its names come back unchanged.
pub fn quote_ident(engine: Engine, ident: &str) -> String {
    let bare = match engine {
        Engine::Postgres => pg_bare(ident),
        Engine::Sqlite => sqlite_bare(ident),
        Engine::Mysql => return format!("`{}`", ident.replace('`', "``")),
        Engine::Mssql => return format!("[{}]", ident.replace(']', "]]")),
        Engine::Duckdb => pg_bare(ident) && DUCKDB_EXTRA_KEYWORDS.binary_search(&ident).is_err(),
        Engine::Mongodb => return ident.to_owned(),
        Engine::Surrealdb => return crate::surreal::lex::quote_ident(ident),
    };
    if bare {
        ident.to_owned()
    } else {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}

/// DuckDB keywords that Postgres reads as plain names but DuckDB rejects as a bare column
/// reference; sorted for the binary search.
const DUCKDB_EXTRA_KEYWORDS: &[&str] = &[
    "anti",
    "asof",
    "at",
    "by",
    "describe",
    "glob",
    "lambda",
    "pivot",
    "pivot_longer",
    "pivot_wider",
    "positional",
    "qualify",
    "semi",
    "show",
    "summarize",
    "unpack",
    "unpivot",
];

/// `text` as a quoted string literal for `engine`; on Postgres a backslash switches to `E'..'`
/// so it reads the same whatever `standard_conforming_strings` says. On MySQL a backslash
/// switches to a hex literal, which reads the same whatever `NO_BACKSLASH_ESCAPES` says.
/// SQL Server takes `N'..'`, so non-ASCII text survives, and a backslash is plain text.
/// MongoDB has no SQL, so its text comes back unchanged. SurrealDB takes `'..'` with
/// backslash escapes.
pub fn quote_literal(engine: Engine, text: &str) -> String {
    if engine == Engine::Mssql {
        return format!("N'{}'", text.replace('\'', "''"));
    }
    if engine == Engine::Mysql && text.contains('\\') {
        let hex: String = text.bytes().map(|b| format!("{b:02x}")).collect();
        return format!("CONVERT(X'{hex}' USING utf8mb4)");
    }
    if engine == Engine::Mongodb {
        return text.to_owned();
    }
    if engine == Engine::Surrealdb {
        return crate::surreal::lex::quote_string(text);
    }
    let quoted = text.replace('\'', "''");
    if engine == Engine::Postgres && text.contains('\\') {
        format!("E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    }
}

/// Postgres folds unquoted names to lower case, so only `[a-z_][a-z0-9_]*` survives bare,
/// and only when the lexer reads it as one identifier or an unreserved keyword.
fn pg_bare(ident: &str) -> bool {
    let plain = ident.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && ident
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    plain
        && pg_query::scan(ident).is_ok_and(|s| {
            matches!(
                s.tokens.as_slice(),
                [t] if t.end as usize == ident.len()
                    && matches!(
                        KeywordKind::try_from(t.keyword_kind),
                        Ok(KeywordKind::NoKeyword | KeywordKind::UnreservedKeyword)
                    )
            )
        })
}

/// SQLite keeps case, so `[A-Za-z_][A-Za-z0-9_]*` stays bare unless it is a keyword. The
/// keyword table holds no one-letter word, and its lookup asserts at least two bytes.
fn sqlite_bare(ident: &str) -> bool {
    ident.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (ident.len() < 2
            || sqlite3_parser::dialect::keyword_token(ident.to_ascii_uppercase().as_bytes())
                .is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_keeps_the_cap_and_marks_more() {
        let make = |n: usize| {
            (0..n)
                .map(|i| Relation::new(format!("t{i}"), RelationKind::Table))
                .collect::<Vec<_>>()
        };
        let over = capped(make(RELATION_CAP + 1));
        assert_eq!((over.items().len(), over.truncated()), (RELATION_CAP, true));
        let exact = capped(make(RELATION_CAP));
        assert_eq!(
            (exact.items().len(), exact.truncated()),
            (RELATION_CAP, false)
        );
    }
}
