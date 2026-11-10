//! MySQL catalog queries: Dbento's own text over the text protocol, every cell read as text.
//! Names are inlined as literals and compared as the server compares them; order is byte order.

use std::collections::HashMap;

use mysql_async::{Row, Value};

use super::MysqlDriver;
use crate::fkeys::{FkDirection, ForeignKey};
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped, quote_ident, quote_literal,
};
use crate::objects::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, IndexInfo,
    ObjectRef, RelationObjects, RoutineInfo, RoutineKind, SchemaObjects, TriggerInfo, TriggerState,
    definition_error, finish,
};
use crate::session::{ExecuteError, Flavor};
use crate::value::{ColumnInfo, ColumnSource};
use crate::workspace::Engine;

/// The schemas the server itself owns, listed after the user's in this order.
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// `column` equal to `value` as the server resolves names: exactly, even where MariaDB's
/// information_schema collation ignores case, unless the server folds names to lower case.
pub(crate) fn name_eq(column: &str, value: &str, fold: bool) -> String {
    let lit = quote_literal(Engine::Mysql, value);
    if fold {
        format!("LOWER({column}) = LOWER({lit})")
    } else {
        format!("({column} = {lit} AND CAST({column} AS BINARY) = CAST({lit} AS BINARY))")
    }
}

/// Cell `ix` as text; NULL is None; a missing column or non-UTF-8 bytes is `Unexpected`.
pub(crate) fn cell(row: &Row, ix: usize) -> Result<Option<String>, IntrospectError> {
    let unexpected = |what: &str| IntrospectError::Unexpected {
        message: format!("catalog column {ix} {what}"),
    };
    match row.as_ref(ix) {
        None => Err(unexpected("is missing")),
        Some(Value::NULL) => Ok(None),
        Some(Value::Bytes(b)) => String::from_utf8(b.clone())
            .map(Some)
            .map_err(|_| unexpected("is not UTF-8")),
        Some(_) => Err(unexpected("is not text")),
    }
}

/// Cell `ix` as text that must not be NULL.
fn text(row: &Row, ix: usize) -> Result<String, IntrospectError> {
    cell(row, ix)?.ok_or_else(|| IntrospectError::Unexpected {
        message: format!("catalog column {ix} is NULL"),
    })
}

/// Every schema: the user's in byte order, then the server's own in a fixed order.
pub(crate) async fn schemas(d: &mut MysqlDriver) -> Result<Vec<SchemaInfo>, IntrospectError> {
    let rows = d
        .catalog("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA")
        .await?;
    let mut names = rows
        .iter()
        .map(|r| text(r, 0))
        .collect::<Result<Vec<_>, _>>()?;
    let rank = |n: &str| SYSTEM_SCHEMAS.iter().position(|s| *s == n);
    names.sort_by(|a, b| (rank(a), a).cmp(&(rank(b), b)));
    Ok(names.into_iter().map(SchemaInfo::new).collect())
}

/// The tables and views of `schema` in byte order, at most RELATION_CAP of them.
pub(crate) async fn relations(
    d: &mut MysqlDriver,
    schema: &str,
) -> Result<RelationList, IntrospectError> {
    let sql = format!(
        "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE {} \
         ORDER BY CAST(TABLE_NAME AS BINARY) LIMIT {}",
        name_eq("TABLE_SCHEMA", schema, d.fold_names()),
        RELATION_CAP + 1
    );
    let rows = d.catalog(&sql).await?;
    let items = rows
        .iter()
        .map(|r| {
            let kind = match cell(r, 1)?.as_deref() {
                Some("BASE TABLE") => RelationKind::Table,
                Some("VIEW" | "SYSTEM VIEW") => RelationKind::View,
                _ => RelationKind::Other,
            };
            Ok(Relation::new(text(r, 0)?, kind))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    Ok(capped(items))
}

/// `COLUMN_TYPE` as MySQL 8 prints it: integer and `year` display widths dropped, except
/// with `zerofill` and for a signed `tinyint(1)`, where the width carries meaning.
pub(crate) fn column_type(raw: &str) -> String {
    const WIDTHED: [&str; 6] = ["tinyint", "smallint", "mediumint", "int", "bigint", "year"];
    let Some((base, rest)) = raw.split_once('(') else {
        return raw.to_owned();
    };
    let Some((width, after)) = rest.split_once(')') else {
        return raw.to_owned();
    };
    let digits = !width.is_empty() && width.bytes().all(|b| b.is_ascii_digit());
    let meaningful =
        after.contains("zerofill") || (base == "tinyint" && width == "1" && after.is_empty());
    if !WIDTHED.contains(&base) || !digits || meaningful {
        return raw.to_owned();
    }
    format!("{base}{after}")
}

/// A column's default as expression text. MySQL prints a literal default unquoted, so it is
/// quoted unless the type reads as a bare literal or `extra` marks an expression; MariaDB
/// already prints the expression, where a bare `NULL` means no default.
pub(crate) fn column_default(
    flavor: Flavor,
    raw: Option<&str>,
    extra: &str,
    column_type: &str,
) -> Option<String> {
    const BARE: [&str; 10] = [
        "tinyint",
        "smallint",
        "mediumint",
        "int",
        "bigint",
        "decimal",
        "float",
        "double",
        "bit",
        "year",
    ];
    let raw = raw?;
    if flavor == Flavor::Mariadb {
        return (raw != "NULL").then(|| raw.to_owned());
    }
    let base = column_type.split(['(', ' ']).next().unwrap_or(column_type);
    if extra.contains("DEFAULT_GENERATED") || BARE.contains(&base) {
        Some(raw.to_owned())
    } else {
        Some(quote_literal(Engine::Mysql, raw))
    }
}

/// The columns of `rel` in ordinal order with their primary-key positions; empty when the
/// relation does not exist. Keys are a second query joined here by column name, because a SQL
/// join on `TABLE_NAME` would ignore case on MariaDB.
pub(crate) async fn columns(
    d: &mut MysqlDriver,
    rel: &RelationRef,
) -> Result<Vec<ColumnMeta>, IntrospectError> {
    let fold = d.fold_names();
    let filter = rel_filter(rel, "TABLE_SCHEMA", "TABLE_NAME", fold);
    let cols = d
        .catalog(&format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA \
             FROM information_schema.COLUMNS WHERE {filter} ORDER BY ORDINAL_POSITION"
        ))
        .await?;
    let keys = d
        .catalog(&format!(
            "SELECT COLUMN_NAME, ORDINAL_POSITION FROM information_schema.KEY_COLUMN_USAGE \
             WHERE {filter} AND CONSTRAINT_NAME = 'PRIMARY'"
        ))
        .await?;
    let mut positions = HashMap::new();
    for r in &keys {
        let pos = text(r, 1)?;
        let pos = pos
            .parse::<u16>()
            .map_err(|_| IntrospectError::Unexpected {
                message: format!("primary key position {pos:?} is not a number"),
            })?;
        positions.insert(text(r, 0)?, pos);
    }
    let flavor = d.server().flavor;
    cols.iter()
        .map(|r| {
            let name = text(r, 0)?;
            let raw_type = text(r, 1)?;
            let default = column_default(
                flavor,
                cell(r, 3)?.as_deref(),
                &cell(r, 4)?.unwrap_or_default(),
                &raw_type,
            );
            let pk = positions.get(&name).copied();
            Ok(ColumnMeta::new(
                name,
                column_type(&raw_type),
                text(r, 2)? == "YES",
                pk,
                default,
            ))
        })
        .collect()
}

/// `rel`'s schema and name matched against two catalog columns, as the server resolves names.
fn rel_filter(rel: &RelationRef, schema_col: &str, name_col: &str, fold: bool) -> String {
    format!(
        "{} AND {}",
        name_eq(schema_col, rel.schema(), fold),
        name_eq(name_col, rel.name(), fold)
    )
}

/// Indexes by name, keys and checks by kind (primary, unique, foreign, check) then name,
/// triggers by name; empty lists for an unknown relation.
pub(crate) async fn relation_objects(
    d: &mut MysqlDriver,
    rel: &RelationRef,
) -> Result<RelationObjects, IntrospectError> {
    let fold = d.fold_names();
    let filter = rel_filter(rel, "TABLE_SCHEMA", "TABLE_NAME", fold);
    // Only MySQL has functional key parts, whose text sits in EXPRESSION.
    let expression = if d.server().flavor == Flavor::Mysql {
        "EXPRESSION"
    } else {
        "NULL"
    };
    let stats = d
        .catalog(&format!(
            "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE, {expression} \
             FROM information_schema.STATISTICS WHERE {filter} \
             ORDER BY CAST(INDEX_NAME AS BINARY), SEQ_IN_INDEX"
        ))
        .await?;
    let mut indexes: Vec<IndexInfo> = Vec::new();
    let mut current: Option<(String, bool, Option<String>, Vec<String>)> = None;
    for r in &stats {
        let name = text(r, 0)?;
        let part = match cell(r, 2)? {
            Some(c) => c,
            None => text(r, 4)?,
        };
        match &mut current {
            Some((n, _, _, cols)) if *n == name => cols.push(part),
            _ => {
                if let Some(done) = current.take() {
                    indexes.push(index_info(rel, done));
                }
                let unique = text(r, 1)? == "0";
                let method = cell(r, 3)?.map(|m| m.to_lowercase());
                current = Some((name, unique, method, vec![part]));
            }
        }
    }
    indexes.extend(current.map(|done| index_info(rel, done)));

    let kinds = d
        .catalog(&format!(
            "SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE FROM information_schema.TABLE_CONSTRAINTS \
             WHERE {filter}"
        ))
        .await?;
    let usage = d
        .catalog(&format!(
            "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME \
             FROM information_schema.KEY_COLUMN_USAGE WHERE {filter} ORDER BY ORDINAL_POSITION"
        ))
        .await?;
    // A foreign key and a unique key may share a name, so key columns are told apart by
    // whether the row references another table.
    let mut key_columns: HashMap<(String, bool), (Vec<String>, Option<RelationRef>)> =
        HashMap::new();
    for r in &usage {
        let referenced = match (cell(r, 2)?, cell(r, 3)?) {
            (Some(schema), Some(name)) => Some(RelationRef::new(schema, name)),
            _ => None,
        };
        let entry = key_columns
            .entry((text(r, 0)?, referenced.is_some()))
            .or_default();
        entry.0.push(text(r, 1)?);
        entry.1 = referenced;
    }
    let mut constraints = kinds
        .iter()
        .map(|r| {
            let name = text(r, 0)?;
            let kind = match text(r, 1)?.as_str() {
                "PRIMARY KEY" => ConstraintKind::PrimaryKey,
                "UNIQUE" => ConstraintKind::Unique,
                "FOREIGN KEY" => ConstraintKind::ForeignKey,
                "CHECK" => ConstraintKind::Check,
                _ => ConstraintKind::Other,
            };
            let (columns, referenced) = match kind {
                ConstraintKind::Check => (Vec::new(), None),
                ConstraintKind::PrimaryKey
                | ConstraintKind::Unique
                | ConstraintKind::ForeignKey
                | ConstraintKind::Exclusion
                | ConstraintKind::Other => key_columns
                    .remove(&(name.clone(), kind == ConstraintKind::ForeignKey))
                    .unwrap_or_default(),
            };
            Ok(ConstraintInfo::new(
                rel.clone(),
                Some(name),
                kind,
                columns,
                referenced,
            ))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    let rank = |k: ConstraintKind| match k {
        ConstraintKind::PrimaryKey => 0,
        ConstraintKind::Unique => 1,
        ConstraintKind::ForeignKey => 2,
        ConstraintKind::Check => 3,
        ConstraintKind::Exclusion | ConstraintKind::Other => 4,
    };
    constraints.sort_by(|a, b| (rank(a.kind()), a.name()).cmp(&(rank(b.kind()), b.name())));

    let triggers = d
        .catalog(&format!(
            "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS WHERE {} \
             ORDER BY CAST(TRIGGER_NAME AS BINARY)",
            rel_filter(rel, "EVENT_OBJECT_SCHEMA", "EVENT_OBJECT_TABLE", fold)
        ))
        .await?
        .iter()
        .map(|r| {
            Ok(TriggerInfo::new(
                rel.clone(),
                text(r, 0)?,
                TriggerState::Enabled,
            ))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    Ok(RelationObjects::new(indexes, constraints, triggers))
}

/// One index from its grouped STATISTICS rows: name, uniqueness, method and key parts.
fn index_info(
    rel: &RelationRef,
    (name, unique, method, columns): (String, bool, Option<String>, Vec<String>),
) -> IndexInfo {
    let primary = name == "PRIMARY";
    IndexInfo::new(rel.clone(), name, columns, unique, primary, false, method)
}

/// Keys of `rel` in both directions, unsorted; a self-reference is listed both ways.
pub(crate) async fn foreign_keys(
    d: &mut MysqlDriver,
    rel: &RelationRef,
) -> Result<Vec<ForeignKey>, IntrospectError> {
    let fold = d.fold_names();
    let mut keys = Vec::new();
    for (direction, schema_col, name_col) in [
        (FkDirection::Outgoing, "TABLE_SCHEMA", "TABLE_NAME"),
        (
            FkDirection::Incoming,
            "REFERENCED_TABLE_SCHEMA",
            "REFERENCED_TABLE_NAME",
        ),
    ] {
        // Ordered so each key's rows are adjacent and in key order.
        let rows = d
            .catalog(&format!(
                "SELECT TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, \
                 REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE REFERENCED_TABLE_NAME IS NOT NULL AND {} \
                 ORDER BY CAST(TABLE_SCHEMA AS BINARY), CAST(TABLE_NAME AS BINARY), \
                 CAST(CONSTRAINT_NAME AS BINARY), ORDINAL_POSITION",
                rel_filter(rel, schema_col, name_col, fold)
            ))
            .await?;
        let rows = rows
            .iter()
            .map(|r| (0..7).map(|ix| text(r, ix)).collect::<Result<Vec<_>, _>>())
            .collect::<Result<Vec<_>, _>>()?;
        for group in rows.chunk_by(|a, b| a[..3] == b[..3]) {
            let first = &group[0];
            keys.push(ForeignKey::new(
                Some(first[2].clone()),
                RelationRef::new(first[0].as_str(), first[1].as_str()),
                group.iter().map(|r| r[3].clone()).collect(),
                RelationRef::new(first[4].as_str(), first[5].as_str()),
                group.iter().map(|r| r[6].clone()).collect(),
                direction,
            ));
        }
    }
    Ok(keys)
}

/// For each column, its `ColumnSource::Mysql` column when schema and table name `rel`
/// (ASCII case folded only when `fold`); None otherwise.
pub(crate) fn source_columns(
    rel: &RelationRef,
    columns: &[ColumnInfo],
    fold: bool,
) -> Vec<Option<String>> {
    let same = |a: &str, b: &str| {
        if fold {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    columns
        .iter()
        .map(|c| match c.source() {
            Some(ColumnSource::Mysql {
                schema,
                table,
                column,
            }) if same(schema, rel.schema()) && same(table, rel.name()) => Some(column.clone()),
            _ => None,
        })
        .collect()
}

/// The routines of `schema` by name then arguments, at most RELATION_CAP + 1 of them, each
/// with `name type` arguments (the mode only when it is not `IN`) and a function's result.
async fn routines(d: &mut MysqlDriver, schema: &str) -> Result<Vec<RoutineInfo>, IntrospectError> {
    let fold = d.fold_names();
    let listed = d
        .catalog(&format!(
            "SELECT ROUTINE_NAME, ROUTINE_TYPE, DTD_IDENTIFIER FROM information_schema.ROUTINES \
             WHERE ROUTINE_TYPE IN ('FUNCTION', 'PROCEDURE') AND {} \
             ORDER BY CAST(ROUTINE_NAME AS BINARY) LIMIT {}",
            name_eq("ROUTINE_SCHEMA", schema, fold),
            RELATION_CAP + 1
        ))
        .await?;
    let params = d
        .catalog(&format!(
            "SELECT SPECIFIC_NAME, ROUTINE_TYPE, PARAMETER_MODE, PARAMETER_NAME, DTD_IDENTIFIER \
             FROM information_schema.PARAMETERS WHERE ORDINAL_POSITION > 0 AND {} \
             ORDER BY ORDINAL_POSITION",
            name_eq("SPECIFIC_SCHEMA", schema, fold)
        ))
        .await?;
    let mut arguments: HashMap<(String, String), Vec<String>> = HashMap::new();
    for r in &params {
        let mode = match cell(r, 2)? {
            Some(m) if m != "IN" => format!("{m} "),
            _ => String::new(),
        };
        let name = cell(r, 3)?.unwrap_or_default();
        let ty = column_type(&text(r, 4)?);
        arguments
            .entry((text(r, 0)?, text(r, 1)?))
            .or_default()
            .push(format!("{mode}{name} {ty}"));
    }
    let mut out = listed
        .iter()
        .map(|r| {
            let name = text(r, 0)?;
            let kind_text = text(r, 1)?;
            let (kind, returns) = if kind_text == "FUNCTION" {
                (RoutineKind::Function, Some(column_type(&text(r, 2)?)))
            } else {
                (RoutineKind::Procedure, None)
            };
            let args = arguments
                .remove(&(name.clone(), kind_text))
                .unwrap_or_default()
                .join(", ");
            Ok(RoutineInfo::new(schema, name, kind, args, returns))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    // The server already sorted by name; a stable sort adds the arguments as a tiebreak.
    out.sort_by(|a, b| (a.name(), a.arguments()).cmp(&(b.name(), b.arguments())));
    Ok(out)
}

/// No types; routines by name then arguments, at most RELATION_CAP; empty for an unknown schema.
pub(crate) async fn schema_objects(
    d: &mut MysqlDriver,
    schema: &str,
) -> Result<SchemaObjects, IntrospectError> {
    let mut found = routines(d, schema).await?;
    let truncated = found.len() > RELATION_CAP;
    found.truncate(RELATION_CAP);
    Ok(SchemaObjects::new(Vec::new(), found, truncated))
}

/// The server's CREATE text of `object`, at most DEFINITION_CAP bytes, never composed. An index
/// or constraint answers with its table's text; a routine is the first listed match by name and
/// arguments, a function before a procedure.
pub(crate) async fn definition(
    d: &mut MysqlDriver,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    let missing = || DefinitionError::Missing {
        object: object.label(),
    };
    let q = |s: &str| quote_ident(Engine::Mysql, s);
    let (sql, column) = match object {
        ObjectRef::Relation(rel)
        | ObjectRef::Index { rel, .. }
        | ObjectRef::Constraint { rel, .. } => (
            format!("SHOW CREATE TABLE {}.{}", q(rel.schema()), q(rel.name())),
            1,
        ),
        ObjectRef::Trigger { rel, name } => (
            format!("SHOW CREATE TRIGGER {}.{}", q(rel.schema()), q(name)),
            2,
        ),
        ObjectRef::Routine {
            schema,
            name,
            arguments,
        } => {
            let listed = routines(d, schema)
                .await
                .map_err(|e| definition_error(e, object))?;
            let matching = |kind| {
                listed.iter().any(|r| {
                    r.kind() == kind && r.name() == name && r.arguments() == arguments.as_str()
                })
            };
            let word = if matching(RoutineKind::Function) {
                "FUNCTION"
            } else if matching(RoutineKind::Procedure) {
                "PROCEDURE"
            } else {
                return Err(missing());
            };
            (format!("SHOW CREATE {word} {}.{}", q(schema), q(name)), 2)
        }
        ObjectRef::Type { .. } => return Err(missing()),
    };
    let rows = match d.catalog(&sql).await {
        Ok(rows) => rows,
        // No such table, database, trigger or routine: the object went away since it was listed.
        Err(IntrospectError::Execute(ExecuteError::Database { code, .. }))
            if matches!(code.as_str(), "1146" | "1049" | "1360" | "1305") =>
        {
            return Err(missing());
        }
        Err(e) => return Err(definition_error(e, object)),
    };
    let Some(row) = rows.first() else {
        return Err(missing());
    };
    match cell(row, column).map_err(|e| definition_error(e, object))? {
        Some(text) => finish(Some(text), DEFINITION_CAP, false, object),
        None => Err(DefinitionError::Unexpected {
            message: format!(
                "the server showed no text for {}; the account may lack the privilege to see it",
                object.label()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Flavor::{Mariadb, Mysql};

    #[test]
    fn display_widths_go_except_where_they_carry_meaning() {
        for (raw, want) in [
            ("int(11)", "int"),
            ("bigint(20) unsigned", "bigint unsigned"),
            ("tinyint(4)", "tinyint"),
            ("tinyint(1)", "tinyint(1)"),
            ("tinyint(1) unsigned", "tinyint unsigned"),
            ("mediumint(8) unsigned", "mediumint unsigned"),
            ("smallint(3)", "smallint"),
            ("year(4)", "year"),
            ("int(5) unsigned zerofill", "int(5) unsigned zerofill"),
            ("varchar(10)", "varchar(10)"),
            ("decimal(5,2)", "decimal(5,2)"),
            ("enum('int(1)')", "enum('int(1)')"),
            ("int", "int"),
        ] {
            assert_eq!(column_type(raw), want, "{raw}");
        }
    }

    #[test]
    fn defaults_become_the_expression_text_on_both_flavors() {
        for (flavor, raw, extra, ty, want) in [
            (Mysql, Some("it's"), "", "varchar(10)", Some("'it''s'")),
            (Mariadb, Some("'it''s'"), "", "varchar(10)", Some("'it''s'")),
            (Mysql, Some("NULL"), "", "varchar(5)", Some("'NULL'")),
            (Mariadb, Some("'NULL'"), "", "varchar(5)", Some("'NULL'")),
            (Mysql, None, "", "varchar(5)", None),
            (Mariadb, Some("NULL"), "", "varchar(5)", None),
            (Mariadb, None, "", "varchar(10)", None),
            (Mysql, Some(""), "", "varchar(5)", Some("''")),
            (Mysql, Some("0"), "", "tinyint(1)", Some("0")),
            (Mysql, Some("1.50"), "", "decimal(5,2)", Some("1.50")),
            (Mysql, Some("b'1'"), "", "bit(1)", Some("b'1'")),
            (Mysql, Some("2024"), "", "year", Some("2024")),
            (Mysql, Some("2024-01-02"), "", "date", Some("'2024-01-02'")),
            (Mysql, Some("x"), "", "enum('x','y')", Some("'x'")),
            (
                Mysql,
                Some("CURRENT_TIMESTAMP"),
                "DEFAULT_GENERATED",
                "timestamp",
                Some("CURRENT_TIMESTAMP"),
            ),
            (
                Mysql,
                Some("CURRENT_TIMESTAMP(3)"),
                "DEFAULT_GENERATED on update CURRENT_TIMESTAMP(3)",
                "datetime(3)",
                Some("CURRENT_TIMESTAMP(3)"),
            ),
            (
                Mariadb,
                Some("current_timestamp()"),
                "",
                "timestamp",
                Some("current_timestamp()"),
            ),
        ] {
            assert_eq!(
                column_default(flavor, raw, extra, ty).as_deref(),
                want,
                "{flavor:?} {raw:?} {ty}"
            );
        }
    }

    #[test]
    fn names_compare_exactly_unless_the_server_folds_them() {
        assert_eq!(
            name_eq("TABLE_NAME", "Pk", false),
            "(TABLE_NAME = 'Pk' AND CAST(TABLE_NAME AS BINARY) = CAST('Pk' AS BINARY))"
        );
        assert_eq!(
            name_eq("TABLE_NAME", "Pk", true),
            "LOWER(TABLE_NAME) = LOWER('Pk')"
        );
        assert_eq!(
            name_eq("c", "it's", false),
            "(c = 'it''s' AND CAST(c AS BINARY) = CAST('it''s' AS BINARY))"
        );
    }
}
