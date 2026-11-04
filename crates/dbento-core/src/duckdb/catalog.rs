//! Dbento's catalog SQL for DuckDB, run on the session's own connection.
//!
//! Every read covers the current database only. Its `main` schema reports `internal`, so
//! schemas are never filtered on that flag, and names are bound, never quoted in.

use ::duckdb::Connection;

use super::DuckDriver;
use crate::introspect::{
    ColumnMeta, RELATION_CAP, Relation, RelationKind, RelationList, SchemaInfo, capped,
};
use crate::objects::{
    DEFINITION_CAP, Definition, DefinitionError, ObjectRef, definition_error, finish,
};

/// The current database's schemas, by name.
pub(crate) fn schemas(c: &Connection) -> ::duckdb::Result<Vec<SchemaInfo>> {
    c.prepare(
        "SELECT schema_name FROM duckdb_schemas() \
         WHERE database_name = current_database() ORDER BY schema_name",
    )?
    .query_map([], |r| Ok(SchemaInfo::new(r.get::<_, String>(0)?)))?
    .collect()
}

/// The tables and user views of `schema`, matched case-insensitively, by name; one row past
/// `RELATION_CAP` is asked for so `capped` can mark the list truncated.
pub(crate) fn relations(c: &Connection, schema: &str) -> ::duckdb::Result<RelationList> {
    let limit = i64::try_from(RELATION_CAP + 1).unwrap_or(i64::MAX);
    let items = c
        .prepare(
            "SELECT name, is_view FROM ( \
               SELECT table_name AS name, false AS is_view FROM duckdb_tables() \
               WHERE database_name = current_database() AND lower(schema_name) = lower($1) \
               UNION ALL \
               SELECT view_name, true FROM duckdb_views() \
               WHERE database_name = current_database() AND NOT internal \
                 AND lower(schema_name) = lower($1) \
             ) ORDER BY name LIMIT $2",
        )?
        .query_map(::duckdb::params![schema, limit], |r| {
            let kind = if r.get::<_, bool>(1)? {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            Ok(Relation::new(r.get::<_, String>(0)?, kind))
        })?
        .collect::<::duckdb::Result<Vec<_>>>()?;
    Ok(capped(items))
}

/// The columns of `schema.name` in declared order; an unknown relation has none. The key
/// position is the column's 1-based place in the relation's `PRIMARY KEY` constraint.
pub(crate) fn columns(
    c: &Connection,
    schema: &str,
    name: &str,
) -> ::duckdb::Result<Vec<ColumnMeta>> {
    // DuckDB also lists NOT NULL constraints, so only the primary key row supplies positions,
    // and an absent name's NULL position means "not a key column".
    c.prepare(
        "SELECT c.column_name, c.data_type, c.is_nullable, \
                list_position(k.keys, c.column_name), c.column_default \
         FROM duckdb_columns() c \
         LEFT JOIN ( \
           SELECT schema_name, table_name, constraint_column_names AS keys \
           FROM duckdb_constraints() \
           WHERE database_name = current_database() AND constraint_type = 'PRIMARY KEY' \
         ) k ON k.schema_name = c.schema_name AND k.table_name = c.table_name \
         WHERE c.database_name = current_database() \
           AND lower(c.schema_name) = lower($1) AND lower(c.table_name) = lower($2) \
         ORDER BY c.column_index",
    )?
    .query_map(::duckdb::params![schema, name], |r| {
        let key = r
            .get::<_, Option<i64>>(3)?
            .and_then(|p| u16::try_from(p).ok())
            .filter(|&p| p > 0);
        Ok(ColumnMeta::new(
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            key,
            r.get::<_, Option<String>>(4)?,
        ))
    })?
    .collect()
}

/// The `sql` DuckDB stores for a table, else for a user view, cut one char past `cap` so the
/// caller can tell an over-long text; None when no such relation exists.
fn relation_sql(
    c: &Connection,
    schema: &str,
    name: &str,
    cap: i64,
) -> ::duckdb::Result<Option<String>> {
    let mut stmt = c.prepare(
        "SELECT left(sql, $3) FROM ( \
           SELECT sql, 0 AS pick FROM duckdb_tables() \
           WHERE database_name = current_database() \
             AND lower(schema_name) = lower($1) AND lower(table_name) = lower($2) \
           UNION ALL \
           SELECT sql, 1 FROM duckdb_views() \
           WHERE database_name = current_database() AND NOT internal \
             AND lower(schema_name) = lower($1) AND lower(view_name) = lower($2) \
         ) ORDER BY pick LIMIT 1",
    )?;
    let mut rows = stmt.query_map(::duckdb::params![schema, name, cap], |r| {
        r.get::<_, Option<String>>(0)
    })?;
    Ok(rows.next().transpose()?.flatten())
}

/// The stored definition of a relation; every other object kind is `Unsupported`.
pub(crate) async fn definition(
    d: &mut DuckDriver,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    let ObjectRef::Relation(rel) = object else {
        return Err(DefinitionError::Unsupported {
            what: object.label(),
        });
    };
    let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
    let cap = i64::try_from(DEFINITION_CAP + 1).unwrap_or(i64::MAX);
    let text = d
        .introspect(move |c| relation_sql(c, &schema, &name, cap))
        .await
        .map_err(|e| definition_error(e, object))?;
    finish(text, DEFINITION_CAP, false, object)
}
