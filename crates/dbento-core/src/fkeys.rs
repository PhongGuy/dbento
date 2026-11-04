//! Foreign keys of one relation in both directions, read from the engine's catalog.

use rusqlite::{Connection, OptionalExtension};

use crate::introspect::{IntrospectError, RelationRef, quote_ident};
use crate::postgres::{PgDriver, cell};
use crate::sqlite::{SqliteDriver, known_schema};
use crate::workspace::Engine;

/// Which way a key points, seen from the relation it was read for.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FkDirection {
    /// The relation's own columns reference another relation.
    Outgoing,
    /// Another relation's columns reference this one.
    Incoming,
}

/// A foreign key: `table.columns` references `referenced.referenced_columns`, in key order.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignKey {
    name: Option<String>,
    table: RelationRef,
    columns: Vec<String>,
    referenced: RelationRef,
    referenced_columns: Vec<String>,
    direction: FkDirection,
}

impl ForeignKey {
    /// A key from both ends and its direction relative to the relation asked about.
    pub fn new(
        name: Option<String>,
        table: RelationRef,
        columns: Vec<String>,
        referenced: RelationRef,
        referenced_columns: Vec<String>,
        direction: FkDirection,
    ) -> Self {
        Self {
            name,
            table,
            columns,
            referenced,
            referenced_columns,
            direction,
        }
    }

    /// The constraint name; None where the engine keeps none (SQLite).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The referencing relation.
    pub fn table(&self) -> &RelationRef {
        &self.table
    }

    /// The referencing columns, in key order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The referenced relation.
    pub fn referenced(&self) -> &RelationRef {
        &self.referenced
    }

    /// The referenced columns, paired with `columns`.
    pub fn referenced_columns(&self) -> &[String] {
        &self.referenced_columns
    }

    /// Which way the key points from the relation it was read for.
    pub fn direction(&self) -> FkDirection {
        self.direction
    }

    /// `referenced` for an outgoing key, `table` for an incoming one.
    pub fn other(&self) -> &RelationRef {
        match self.direction {
            FkDirection::Outgoing => &self.referenced,
            FkDirection::Incoming => &self.table,
        }
    }
}

/// Puts keys in their listed order: outgoing first, then by the far relation, then columns.
pub(crate) fn sort_keys(keys: &mut [ForeignKey]) {
    keys.sort_by(|a, b| {
        (a.direction, a.other(), &a.columns).cmp(&(b.direction, b.other(), &b.columns))
    });
}

/// One pragma row: key id, referenced table as written, from column, to column (None = PK).
type PragmaRow = (i64, String, String, Option<String>);

/// Keys of `rel` on SQLite in both directions, unsorted; empty for an unknown relation.
pub(crate) async fn sqlite_foreign_keys(
    d: &mut SqliteDriver,
    rel: &RelationRef,
) -> Result<Vec<ForeignKey>, IntrospectError> {
    let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
    d.introspect(move |c| {
        // The schema cannot be bound, so an unknown one is answered before it is quoted in.
        if !known_schema(c, &schema)? {
            return Ok(Vec::new());
        }
        let Some(name) = real_table(c, &schema, &name)? else {
            return Ok(Vec::new());
        };
        let at = |n: &str| RelationRef::new(schema.as_str(), n);
        let mut keys = Vec::new();

        let outgoing: Vec<PragmaRow> = c
            .prepare(
                "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1, ?2) \
                 ORDER BY id, seq",
            )?
            .query_map([&name, &schema], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for group in outgoing.chunk_by(|a, b| a.0 == b.0) {
            // A target named only by table resolves to its primary key; without one the key
            // names no columns and is dropped.
            let written = &group[0].1;
            let real = real_table(c, &schema, written)?;
            let target = real.clone().unwrap_or_else(|| written.clone());
            let Some(to) = target_columns(c, &schema, real.as_deref(), group)? else {
                continue;
            };
            let from = group.iter().map(|r| r.2.clone()).collect();
            keys.push(ForeignKey::new(
                None,
                at(&name),
                from,
                at(&target),
                to,
                FkDirection::Outgoing,
            ));
        }

        let sql = format!(
            "SELECT m.name, f.id, f.\"table\", f.\"from\", f.\"to\" \
             FROM {}.sqlite_schema m JOIN pragma_foreign_key_list(m.name, ?2) f \
             WHERE m.type = 'table' AND f.\"table\" = ?1 COLLATE NOCASE \
             ORDER BY m.name, f.id, f.seq",
            quote_ident(Engine::Sqlite, &schema)
        );
        let incoming: Vec<(String, PragmaRow)> = c
            .prepare(&sql)?
            .query_map([&name, &schema], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for group in incoming.chunk_by(|a, b| a.0 == b.0 && a.1.0 == b.1.0) {
            let rows: Vec<PragmaRow> = group.iter().map(|g| g.1.clone()).collect();
            let Some(to) = target_columns(c, &schema, Some(&name), &rows)? else {
                continue;
            };
            let from = rows.iter().map(|r| r.2.clone()).collect();
            keys.push(ForeignKey::new(
                None,
                at(&group[0].0),
                from,
                at(&name),
                to,
                FkDirection::Incoming,
            ));
        }
        Ok(keys)
    })
    .await
}

/// The real name of the table `name` matches case-insensitively, if any.
fn real_table(c: &Connection, schema: &str, name: &str) -> rusqlite::Result<Option<String>> {
    let sql = format!(
        "SELECT name FROM {}.sqlite_schema WHERE type = 'table' AND name = ?1 COLLATE NOCASE",
        quote_ident(Engine::Sqlite, schema)
    );
    c.query_row(&sql, [name], |r| r.get(0)).optional()
}

/// The referenced columns of one key: as written, or the target's primary key when the key
/// names none. None when they cannot be known (missing target or no primary key).
fn target_columns(
    c: &Connection,
    schema: &str,
    target: Option<&str>,
    rows: &[PragmaRow],
) -> rusqlite::Result<Option<Vec<String>>> {
    if let Some(to) = rows.iter().map(|r| r.3.clone()).collect::<Option<Vec<_>>>() {
        return Ok(Some(to));
    }
    let Some(target) = target else {
        return Ok(None);
    };
    let pk: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk")?
        .query_map([target, schema], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((!pk.is_empty()).then_some(pk))
}

/// Every key with `rel` at either end. Columns 8 and 9 say which side `rel` is on; a key cloned
/// onto a partition counts for a side only when that side differs from its parent's.
const PG_FOREIGN_KEYS_SQL: &str = "\
SELECT c.conname::text, sn.nspname::text, s.relname::text, tn.nspname::text, t.relname::text,
  ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY AS k(n, o)
        JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.n ORDER BY k.o),
  ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY AS k(n, o)
        JOIN pg_catalog.pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.n ORDER BY k.o),
  (sn.nspname = $1::text AND s.relname = $2::text AND (c.conparentid = 0 OR p.conrelid <> c.conrelid)),
  (tn.nspname = $1::text AND t.relname = $2::text AND (c.conparentid = 0 OR p.confrelid <> c.confrelid))
FROM pg_catalog.pg_constraint c
JOIN pg_catalog.pg_class s ON s.oid = c.conrelid
JOIN pg_catalog.pg_namespace sn ON sn.oid = s.relnamespace
JOIN pg_catalog.pg_class t ON t.oid = c.confrelid
JOIN pg_catalog.pg_namespace tn ON tn.oid = t.relnamespace
LEFT JOIN pg_catalog.pg_constraint p ON p.oid = c.conparentid
WHERE c.contype = 'f'
  AND ((sn.nspname = $1::text AND s.relname = $2::text) OR (tn.nspname = $1::text AND t.relname = $2::text))";

/// Keys of `rel` on Postgres in both directions, unsorted; a self-reference yields one key
/// per direction. Empty for an unknown relation.
pub(crate) async fn pg_foreign_keys(
    d: &mut PgDriver,
    rel: &RelationRef,
) -> Result<Vec<ForeignKey>, IntrospectError> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&rel.schema(), &rel.name()];
    let mut keys = Vec::new();
    for row in d.catalog(PG_FOREIGN_KEYS_SQL, &params).await? {
        let key = |direction| -> Result<ForeignKey, IntrospectError> {
            Ok(ForeignKey::new(
                Some(cell(&row, 0)?),
                RelationRef::new(cell::<String>(&row, 1)?, cell::<String>(&row, 2)?),
                cell(&row, 5)?,
                RelationRef::new(cell::<String>(&row, 3)?, cell::<String>(&row, 4)?),
                cell(&row, 6)?,
                direction,
            ))
        };
        if cell::<bool>(&row, 7)? {
            keys.push(key(FkDirection::Outgoing)?);
        }
        if cell::<bool>(&row, 8)? {
            keys.push(key(FkDirection::Incoming)?);
        }
    }
    Ok(keys)
}
