//! SQL Server catalog queries over the `sys` views of the connection's database. Names are
//! bound as parameters, so the server compares and orders them in the database's collation.

use std::collections::HashMap;

use super::MssqlDriver;
use super::types::type_text;
use crate::fkeys::{FkDirection, ForeignKey};
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped, quote_ident,
};
use crate::objects::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, IndexInfo,
    ObjectRef, RelationObjects, RoutineInfo, RoutineKind, SchemaObjects, TriggerInfo, TriggerState,
    TypeInfo, TypeKind, definition_error, finish,
};
use crate::tsql::{TsqlKind, TsqlToken, tsql_tokens};
use crate::workspace::Engine;

/// The object id of the relation named by `@P1` and `@P2`, NULL when there is none, so an unknown
/// relation matches nothing.
const REL_ID: &str = "OBJECT_ID(QUOTENAME(@P1) + N'.' + QUOTENAME(@P2))";

/// Every `sys.objects` type that is a routine. `RF` is a procedure created `FOR REPLICATION`, and
/// `X` an extended procedure, which `sys` holds by the hundred.
const ROUTINE_TYPES: &str = "'P', 'PC', 'RF', 'X', 'FN', 'FS', 'IF', 'TF', 'FT', 'AF'";

/// `expr` cut by the server just past the cap, leaving `finish` the rest. The collation counts
/// code points, so the cut never splits a surrogate pair, which the client could not decode.
fn fetched(expr: &str) -> String {
    format!(
        "LEFT({expr} COLLATE Latin1_General_100_CI_AS_SC, {})",
        DEFINITION_CAP + 1
    )
}

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

/// The first `width` cells of each catalog row, all of which must be present and not NULL.
fn strings(
    rows: &[Vec<Option<String>>],
    width: usize,
) -> Result<Vec<Vec<String>>, IntrospectError> {
    rows.iter()
        .map(|r| (0..width).map(|ix| text(r, ix)).collect())
        .collect()
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

/// The tables, views and synonyms of `schema` by name, at most RELATION_CAP of them;
/// `sys.all_objects` also lists the system views of `sys` and `INFORMATION_SCHEMA`.
pub(crate) async fn relations(
    d: &mut MssqlDriver,
    schema: &str,
) -> Result<RelationList, IntrospectError> {
    let sql = format!(
        "SELECT TOP ({}) o.name, RTRIM(o.type) FROM sys.all_objects o \
         JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = @P1 AND o.type IN ('U', 'V', 'SN') ORDER BY o.name",
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
    // for a CLR type, which keeps its own name. The left join keeps a column whose user type
    // the login cannot see. The join to all_objects drops table-valued functions' result columns.
    // A bound default keeps its comments, so the server cuts the text.
    let default = fetched("OBJECT_DEFINITION(c.default_object_id)");
    let rows = d
        .catalog(
            &format!(
                "SELECT c.name, \
                 COALESCE(CASE WHEN t.is_assembly_type = 1 THEN t.name \
                 ELSE TYPE_NAME(c.system_type_id) END, N'unknown'), \
                 SCHEMA_NAME(t.schema_id), CAST(t.is_assembly_type AS nvarchar(1)), \
                 CAST(c.max_length AS nvarchar(11)), CAST(c.precision AS nvarchar(3)), \
                 CAST(c.scale AS nvarchar(3)), CAST(c.is_nullable AS nvarchar(1)), \
                 CAST(k.key_ordinal AS nvarchar(5)), {default}, \
                 CAST(OBJECTPROPERTY(c.default_object_id, 'IsDefaultCnst') AS nvarchar(1)) \
                 FROM sys.all_columns c \
                 JOIN sys.all_objects o ON o.object_id = c.object_id AND o.type IN ('U', 'V') \
                 LEFT JOIN sys.types t ON t.user_type_id = c.user_type_id \
                 LEFT JOIN (sys.indexes i JOIN sys.index_columns k \
                            ON k.object_id = i.object_id AND k.index_id = i.index_id) \
                   ON i.object_id = c.object_id AND i.is_primary_key = 1 AND k.column_id = c.column_id \
                 WHERE c.object_id = {REL_ID} \
                 ORDER BY c.column_id"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?;
    rows.iter()
        .map(|r| {
            let type_name = text(r, 1)?;
            // A user CLR type is spelled `schema.name`, as a result column's type is.
            let base = match (cell(r, 3)?.as_deref(), cell(r, 2)?) {
                (Some("1"), Some(schema)) if schema != "sys" => format!("{schema}.{type_name}"),
                _ => type_name,
            };
            let ty = type_text(&base, number(r, 4)?, number(r, 5)?, number(r, 6)?);
            let pk = cell(r, 8)?.map(|_| number(r, 8)).transpose()?;
            let default = default_text(cell(r, 9)?, cell(r, 10)?.as_deref());
            Ok(ColumnMeta::new(
                text(r, 0)?,
                ty,
                text(r, 7)? == "1",
                pk,
                default,
            ))
        })
        .collect()
}

/// A column's default from its stored text and its `IsDefaultCnst`, which is NULL only when there
/// is no default. A T-SQL comment marks text withheld from the login, and text the server cut.
fn default_text(text: Option<String>, is_constraint: Option<&str>) -> Option<String> {
    let Some(text) = text else {
        return is_constraint.map(|_| "/* withheld: needs VIEW DEFINITION */".to_owned());
    };
    // `fetched` leaves one code point past the cap, so the test comes before `bound_default`,
    // whose result can look whole. The mark leads, as an open string would swallow a trailing one.
    let cut = text.chars().count() > DEFINITION_CAP;
    let expression = match is_constraint {
        Some("0") => bound_default(&text),
        _ => text,
    };
    Some(if cut {
        format!("/* cut */ {expression}")
    } else {
        expression
    })
}

/// The expression of `CREATE DEFAULT name AS expression`; text of another shape comes back whole.
pub(crate) fn bound_default(definition: &str) -> String {
    // Comments and strings are tokens of their own, so only a real `AS` opens the expression, and
    // its last code token ends it.
    let tokens = tsql_tokens(definition);
    let spelled = |t: &TsqlToken| &definition[t.range.clone()];
    let mut code: Vec<_> = tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TsqlKind::Space | TsqlKind::LineComment | TsqlKind::BlockComment
            )
        })
        .skip_while(|t| t.kind != TsqlKind::Word || !spelled(t).eq_ignore_ascii_case("AS"))
        .skip(1)
        .collect();
    if code.last().is_some_and(|t| spelled(t) == ";") {
        code.pop();
    }
    match (code.first(), code.last()) {
        (Some(first), Some(last)) => definition[first.range.start..last.range.end].to_owned(),
        _ => definition.to_owned(),
    }
}

/// Indexes by name, keys and checks by kind (primary, unique, foreign, check) then name, triggers
/// by name; empty lists for an unknown relation.
pub(crate) async fn relation_objects(
    d: &mut MssqlDriver,
    rel: &RelationRef,
) -> Result<RelationObjects, IntrospectError> {
    let indexes = indexes(d, rel).await?;
    let constraints = constraints(d, rel).await?;
    let triggers = d
        .catalog(
            &format!(
                "SELECT name, CAST(is_disabled AS nvarchar(1)) FROM sys.triggers \
                 WHERE parent_id = {REL_ID} ORDER BY name"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?
        .iter()
        .map(|r| {
            let state = match text(r, 1)?.as_str() {
                "1" => TriggerState::Disabled,
                _ => TriggerState::Enabled,
            };
            Ok(TriggerInfo::new(rel.clone(), text(r, 0)?, state))
        })
        .collect::<Result<Vec<_>, IntrospectError>>()?;
    Ok(RelationObjects::new(indexes, constraints, triggers))
}

/// The indexes of `rel` by name, without the heap and hypothetical indexes. Columns are the key
/// columns in key order; a columnstore, XML or spatial index has no key, so it lists its columns.
async fn indexes(
    d: &mut MssqlDriver,
    rel: &RelationRef,
) -> Result<Vec<IndexInfo>, IntrospectError> {
    let rows = d
        .catalog(
            &format!(
                "SELECT i.name, CAST(i.is_unique AS nvarchar(1)), \
                 CAST(i.is_primary_key AS nvarchar(1)), CAST(i.has_filter AS nvarchar(1)), \
                 LOWER(i.type_desc), c.name \
                 FROM sys.indexes i \
                 JOIN sys.index_columns ic \
                   ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
                 JOIN sys.all_columns c \
                   ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                 WHERE i.object_id = {REL_ID} AND i.index_id > 0 AND i.is_hypothetical = 0 \
                 AND (ic.key_ordinal > 0 OR i.type NOT IN (1, 2, 7)) \
                 ORDER BY i.name, ic.key_ordinal, ic.index_column_id"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?;
    let rows = strings(&rows, 6)?;
    Ok(rows
        .chunk_by(|a, b| a[0] == b[0])
        .map(|group| {
            let first = &group[0];
            IndexInfo::new(
                rel.clone(),
                first[0].as_str(),
                group.iter().map(|r| r[5].clone()).collect(),
                first[1] == "1",
                first[2] == "1",
                first[3] == "1",
                Some(first[4].clone()),
            )
        })
        .collect())
}

/// The keys and checks of `rel` by kind then name. A check names its one column, or none when
/// it spans several; a foreign key to a table the login cannot see has no `referenced`; an edge
/// constraint is `Other`.
async fn constraints(
    d: &mut MssqlDriver,
    rel: &RelationRef,
) -> Result<Vec<ConstraintInfo>, IntrospectError> {
    let rows = d
        .catalog(
            &format!(
                "SELECT kc.name, RTRIM(kc.type), c.name FROM sys.key_constraints kc \
                 JOIN sys.index_columns ic \
                   ON ic.object_id = kc.parent_object_id AND ic.index_id = kc.unique_index_id \
                 JOIN sys.all_columns c \
                   ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                 WHERE kc.parent_object_id = {REL_ID} ORDER BY kc.name, ic.key_ordinal"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?;
    let rows = strings(&rows, 3)?;
    let mut constraints: Vec<_> = rows
        .chunk_by(|a, b| a[0] == b[0])
        .map(|group| {
            let first = &group[0];
            let kind = match first[1].as_str() {
                "PK" => ConstraintKind::PrimaryKey,
                _ => ConstraintKind::Unique,
            };
            let columns = group.iter().map(|r| r[2].clone()).collect();
            ConstraintInfo::new(rel.clone(), Some(first[0].clone()), kind, columns, None)
        })
        .collect();
    let keys = key_rows(d, rel, FkDirection::Outgoing).await?;
    for group in keys.chunk_by(|a, b| a[..3] == b[..3]) {
        let referenced = match (cell(&group[0], 4)?, cell(&group[0], 5)?) {
            (Some(schema), Some(name)) => Some(RelationRef::new(schema, name)),
            _ => None,
        };
        constraints.push(ConstraintInfo::new(
            rel.clone(),
            Some(text(&group[0], 2)?),
            ConstraintKind::ForeignKey,
            group.iter().map(|r| text(r, 3)).collect::<Result<_, _>>()?,
            referenced,
        ));
    }
    let checks = d
        .catalog(
            &format!(
                "SELECT name, COL_NAME(parent_object_id, parent_column_id) \
                 FROM sys.check_constraints WHERE parent_object_id = {REL_ID}"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?;
    for r in &checks {
        constraints.push(ConstraintInfo::new(
            rel.clone(),
            Some(text(r, 0)?),
            ConstraintKind::Check,
            cell(r, 1)?.into_iter().collect(),
            None,
        ));
    }
    let edges = d
        .catalog(
            &format!(
                "SELECT name FROM sys.objects WHERE type = 'EC' AND parent_object_id = {REL_ID}"
            ),
            &[rel.schema(), rel.name()],
        )
        .await?;
    for r in &edges {
        constraints.push(ConstraintInfo::new(
            rel.clone(),
            Some(text(r, 0)?),
            ConstraintKind::Other,
            Vec::new(),
            None,
        ));
    }
    let rank = |k: ConstraintKind| match k {
        ConstraintKind::PrimaryKey => 0,
        ConstraintKind::Unique => 1,
        ConstraintKind::ForeignKey => 2,
        ConstraintKind::Check => 3,
        ConstraintKind::Exclusion | ConstraintKind::Other => 4,
    };
    constraints.sort_by(|a, b| (rank(a.kind()), a.name()).cmp(&(rank(b.kind()), b.name())));
    Ok(constraints)
}

/// Keys of `rel` in both directions, unsorted; a self-reference is listed both ways. A key to a
/// table the login cannot see is left out, as without a far name there is nothing to follow.
pub(crate) async fn foreign_keys(
    d: &mut MssqlDriver,
    rel: &RelationRef,
) -> Result<Vec<ForeignKey>, IntrospectError> {
    let mut keys = Vec::new();
    for direction in [FkDirection::Outgoing, FkDirection::Incoming] {
        let rows = key_rows(d, rel, direction).await?;
        for group in rows.chunk_by(|a, b| a[..3] == b[..3]) {
            if group.iter().any(|r| r[4..7].contains(&None)) {
                continue;
            }
            let group = strings(group, 7)?;
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

/// One row per column of the keys declared on `rel` (outgoing) or referencing it (incoming):
/// schema, table, key name and column, then the far schema, table and column, which are NULL
/// when the login cannot see them. Each key's rows are adjacent and in the constraint's order.
async fn key_rows(
    d: &mut MssqlDriver,
    rel: &RelationRef,
    direction: FkDirection,
) -> Result<Vec<Vec<Option<String>>>, IntrospectError> {
    let side = match direction {
        FkDirection::Outgoing => "fk.parent_object_id",
        FkDirection::Incoming => "fk.referenced_object_id",
    };
    d.catalog(
        &format!(
            "SELECT SCHEMA_NAME(p.schema_id), p.name, fk.name, pc.name, \
             SCHEMA_NAME(r.schema_id), r.name, rc.name \
             FROM sys.foreign_keys fk \
             JOIN sys.foreign_key_columns fc ON fc.constraint_object_id = fk.object_id \
             JOIN sys.all_objects p ON p.object_id = fk.parent_object_id \
             JOIN sys.all_columns pc \
               ON pc.object_id = fc.parent_object_id AND pc.column_id = fc.parent_column_id \
             LEFT JOIN sys.all_objects r ON r.object_id = fk.referenced_object_id \
             LEFT JOIN sys.all_columns rc ON rc.object_id = fc.referenced_object_id \
               AND rc.column_id = fc.referenced_column_id \
             WHERE {side} = {REL_ID} ORDER BY fk.object_id, fc.constraint_column_id"
        ),
        &[rel.schema(), rel.name()],
    )
    .await
}

/// The user-defined types and the routines of `schema` by name, at most RELATION_CAP of each;
/// empty for an unknown schema.
pub(crate) async fn schema_objects(
    d: &mut MssqlDriver,
    schema: &str,
) -> Result<SchemaObjects, IntrospectError> {
    let mut types = user_types(d, schema).await?;
    let mut routines = routines(d, schema).await?;
    let truncated = types.len() > RELATION_CAP || routines.len() > RELATION_CAP;
    types.truncate(RELATION_CAP);
    routines.truncate(RELATION_CAP);
    Ok(SchemaObjects::new(types, routines, truncated))
}

async fn user_types(d: &mut MssqlDriver, schema: &str) -> Result<Vec<TypeInfo>, IntrospectError> {
    let sql = format!(
        "SELECT TOP ({}) t.name, CAST(t.is_table_type AS nvarchar(1)), \
         CAST(t.is_assembly_type AS nvarchar(1)) FROM sys.types t \
         JOIN sys.schemas s ON s.schema_id = t.schema_id \
         WHERE s.name = @P1 AND t.is_user_defined = 1 ORDER BY t.name",
        RELATION_CAP + 1
    );
    let rows = d.catalog(&sql, &[schema]).await?;
    let rows = strings(&rows, 3)?;
    Ok(rows
        .iter()
        .map(|r| {
            let kind = match (r[1].as_str(), r[2].as_str()) {
                ("1", _) => TypeKind::Composite,
                (_, "1") => TypeKind::Other,
                _ => TypeKind::Domain,
            };
            TypeInfo::new(schema, r[0].as_str(), kind)
        })
        .collect())
}

/// The routines of `schema` by name, each with its arguments and result type.
async fn routines(d: &mut MssqlDriver, schema: &str) -> Result<Vec<RoutineInfo>, IntrospectError> {
    let sql = format!(
        "SELECT TOP ({}) CAST(o.object_id AS nvarchar(11)), o.name, RTRIM(o.type) \
         FROM sys.all_objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = @P1 AND o.type IN ({ROUTINE_TYPES}) ORDER BY o.name",
        RELATION_CAP + 1
    );
    let listed = d.catalog(&sql, &[schema]).await?;
    let mut params = parameters(d, schema).await?;
    listed
        .iter()
        .map(|r| {
            let (id, name, kind) = (text(r, 0)?, text(r, 1)?, text(r, 2)?);
            let (arguments, result) = params.remove(&id).unwrap_or_default();
            let (kind, returns) = match kind.as_str() {
                "P" | "PC" | "RF" | "X" => (RoutineKind::Procedure, None),
                "AF" => (RoutineKind::Aggregate, result),
                "IF" | "TF" | "FT" => (RoutineKind::Function, Some("TABLE".to_owned())),
                _ => (RoutineKind::Function, result),
            };
            let arguments = arguments.join(", ");
            Ok(RoutineInfo::new(schema, name, kind, arguments, returns))
        })
        .collect()
}

/// Per routine of `schema` by object id, its arguments in order and the type of parameter 0.
/// A login without rights on a user type does not see its row, so the base type stands in; a
/// cursor parameter reports `int`.
async fn parameters(
    d: &mut MssqlDriver,
    schema: &str,
) -> Result<HashMap<String, (Vec<String>, Option<String>)>, IntrospectError> {
    let rows = d
        .catalog(
            "SELECT CAST(p.object_id AS nvarchar(11)), CAST(p.parameter_id AS nvarchar(11)), p.name, \
             COALESCE(CASE WHEN p.is_cursor_ref = 1 THEN N'cursor' \
             WHEN t.is_user_defined = 1 THEN SCHEMA_NAME(t.schema_id) + N'.' + t.name \
             ELSE t.name END, TYPE_NAME(p.system_type_id), N'unknown'), \
             CAST(p.max_length AS nvarchar(6)), CAST(p.precision AS nvarchar(3)), \
             CAST(p.scale AS nvarchar(3)), CAST(p.is_output AS nvarchar(1)), \
             CAST(p.is_readonly AS nvarchar(1)) \
             FROM sys.all_parameters p \
             JOIN sys.all_objects o ON o.object_id = p.object_id \
             JOIN sys.schemas s ON s.schema_id = o.schema_id \
             LEFT JOIN sys.types t ON t.user_type_id = p.user_type_id \
             WHERE s.name = @P1 ORDER BY p.object_id, p.parameter_id",
            &[schema],
        )
        .await?;
    let mut params: HashMap<String, (Vec<String>, Option<String>)> = HashMap::new();
    for r in &rows {
        let ty = type_text(&text(r, 3)?, number(r, 4)?, number(r, 5)?, number(r, 6)?);
        let entry = params.entry(text(r, 0)?).or_default();
        if text(r, 1)? == "0" {
            entry.1 = Some(ty);
            continue;
        }
        let mode = match (text(r, 7)?.as_str(), text(r, 8)?.as_str()) {
            ("1", _) => " OUTPUT",
            (_, "1") => " READONLY",
            _ => "",
        };
        entry.0.push(format!("{} {ty}{mode}", text(r, 2)?));
    }
    Ok(params)
}

/// The text the server stores for `object`, at most DEFINITION_CAP bytes. A check and a synonym are
/// composed from what the catalog holds, and a routine is found by schema and name, as T-SQL has
/// no overloads. A table, index, key or type has no stored text and is `Unsupported`.
pub(crate) async fn definition(
    d: &mut MssqlDriver,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    let unsupported = || DefinitionError::Unsupported {
        what: object.label(),
    };
    let fail = |e| definition_error(e, object);
    match object {
        ObjectRef::Relation(rel) => {
            let sql = format!(
                "SELECT {}, RTRIM(o.type), sn.base_object_name \
                 FROM sys.all_objects o LEFT JOIN sys.synonyms sn ON sn.object_id = o.object_id \
                 WHERE o.object_id = {REL_ID}",
                fetched("OBJECT_DEFINITION(o.object_id)")
            );
            let rows = d.catalog(&sql, &[rel.schema(), rel.name()]).await;
            let rows = rows.map_err(fail)?;
            match rows.first().map(|r| text(r, 1)).transpose().map_err(fail)? {
                None => Err(missing(object)),
                Some(kind) if kind == "V" => stored(&rows, object),
                Some(kind) if kind == "SN" => {
                    let base = text(&rows[0], 2).map_err(fail)?;
                    let q = |s: &str| quote_ident(Engine::Mssql, s);
                    let (schema, name) = (q(rel.schema()), q(rel.name()));
                    let text = format!("CREATE SYNONYM {schema}.{name} FOR {base}");
                    finish(Some(text), DEFINITION_CAP, true, object)
                }
                Some(_) => Err(unsupported()),
            }
        }
        ObjectRef::Trigger { rel, name } => {
            let sql = format!(
                "SELECT {} FROM sys.triggers WHERE parent_id = {REL_ID} AND name = @P3",
                fetched("OBJECT_DEFINITION(object_id)")
            );
            let rows = d.catalog(&sql, &[rel.schema(), rel.name(), name]).await;
            stored(&rows.map_err(fail)?, object)
        }
        ObjectRef::Routine { schema, name, .. } => {
            let sql = format!(
                "SELECT {} FROM sys.all_objects \
                 WHERE object_id = {REL_ID} AND type IN ({ROUTINE_TYPES})",
                fetched("OBJECT_DEFINITION(object_id)")
            );
            let rows = d.catalog(&sql, &[schema, name]).await;
            stored(&rows.map_err(fail)?, object)
        }
        ObjectRef::Constraint {
            rel,
            name: Some(name),
            kind: ConstraintKind::Check,
        } => {
            let sql = format!(
                "SELECT {} FROM sys.check_constraints \
                 WHERE parent_object_id = {REL_ID} AND name = @P3",
                fetched("definition")
            );
            let rows = d.catalog(&sql, &[rel.schema(), rel.name(), name]).await;
            let rows = rows.map_err(fail)?;
            let Some(row) = rows.first() else {
                return Err(missing(object));
            };
            let Some(check) = cell(row, 0).map_err(fail)? else {
                return Err(no_text(object, "the login lacks VIEW DEFINITION"));
            };
            let q = |s: &str| quote_ident(Engine::Mssql, s);
            let (schema, table, name) = (q(rel.schema()), q(rel.name()), q(name));
            let text = format!("ALTER TABLE {schema}.{table} ADD CONSTRAINT {name} CHECK {check}");
            finish(Some(text), DEFINITION_CAP, true, object)
        }
        ObjectRef::Index { .. } | ObjectRef::Constraint { .. } | ObjectRef::Type { .. } => {
            Err(unsupported())
        }
    }
}

fn missing(object: &ObjectRef) -> DefinitionError {
    DefinitionError::Missing {
        object: object.label(),
    }
}

/// Why the server shows NULL where a row exists: it never says which cause applies.
fn no_text(object: &ObjectRef, causes: &str) -> DefinitionError {
    DefinitionError::Unexpected {
        message: format!("the server shows no text for {}: {causes}", object.label()),
    }
}

/// The first column of the one row a text lookup returns; no row means the object is gone.
fn stored(rows: &[Vec<Option<String>>], object: &ObjectRef) -> Result<Definition, DefinitionError> {
    let Some(row) = rows.first() else {
        return Err(missing(object));
    };
    match cell(row, 0).map_err(|e| definition_error(e, object))? {
        Some(text) => finish(Some(text), DEFINITION_CAP, false, object),
        None => Err(no_text(
            object,
            "it is encrypted, CLR or an extended procedure, or the login lacks VIEW DEFINITION",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{bound_default, default_text};
    use crate::objects::DEFINITION_CAP;

    #[test]
    fn a_legacy_default_is_the_expression_after_its_first_as() {
        for (text, want) in [
            ("CREATE DEFAULT dbo.d AS 0", "0"),
            ("CREATE DEFAULT dbo.d AS 0;", "0"),
            ("create default d as (1 + 2) -- note\n", "(1 + 2)"),
            (
                "CREATE DEFAULT /* AS 9 */ d AS N'a AS b'  -- trailing",
                "N'a AS b'",
            ),
            ("CREATE DEFAULT [as] AS GETDATE()\r\n", "GETDATE()"),
            ("CREATE DEFAULT d\nAS\n'x;y' /* c */ ;", "'x;y'"),
        ] {
            assert_eq!(bound_default(text), want, "{text}");
        }
    }

    #[test]
    fn a_withheld_default_says_so_where_no_default_stays_none() {
        assert_eq!(default_text(None, None), None);
        for is_constraint in ["0", "1"] {
            assert_eq!(
                default_text(None, Some(is_constraint)).as_deref(),
                Some("/* withheld: needs VIEW DEFINITION */")
            );
        }
    }

    #[test]
    fn a_default_within_the_cap_is_unmarked() {
        let constraint = default_text(Some("((7))".to_owned()), Some("1"));
        assert_eq!(constraint.as_deref(), Some("((7))"));
        let bound = default_text(Some("CREATE DEFAULT d AS 0".to_owned()), Some("0"));
        assert_eq!(bound.as_deref(), Some("0"));
    }

    // The tail of `1 /*huge*/ + 2` is gone, so the `1` left must not read as the whole expression.
    #[test]
    fn a_bound_default_cut_in_a_comment_is_marked_though_it_looks_whole() {
        let cut = format!("CREATE DEFAULT d AS 1 /*{}", "x".repeat(DEFINITION_CAP));
        let got = default_text(Some(cut), Some("0"));
        assert_eq!(got.as_deref(), Some("/* cut */ 1"));
    }

    // The mark leads, as one placed after a string the cut left open would read as its content.
    #[test]
    fn a_constraint_default_cut_in_a_string_is_marked_ahead_of_it() {
        let cut = format!("(N'{}", "x".repeat(DEFINITION_CAP));
        let got = default_text(Some(cut.clone()), Some("1"));
        assert_eq!(got, Some(format!("/* cut */ {cut}")));
    }

    // `fetched` counts code points, so at the cap an emoji text is whole however many bytes it has.
    #[test]
    fn the_cap_counts_code_points() {
        let at = "\u{1F600}".repeat(DEFINITION_CAP);
        assert_eq!(default_text(Some(at.clone()), Some("1")), Some(at));
        let past = "\u{1F600}".repeat(DEFINITION_CAP + 1);
        let got = default_text(Some(past.clone()), Some("1"));
        assert_eq!(got, Some(format!("/* cut */ {past}")));
    }

    #[test]
    fn text_of_any_other_shape_comes_back_whole() {
        for text in [
            "no keyword here",
            "CREATE DEFAULT d AS",
            "CREATE DEFAULT d AS ;",
        ] {
            assert_eq!(bound_default(text), text);
        }
    }
}
