//! SQLite schema objects: indexes and keys from pragmas, checks and names from the CREATE text.

use rusqlite::{Connection, OptionalExtension};
use sqlite3_parser::{
    Bump, FallibleIterator as _,
    ast::{
        Cmd, ColumnConstraint, CreateTableBody, Expr, Name, SortedColumn, Stmt, TableConstraint,
    },
    lexer::sql::Parser,
};

use crate::fkeys::{FkDirection, sqlite_foreign_keys};
use crate::introspect::{IntrospectError, RelationRef, quote_ident};
use crate::source::unquote;
use crate::sqlite::{SqliteDriver, known_schema};
use crate::workspace::Engine;

use super::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, IndexInfo,
    ObjectRef, RelationObjects, TriggerInfo, TriggerState, definition_error, finish,
};

/// One constraint as the CREATE TABLE text declares it; columns unquoted, in clause order.
#[derive(Debug, PartialEq, Eq)]
struct Declared {
    name: Option<String>,
    kind: ConstraintKind,
    columns: Vec<String>,
}

/// What the worker reads in one pass: the relation's real name, its indexes, its keys and
/// checks other than foreign keys, its triggers and the declared clauses (empty: unparsed).
type Listing = (
    String,
    Vec<IndexInfo>,
    Vec<ConstraintInfo>,
    Vec<TriggerInfo>,
    Vec<Declared>,
);

/// Keys, indexes, checks and triggers of `rel` on SQLite; empty lists when it is unknown.
pub(crate) async fn sqlite_relation_objects(
    d: &mut SqliteDriver,
    rel: &RelationRef,
) -> Result<RelationObjects, IntrospectError> {
    let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
    let (real, indexes, mut constraints, mut triggers, declared) =
        match d.introspect(move |c| read(c, &schema, &name)).await? {
            None => return Ok(RelationObjects::new(Vec::new(), Vec::new(), Vec::new())),
            Some(Err(reason)) => return Ok(RelationObjects::unreadable(reason)),
            Some(Ok(listing)) => listing,
        };
    let at = RelationRef::new(rel.schema(), real.as_str());
    for fk in sqlite_foreign_keys(d, &at).await? {
        if fk.direction() == FkDirection::Outgoing {
            let name = declared_name(&declared, ConstraintKind::ForeignKey, fk.columns());
            constraints.push(ConstraintInfo::new(
                at.clone(),
                name,
                ConstraintKind::ForeignKey,
                fk.columns().to_vec(),
                Some(fk.referenced().clone()),
            ));
        }
    }
    constraints.sort_by(|a, b| {
        (rank(a.kind()), a.name(), a.columns()).cmp(&(rank(b.kind()), b.name(), b.columns()))
    });
    triggers.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(RelationObjects::new(indexes, constraints, triggers))
}

/// The stored CREATE text of `object` on SQLite, at most [`DEFINITION_CAP`] bytes. Indexes
/// without text (automatic ones) and constraints answer with their table's text.
pub(crate) async fn sqlite_definition(
    d: &mut SqliteDriver,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    // Each lookup is (catalog row filter, the schema whose catalog is read, the name, the
    // owning relation's name or "").
    let (filter, schema, name, table) = match object {
        ObjectRef::Relation(rel) => (
            "s.type IN ('table', 'view') AND s.name = ?1 COLLATE NOCASE",
            rel.schema(),
            rel.name(),
            "",
        ),
        ObjectRef::Index { rel, name } => (
            "s.type = 'index' AND s.name = ?1 COLLATE NOCASE AND s.tbl_name = ?2 COLLATE NOCASE",
            rel.schema(),
            name.as_str(),
            rel.name(),
        ),
        ObjectRef::Constraint { rel, .. } => (
            "s.type = 'table' AND s.name = ?1 COLLATE NOCASE",
            rel.schema(),
            rel.name(),
            "",
        ),
        ObjectRef::Trigger { rel, name } => (
            "s.type = 'trigger' AND s.name = ?1 COLLATE NOCASE AND s.tbl_name = ?2 COLLATE NOCASE",
            rel.schema(),
            name.as_str(),
            rel.name(),
        ),
        ObjectRef::Type { .. } | ObjectRef::Routine { .. } => {
            return Err(DefinitionError::Unsupported {
                what: object.label(),
            });
        }
    };
    let temp_too = matches!(object, ObjectRef::Trigger { .. });
    let (schema, name, table) = (schema.to_owned(), name.to_owned(), table.to_owned());
    let text = d
        .introspect(move |c| {
            // The schema cannot be bound, so an unknown one is answered before it is quoted in.
            if !known_schema(c, &schema)? {
                return Ok(None);
            }
            let catalog = format!("{}.sqlite_schema", quote_ident(Engine::Sqlite, &schema));
            let cap = i64::try_from(DEFINITION_CAP + 1).unwrap_or(i64::MAX);
            // An automatic index has NULL text; its owning table's text stands in.
            let sql = format!(
                "SELECT substr(coalesce(s.sql, o.sql), 1, ?3) FROM {catalog} s \
                 LEFT JOIN {catalog} o ON o.type = 'table' AND o.name = s.tbl_name COLLATE NOCASE \
                 WHERE {filter}"
            );
            let mut found: Option<Option<String>> = c
                .query_row(&sql, rusqlite::params![name, table, cap], |r| r.get(0))
                .optional()?;
            // A TEMP trigger on this table is stored in temp's catalog instead.
            if found.is_none() && temp_too {
                let sql = sql.replace(&catalog, "temp.sqlite_schema");
                found = c
                    .query_row(&sql, rusqlite::params![name, table, cap], |r| r.get(0))
                    .optional()?;
            }
            Ok(found.flatten())
        })
        .await
        .map_err(|e| definition_error(e, object))?;
    finish(text, DEFINITION_CAP, false, object)
}

/// The worker half of the listing; None when the schema or relation is unknown, and the
/// engine's reason when the relation is a virtual table whose module is not loaded.
fn read(
    c: &Connection,
    schema: &str,
    name: &str,
) -> rusqlite::Result<Option<Result<Listing, String>>> {
    // The schema cannot be bound, so an unknown one is answered before it is quoted in.
    if !known_schema(c, schema)? {
        return Ok(None);
    }
    let catalog = format!("{}.sqlite_schema", quote_ident(Engine::Sqlite, schema));
    let found: Option<(String, Option<String>)> = c
        .query_row(
            &format!(
                "SELECT name, sql FROM {catalog} \
                 WHERE type IN ('table', 'view') AND name = ?1 COLLATE NOCASE"
            ),
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((real, sql)) = found else {
        return Ok(None);
    };
    // Every pragma on such a table fails the same way, so the first one's reason stands.
    match list(c, schema, &catalog, real, sql) {
        Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.starts_with("no such module") => {
            Ok(Some(Err(m)))
        }
        other => other.map(|l| Some(Ok(l))),
    }
}

/// The objects of the relation `real` found in `schema`, whose stored text is `sql`.
fn list(
    c: &Connection,
    schema: &str,
    catalog: &str,
    real: String,
    sql: Option<String>,
) -> rusqlite::Result<Listing> {
    let rel = RelationRef::new(schema, real.as_str());

    // Each index's key columns come from xinfo; an expression column has cid -2 and no name.
    let listed: Vec<(String, bool, String, bool)> = c
        .prepare(
            "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1, ?2) ORDER BY name",
        )?
        .query_map([&real, schema], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut info =
        c.prepare("SELECT cid, name FROM pragma_index_xinfo(?1, ?2) WHERE key = 1 ORDER BY seqno")?;
    let mut indexes = Vec::new();
    let mut constraints = Vec::new();
    for (index, unique, origin, partial) in listed {
        let columns: Vec<String> = info
            .query_map([index.as_str(), schema], |r| {
                let (cid, column): (i64, Option<String>) = (r.get(0)?, r.get(1)?);
                Ok(match column {
                    Some(column) if cid != -2 => column,
                    _ => "(expression)".to_owned(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if origin == "u" {
            constraints.push((ConstraintKind::Unique, columns.clone()));
        }
        indexes.push(IndexInfo::new(
            rel.clone(),
            index,
            columns,
            unique,
            origin == "pk",
            partial,
            None,
        ));
    }

    // A rowid alias has no index, so the primary key always comes from table_info.
    let primary: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk")?
        .query_map([&real, schema], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !primary.is_empty() {
        constraints.push((ConstraintKind::PrimaryKey, primary));
    }

    let declared = sql.as_deref().and_then(parse_table).unwrap_or_default();
    let mut constraints: Vec<ConstraintInfo> = constraints
        .into_iter()
        .map(|(kind, columns)| {
            let name = declared_name(&declared, kind, &columns);
            ConstraintInfo::new(rel.clone(), name, kind, columns, None)
        })
        .collect();
    for check in declared.iter().filter(|d| d.kind == ConstraintKind::Check) {
        constraints.push(ConstraintInfo::new(
            rel.clone(),
            check.name.clone(),
            ConstraintKind::Check,
            check.columns.clone(),
            None,
        ));
    }

    let triggers: Vec<TriggerInfo> = c
        .prepare(&format!(
            "SELECT name FROM {catalog} WHERE type = 'trigger' AND tbl_name = ?1 COLLATE NOCASE"
        ))?
        .query_map([&real], |r| r.get::<_, String>(0))?
        .chain(temp_triggers(c, schema, &real)?.into_iter().map(Ok))
        .map(|n| n.map(|n| TriggerInfo::new(rel.clone(), n, TriggerState::Enabled)))
        .collect::<rusqlite::Result<_>>()?;
    Ok((real, indexes, constraints, triggers, declared))
}

/// The TEMP triggers that fire on `schema`.`real`: temp's catalog holds them whatever
/// table they are on, so each one's ON clause, or the name lookup order, picks the table.
fn temp_triggers(c: &Connection, schema: &str, real: &str) -> rusqlite::Result<Vec<String>> {
    let found: Vec<(String, Option<String>)> = c
        .prepare(
            "SELECT name, sql FROM temp.sqlite_schema \
             WHERE type = 'trigger' AND tbl_name = ?1 COLLATE NOCASE",
        )?
        .query_map([real], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut names = Vec::new();
    for (name, sql) in found {
        let here = match sql.as_deref().and_then(trigger_schema) {
            Some(Some(on)) => on.eq_ignore_ascii_case(schema),
            _ => first_holder(c, real)?.is_some_and(|s| s.eq_ignore_ascii_case(schema)),
        };
        if here {
            names.push(name);
        }
    }
    Ok(names)
}

/// The schema an unqualified `real` names: temp first, then main and the attached ones.
fn first_holder(c: &Connection, real: &str) -> rusqlite::Result<Option<String>> {
    let mut order: Vec<String> = c
        .prepare("SELECT name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    order.insert(0, "temp".to_owned());
    for schema in order {
        let catalog = format!("{}.sqlite_schema", quote_ident(Engine::Sqlite, &schema));
        let has: Option<i64> = c
            .query_row(
                &format!(
                    "SELECT 1 FROM {catalog} \
                     WHERE type IN ('table', 'view') AND name = ?1 COLLATE NOCASE"
                ),
                [real],
                |r| r.get(0),
            )
            .optional()?;
        if has.is_some() {
            return Ok(Some(schema));
        }
    }
    Ok(None)
}

/// The schema a CREATE TRIGGER's ON clause names (None inside: unqualified); None when
/// sqlite3-parser rejects the text.
fn trigger_schema(sql: &str) -> Option<Option<String>> {
    let bump = Bump::new();
    let Ok(Some(Cmd::Stmt(Stmt::CreateTrigger { tbl_name, .. }))) =
        Parser::new(&bump, sql.as_bytes()).next()
    else {
        return None;
    };
    Some(tbl_name.db_name.as_ref().map(unquote))
}

/// The declared name of the key of `kind` over `columns` (the primary key by kind alone).
/// Two keys of one kind over the same columns both take the first matching declared name.
fn declared_name(
    declared: &[Declared],
    kind: ConstraintKind,
    columns: &[String],
) -> Option<String> {
    declared
        .iter()
        .find(|d| {
            d.kind == kind
                && (kind == ConstraintKind::PrimaryKey
                    || (d.columns.len() == columns.len()
                        && d.columns
                            .iter()
                            .zip(columns)
                            .all(|(a, b)| a.eq_ignore_ascii_case(b))))
        })
        .and_then(|d| d.name.clone())
}

/// The listing order of constraint kinds: primary, unique, foreign, exclusion, check.
fn rank(kind: ConstraintKind) -> u8 {
    match kind {
        ConstraintKind::PrimaryKey => 0,
        ConstraintKind::Unique => 1,
        ConstraintKind::ForeignKey => 2,
        ConstraintKind::Exclusion => 3,
        ConstraintKind::Check => 4,
        ConstraintKind::Other => 5,
    }
}

/// The PRIMARY KEY, UNIQUE, FOREIGN KEY and CHECK clauses of `sql`, column ones first in
/// column order, then table ones; None when sqlite3-parser rejects the text.
fn parse_table(sql: &str) -> Option<Vec<Declared>> {
    let bump = Bump::new();
    let Ok(Some(Cmd::Stmt(Stmt::CreateTable {
        body:
            CreateTableBody::ColumnsAndConstraints {
                columns,
                constraints,
                ..
            },
        ..
    }))) = Parser::new(&bump, sql.as_bytes()).next()
    else {
        return None;
    };
    let declare = |name: &Option<Name>, kind, columns| Declared {
        name: name.as_ref().map(unquote),
        kind,
        columns,
    };
    let mut declared = Vec::new();
    for column in columns.iter() {
        let own = vec![unquote(&column.col_name)];
        for c in column.constraints {
            let kind = match c.constraint {
                ColumnConstraint::PrimaryKey { .. } => ConstraintKind::PrimaryKey,
                ColumnConstraint::Unique(_) => ConstraintKind::Unique,
                ColumnConstraint::Check(_) => ConstraintKind::Check,
                ColumnConstraint::ForeignKey { .. } => ConstraintKind::ForeignKey,
                ColumnConstraint::NotNull { .. }
                | ColumnConstraint::Default(_)
                | ColumnConstraint::Defer(_)
                | ColumnConstraint::Collate { .. }
                | ColumnConstraint::Generated { .. } => continue,
            };
            declared.push(declare(&c.name, kind, own.clone()));
        }
    }
    for t in constraints.into_iter().flatten() {
        let (kind, columns) = match &t.constraint {
            TableConstraint::PrimaryKey { columns, .. } => {
                (ConstraintKind::PrimaryKey, sorted(columns))
            }
            TableConstraint::Unique { columns, .. } => (ConstraintKind::Unique, sorted(columns)),
            TableConstraint::Check(..) => (ConstraintKind::Check, Vec::new()),
            TableConstraint::ForeignKey { columns, .. } => (
                ConstraintKind::ForeignKey,
                columns.iter().map(|c| unquote(&c.col_name)).collect(),
            ),
        };
        declared.push(declare(&t.name, kind, columns));
    }
    Some(declared)
}

/// Key column names of a table PRIMARY KEY or UNIQUE, seen through any COLLATE.
fn sorted(columns: &[SortedColumn]) -> Vec<String> {
    columns
        .iter()
        .map(|c| {
            let mut expr = &c.expr;
            while let Expr::Collate(inner, _) = expr {
                expr = inner;
            }
            #[allow(
                clippy::wildcard_enum_match_arm,
                reason = "Expr has too many variants to list; the rest share one arm"
            )]
            match expr {
                Expr::Id(id) => unquote(&Name(id.0)),
                Expr::Name(name) => unquote(name),
                _ => "(expression)".to_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: Option<&str>, kind: ConstraintKind, columns: &[&str]) -> Declared {
        Declared {
            name: name.map(str::to_owned),
            kind,
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    #[test]
    fn column_and_table_clauses_are_declared_with_their_names() {
        let sql = r#"CREATE TABLE t(id INTEGER PRIMARY KEY, "E mail" TEXT CONSTRAINT once UNIQUE,
            qty INT CONSTRAINT qty_positive CHECK (qty > 0), a INT REFERENCES p, b TEXT,
            CONSTRAINT t_ab FOREIGN KEY (a, b) REFERENCES p(a, b), UNIQUE ([a], "B" COLLATE NOCASE),
            CHECK (a < 100))"#;
        assert_eq!(
            parse_table(sql),
            Some(vec![
                d(None, ConstraintKind::PrimaryKey, &["id"]),
                d(Some("once"), ConstraintKind::Unique, &["E mail"]),
                d(Some("qty_positive"), ConstraintKind::Check, &["qty"]),
                d(None, ConstraintKind::ForeignKey, &["a"]),
                d(Some("t_ab"), ConstraintKind::ForeignKey, &["a", "b"]),
                d(None, ConstraintKind::Unique, &["a", "B"]),
                d(None, ConstraintKind::Check, &[]),
            ])
        );
    }

    #[test]
    fn text_the_parser_rejects_declares_nothing() {
        assert_eq!(parse_table("CREATE TABLE odd("), None);
        assert_eq!(parse_table("CREATE VIEW v AS SELECT 1"), None);
    }
}
