//! SQL Server catalog queries over the `sys` views of the connection's database. Names are
//! bound as parameters, so the server compares and orders them in the database's collation.

use super::MssqlDriver;
use super::types::type_text;
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped,
};

/// Cell `ix` of a catalog row that must be present and not NULL.
fn text(row: &[Option<String>], ix: usize) -> Result<String, IntrospectError> {
    match row.get(ix) {
        Some(Some(t)) => Ok(t.clone()),
        Some(None) => Err(IntrospectError::Unexpected {
            message: format!("catalog column {ix} is NULL"),
        }),
        None => Err(IntrospectError::Unexpected {
            message: format!("catalog column {ix} is missing"),
        }),
    }
}

/// Cell `ix` of a catalog row that may be NULL.
fn cell(row: &[Option<String>], ix: usize) -> Result<Option<String>, IntrospectError> {
    row.get(ix)
        .cloned()
        .ok_or_else(|| IntrospectError::Unexpected {
            message: format!("catalog column {ix} is missing"),
        })
}

/// Cell `ix` of a catalog row parsed as a number.
fn number<T: std::str::FromStr>(row: &[Option<String>], ix: usize) -> Result<T, IntrospectError> {
    let t = text(row, ix)?;
    t.parse().map_err(|_| IntrospectError::Unexpected {
        message: format!("catalog column {ix} is not a number: {t:?}"),
    })
}

/// The user's schemas by name, then `INFORMATION_SCHEMA` and `sys`; `guest` and the role
/// schemas (id 16384 and up) are left out.
pub(crate) async fn schemas(d: &mut MssqlDriver) -> Result<Vec<SchemaInfo>, IntrospectError> {
    let rows = d
        .catalog(
            "SELECT name FROM sys.schemas WHERE schema_id < 16384 AND name <> N'guest' \
             ORDER BY CASE WHEN name IN (N'sys', N'INFORMATION_SCHEMA') THEN 1 ELSE 0 END, name",
            &[],
        )
        .await?;
    rows.iter()
        .map(|r| text(r, 0).map(SchemaInfo::new))
        .collect()
}

/// The tables and views of `schema` by name, at most RELATION_CAP of them; `sys.all_objects`
/// also lists the system views of `sys` and `INFORMATION_SCHEMA`.
pub(crate) async fn relations(
    d: &mut MssqlDriver,
    schema: &str,
) -> Result<RelationList, IntrospectError> {
    let sql = format!(
        "SELECT TOP ({}) o.name, RTRIM(o.type) FROM sys.all_objects o \
         JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = @P1 AND o.type IN ('U', 'V') ORDER BY o.name",
        RELATION_CAP + 1
    );
    let rows = d.catalog(&sql, &[schema]).await?;
    let items = rows
        .iter()
        .map(|r| {
            let kind = match r.get(1).and_then(Option::as_deref) {
                Some("U") => RelationKind::Table,
                Some("V") => RelationKind::View,
                _ => RelationKind::Other,
            };
            Ok(Relation::new(text(r, 0)?, kind))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    Ok(capped(items))
}

/// The columns of `rel` in column order with their primary-key positions and defaults; empty
/// when no table or view of that name exists (a synonym included).
pub(crate) async fn columns(
    d: &mut MssqlDriver,
    rel: &RelationRef,
) -> Result<Vec<ColumnMeta>, IntrospectError> {
    // TYPE_NAME of the system type folds alias types and sysname into their base, but is NULL
    // for a CLR type, which keeps its own name. The join to all_objects drops the result
    // columns of table-valued functions.
    let rows = d
        .catalog(
            "SELECT c.name, \
             CASE WHEN t.is_assembly_type = 1 THEN t.name ELSE TYPE_NAME(c.system_type_id) END, \
             SCHEMA_NAME(t.schema_id), CAST(t.is_assembly_type AS nvarchar(1)), \
             CAST(c.max_length AS nvarchar(11)), CAST(c.precision AS nvarchar(3)), \
             CAST(c.scale AS nvarchar(3)), CAST(c.is_nullable AS nvarchar(1)), \
             CAST(k.key_ordinal AS nvarchar(5)), dc.definition \
             FROM sys.all_columns c \
             JOIN sys.all_objects o ON o.object_id = c.object_id AND o.type IN ('U', 'V') \
             JOIN sys.types t ON t.user_type_id = c.user_type_id \
             LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id \
             LEFT JOIN (sys.indexes i JOIN sys.index_columns k \
                        ON k.object_id = i.object_id AND k.index_id = i.index_id) \
               ON i.object_id = c.object_id AND i.is_primary_key = 1 AND k.column_id = c.column_id \
             WHERE c.object_id = OBJECT_ID(QUOTENAME(@P1) + N'.' + QUOTENAME(@P2)) \
             ORDER BY c.column_id",
            &[rel.schema(), rel.name()],
        )
        .await?;
    rows.iter()
        .map(|r| {
            let type_name = text(r, 1)?;
            // A user CLR type is spelled `schema.name`, as a result column's type is.
            let base = match (text(r, 3)?.as_str(), text(r, 2)?) {
                ("1", schema) if schema != "sys" => format!("{schema}.{type_name}"),
                _ => type_name,
            };
            let ty = type_text(&base, number(r, 4)?, number(r, 5)?, number(r, 6)?);
            let pk = cell(r, 8)?.map(|_| number(r, 8)).transpose()?;
            Ok(ColumnMeta::new(
                text(r, 0)?,
                ty,
                text(r, 7)? == "1",
                pk,
                cell(r, 9)?,
            ))
        })
        .collect()
}
