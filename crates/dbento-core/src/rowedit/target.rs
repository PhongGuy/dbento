//! The edit target of a relation: its columns with their cast types and writability, and
//! the columns that identify one row.

use crate::introspect::{ColumnMeta, IntrospectError, RelationRef, quote_ident};
use crate::postgres::{PgDriver, cell};
use crate::session::Driver;
use crate::sqlite::{SqliteDriver, known_schema};
use crate::workspace::Engine;

/// One column of an edit target.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditColumn {
    meta: ColumnMeta,
    cast_type: String,
    editable: bool,
}

impl EditColumn {
    pub(crate) fn new(meta: ColumnMeta, cast_type: impl Into<String>, editable: bool) -> Self {
        Self {
            meta,
            cast_type: cast_type.into(),
            editable,
        }
    }

    /// The column as `Session::columns` describes it.
    pub fn meta(&self) -> &ColumnMeta {
        &self.meta
    }

    /// The column name.
    pub fn name(&self) -> &str {
        self.meta.name()
    }

    /// The type a value is cast to: on Postgres the quoted, schema-qualified catalog type
    /// without size modifiers or domains; on SQLite the declared type.
    pub fn cast_type(&self) -> &str {
        &self.cast_type
    }

    /// False for generated columns and `GENERATED ALWAYS` identity columns.
    pub fn editable(&self) -> bool {
        self.editable
    }
}

/// A relation described for editing: its columns and how one row is identified.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    relation: RelationRef,
    engine: Engine,
    columns: Vec<EditColumn>,
    identity: Vec<String>,
    locators: Vec<String>,
    only: bool,
    returning: bool,
    pins: Vec<(String, String)>,
}

impl EditTarget {
    pub(crate) fn new(
        relation: RelationRef,
        engine: Engine,
        columns: Vec<EditColumn>,
        identity: Vec<String>,
        locators: Vec<String>,
        only: bool,
    ) -> Self {
        Self {
            relation,
            engine,
            columns,
            identity,
            locators,
            only,
            returning: true,
            pins: Vec::new(),
        }
    }

    /// Whether an UPDATE may return the identity; false for a Postgres table whose INSTEAD
    /// UPDATE rule refuses `RETURNING`.
    pub(crate) fn returning(&self) -> bool {
        self.returning
    }

    /// The output settings of the session that described the target, which an apply session
    /// takes on so key text parses and returns as that session's browse reads it.
    pub(crate) fn pins(&self) -> &[(String, String)] {
        &self.pins
    }

    /// The relation edited.
    pub fn relation(&self) -> &RelationRef {
        &self.relation
    }

    /// The engine the relation lives in.
    pub fn engine(&self) -> Engine {
        self.engine
    }

    /// The columns in table order, matching `SELECT *`.
    pub fn columns(&self) -> &[EditColumn] {
        &self.columns
    }

    /// The column named `name`, if any.
    pub fn column(&self, name: &str) -> Option<&EditColumn> {
        self.columns.iter().find(|c| c.name() == name)
    }

    /// The columns identifying a row, in key order; empty when rows cannot be edited.
    pub fn identity(&self) -> &[String] {
        &self.identity
    }

    /// Whether rows can be identified, and so edited.
    pub fn has_identity(&self) -> bool {
        !self.identity.is_empty()
    }

    /// Rows are identified by a primary key or a SQLite rowid alias, which an UPDATE keeps.
    pub fn stable_key(&self) -> bool {
        self.has_identity() && !(self.engine == Engine::Postgres && !self.locators.is_empty())
    }

    /// Extra columns a browse selects before `*` to locate rows (`ctid, xmin` or a rowid alias).
    pub fn locators(&self) -> &[String] {
        &self.locators
    }

    /// Whether a browse can be ordered by `name`: any column, or a locator other than
    /// Postgres `xmin`, whose `xid` type has no ordering operator.
    pub fn sortable(&self, name: &str) -> bool {
        self.column(name).is_some()
            || (self.locators.iter().any(|l| l == name)
                && !(self.engine == Engine::Postgres && name == "xmin"))
    }

    /// Whether browse and edit statements name the relation with `ONLY`.
    pub fn only(&self) -> bool {
        self.only
    }
}

/// `$1` is the schema, `$2` the relation.
const PG_RELATION_SQL: &str = "SELECT c.relkind::text, c.relhassubclass, \
EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r \
        WHERE r.ev_class = c.oid AND r.ev_type = '2' AND r.is_instead) \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
WHERE n.nspname = $1::text AND c.relname = $2::text";

/// `$1` is the schema, `$2` the relation. The cast type resolves an array's element and a
/// domain chain to the ultimate base type, then takes the array type again, so a cast
/// neither truncates through a size modifier nor checks a domain.
const PG_EDIT_COLUMNS_SQL: &str = "\
SELECT a.attname::text,
       pg_catalog.quote_ident(cn.nspname) || '.' || pg_catalog.quote_ident(ct.typname),
       a.attgenerated <> '' OR a.attidentity = 'a'
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_type t ON t.oid = a.atttypid
LEFT JOIN pg_catalog.pg_type e ON e.typarray = t.oid
CROSS JOIN LATERAL (
    WITH RECURSIVE chain(oid, typtype, typbasetype) AS (
        SELECT x.oid, x.typtype, x.typbasetype FROM pg_catalog.pg_type x WHERE x.oid = coalesce(e.oid, t.oid)
        UNION ALL
        SELECT b.oid, b.typtype, b.typbasetype FROM chain ch JOIN pg_catalog.pg_type b ON b.oid = ch.typbasetype
        WHERE ch.typtype = 'd')
    SELECT chain.oid FROM chain WHERE chain.typtype <> 'd') base
JOIN pg_catalog.pg_type bt ON bt.oid = base.oid
JOIN pg_catalog.pg_type ct ON ct.oid = CASE WHEN e.oid IS NULL THEN bt.oid ELSE bt.typarray END
JOIN pg_catalog.pg_namespace cn ON cn.oid = ct.typnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum";

/// Describes a Postgres relation for editing; a missing relation is `Unexpected`.
pub(crate) async fn pg_edit_target(
    d: &mut PgDriver,
    rel: &RelationRef,
) -> Result<EditTarget, IntrospectError> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&rel.schema(), &rel.name()];
    let rows = d.catalog(PG_RELATION_SQL, &params).await?;
    let Some(row) = rows.first() else {
        return Err(missing(rel));
    };
    let (relkind, has_children) = (cell::<String>(row, 0)?, cell::<bool>(row, 1)?);
    let instead_update = cell::<bool>(row, 2)?;

    let metas = Driver::columns(d, rel).await?;
    let extra = d
        .catalog(PG_EDIT_COLUMNS_SQL, &params)
        .await?
        .iter()
        .map(|r| {
            Ok((
                cell::<String>(r, 0)?,
                cell::<String>(r, 1)?,
                !cell::<bool>(r, 2)?,
            ))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    let columns = zip_columns(metas, extra)?;

    // Only plain and partitioned tables are edited; a keyless plain table falls back to its
    // physical row address, checked against xmin so a moved or rewritten row reads as gone.
    let table = matches!(relkind.as_str(), "r" | "p");
    let mut identity = if table {
        key_columns(&columns)
    } else {
        Vec::new()
    };
    let mut locators = Vec::new();
    if identity.is_empty() && relkind == "r" {
        identity = vec!["ctid".to_owned(), "xmin".to_owned()];
        locators.clone_from(&identity);
    }
    let only = relkind == "r" && has_children;
    let settings = d.catalog(PG_PINNED_SQL, &[]).await?;
    let pins = match settings.first() {
        Some(row) => PG_PINNED
            .iter()
            .enumerate()
            .map(|(i, name)| Ok(((*name).to_owned(), cell::<String>(row, i)?)))
            .collect::<Result<Vec<_>, IntrospectError>>()?,
        None => Vec::new(),
    };
    let mut target = EditTarget::new(
        rel.clone(),
        Engine::Postgres,
        columns,
        identity,
        locators,
        only,
    );
    // Any INSTEAD UPDATE rule, even one with its own RETURNING, gets the plain UPDATE.
    target.returning = !instead_update;
    target.pins = pins;
    Ok(target)
}

/// The settings that shape a type's output and input text, beyond those fixed at startup.
const PG_PINNED: [&str; 5] = [
    "DateStyle",
    "IntervalStyle",
    "TimeZone",
    "extra_float_digits",
    "lc_monetary",
];
const PG_PINNED_SQL: &str = "SELECT pg_catalog.current_setting('DateStyle'), \
pg_catalog.current_setting('IntervalStyle'), pg_catalog.current_setting('TimeZone'), \
pg_catalog.current_setting('extra_float_digits'), pg_catalog.current_setting('lc_monetary')";

/// Describes a SQLite table or view for editing; an unknown schema or relation is `Unexpected`.
pub(crate) async fn sqlite_edit_target(
    d: &mut SqliteDriver,
    rel: &RelationRef,
) -> Result<EditTarget, IntrospectError> {
    let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
    let found = d
        .introspect(move |c| {
            // The schema cannot be bound, so an unknown one is answered before it is quoted in.
            if !known_schema(c, &schema)? {
                return Ok(None);
            }
            // SQLite matches names without regard to case, so `FROM album` reaches `Album`;
            // the target takes the name as stored.
            let sql = format!(
                "SELECT type, name FROM {}.sqlite_schema \
                 WHERE name = ?1 COLLATE NOCASE AND type IN ('table', 'view')",
                quote_ident(Engine::Sqlite, &schema)
            );
            let found: Option<(String, String)> = rusqlite::OptionalExtension::optional(
                c.query_row(&sql, [&name], |r| Ok((r.get(0)?, r.get(1)?))),
            )?;
            let Some((kind, name)) = found else {
                return Ok(None);
            };
            // Hidden 2 and 3 are virtual and stored generated columns.
            let writable = c
                .prepare(
                    "SELECT name, hidden FROM pragma_table_xinfo(?1, ?2) \
                     WHERE hidden <> 1 ORDER BY cid",
                )?
                .query_map([&name, &schema], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        !matches!(r.get::<_, i64>(1)?, 2 | 3),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some((kind, name, writable)))
        })
        .await?;
    let Some((kind, name, writable)) = found else {
        return Err(missing(rel));
    };
    let rel = &RelationRef::new(rel.schema(), name);

    let metas = Driver::columns(d, rel).await?;
    // SQLite casts nothing, so the cast type is the declared one.
    let extra = writable
        .into_iter()
        .enumerate()
        .map(|(i, (n, w))| {
            let declared = metas.get(i).map_or("", |m| m.type_name());
            (n, declared.to_owned(), w)
        })
        .collect();
    let columns = zip_columns(metas, extra)?;

    // A keyless table is addressed by the first rowid alias no column shadows.
    let mut identity = Vec::new();
    let mut locators = Vec::new();
    if kind == "table" {
        identity = key_columns(&columns);
        if identity.is_empty() {
            let alias = ["rowid", "_rowid_", "oid"]
                .into_iter()
                .find(|a| !columns.iter().any(|c| c.name().eq_ignore_ascii_case(a)));
            if let Some(alias) = alias {
                identity = vec![alias.to_owned()];
                locators.clone_from(&identity);
            }
        }
    }
    Ok(EditTarget::new(
        rel.clone(),
        Engine::Sqlite,
        columns,
        identity,
        locators,
        false,
    ))
}

/// Joins `Driver::columns` with the per-column `(name, cast type, editable)` read alongside
/// it; the two must name the same columns in the same order.
fn zip_columns(
    metas: Vec<ColumnMeta>,
    extra: Vec<(String, String, bool)>,
) -> Result<Vec<EditColumn>, IntrospectError> {
    if metas.len() != extra.len() {
        return Err(IntrospectError::Unexpected {
            message: format!("{} columns but {} cast types", metas.len(), extra.len()),
        });
    }
    metas
        .into_iter()
        .zip(extra)
        .map(|(meta, (name, cast, editable))| {
            if meta.name() == name {
                Ok(EditColumn::new(meta, cast, editable))
            } else {
                Err(IntrospectError::Unexpected {
                    message: format!("column {} read back as {name}", meta.name()),
                })
            }
        })
        .collect()
}

/// The primary key columns ordered by their position in the key.
fn key_columns(columns: &[EditColumn]) -> Vec<String> {
    let mut keyed: Vec<(u16, &str)> = columns
        .iter()
        .filter_map(|c| c.meta().primary_key().map(|p| (p, c.name())))
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, n)| n.to_owned()).collect()
}

fn missing(rel: &RelationRef) -> IntrospectError {
    IntrospectError::Missing {
        relation: format!("{}.{}", rel.schema(), rel.name()),
    }
}
