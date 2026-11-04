//! Maps result columns back to the columns of one relation through their recorded source.

use crate::introspect::{IntrospectError, RelationRef};
use crate::postgres::{PgDriver, cell};
use crate::value::{ColumnInfo, ColumnSource};

/// For each of `columns`, the column of `rel` it reads, when the sources carry names (SQLite)
/// or there are none; None when a Postgres or MySQL source needs the server to name it.
/// SQLite matches database and table names without regard to ASCII case.
pub fn named_source_columns(
    rel: &RelationRef,
    columns: &[ColumnInfo],
) -> Option<Vec<Option<String>>> {
    columns
        .iter()
        .map(|c| match c.source() {
            Some(ColumnSource::Sqlite {
                schema,
                table,
                column,
            }) => Some(
                (schema.eq_ignore_ascii_case(rel.schema())
                    && table.eq_ignore_ascii_case(rel.name()))
                .then(|| column.clone()),
            ),
            // MySQL name matching follows the server's case rules, which only it knows.
            Some(ColumnSource::Postgres { .. } | ColumnSource::Mysql { .. }) => None,
            None => Some(None),
        })
        .collect()
}

const PG_ATTRIBUTES_SQL: &str = "\
SELECT c.oid, a.attnum, a.attname::text
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid
WHERE n.nspname = $1::text AND c.relname = $2::text AND a.attnum = ANY($3::int2[])
  AND NOT a.attisdropped";

/// Postgres records OIDs, so one catalog query names the columns of `rel` the result reads;
/// a column of any other table, or none, maps to None.
pub(crate) async fn pg_source_columns(
    d: &mut PgDriver,
    rel: &RelationRef,
    columns: &[ColumnInfo],
) -> Result<Vec<Option<String>>, IntrospectError> {
    let wanted: Vec<i16> = columns
        .iter()
        .filter_map(|c| match c.source() {
            Some(ColumnSource::Postgres { column_id, .. }) => Some(*column_id),
            _ => None,
        })
        .collect();
    if wanted.is_empty() {
        return Ok(vec![None; columns.len()]);
    }
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
        [&rel.schema(), &rel.name(), &wanted];
    let mut known = Vec::new();
    for row in d.catalog(PG_ATTRIBUTES_SQL, &params).await? {
        known.push((
            cell::<u32>(&row, 0)?,
            cell::<i16>(&row, 1)?,
            cell::<String>(&row, 2)?,
        ));
    }
    Ok(columns
        .iter()
        .map(|c| match c.source() {
            Some(ColumnSource::Postgres {
                table_oid,
                column_id,
            }) => known
                .iter()
                .find(|(oid, num, _)| oid == table_oid && num == column_id)
                .map(|(.., name)| name.clone()),
            _ => None,
        })
        .collect())
}
