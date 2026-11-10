//! Postgres catalog lookups for schema objects and their definitions.

use tokio_postgres::Row;
use tokio_postgres::types::ToSql;

use crate::introspect::{IntrospectError, RELATION_CAP, RelationRef};
use crate::postgres::{PgDriver, cell};

use super::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, IndexInfo,
    ObjectRef, RelationObjects, RoutineInfo, RoutineKind, SchemaObjects, TriggerInfo, TriggerState,
    TypeInfo, TypeKind, definition_error, finish, qualified,
};

/// The definition cut passed to SQL `left()`, which counts chars: one char past the cap brings
/// any over-cap text in over it in bytes.
const CAP_CHARS: i32 = {
    assert!(
        DEFINITION_CAP < i32::MAX as usize,
        "the cap plus one fits an i32"
    );
    (DEFINITION_CAP + 1) as i32
};

/// A relation's kind and, for a view or materialized view, its CREATE text cut to `$3` chars.
/// The relation's `reloptions` ride along as `WITH (..)` so the text replays faithfully.
const DEF_RELATION_SQL: &str = r#"
SELECT c.relkind::text,
  left(CASE WHEN c.relkind IN ('v', 'm') THEN
    pg_catalog.format('CREATE %sVIEW %I.%I%s AS',
      CASE c.relkind WHEN 'm' THEN 'MATERIALIZED ' ELSE '' END, n.nspname, c.relname,
      CASE WHEN c.reloptions IS NOT NULL
        THEN ' WITH (' || pg_catalog.array_to_string(c.reloptions, ', ') || ')' ELSE '' END)
    || E'\n' || pg_catalog.pg_get_viewdef(c.oid, true) END, $3::int4)
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
"#;

/// An index's CREATE INDEX text; `$1` schema, `$2` table, `$3` index, `$4` cap.
const DEF_INDEX_SQL: &str = r#"
SELECT left(pg_catalog.pg_get_indexdef(i.indexrelid), $4::int4)
FROM pg_catalog.pg_index i
JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid
JOIN pg_catalog.pg_class c ON c.oid = i.indrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND ic.relname = $3::text
"#;

/// A listed constraint as `ALTER TABLE .. ADD CONSTRAINT`; `$1` schema, `$2` table,
/// `$3` constraint, `$4` cap.
const DEF_CONSTRAINT_SQL: &str = r#"
SELECT left(pg_catalog.format('ALTER TABLE %I.%I ADD CONSTRAINT %I ', n.nspname, c.relname, con.conname)
            || pg_catalog.pg_get_constraintdef(con.oid, true), $4::int4)
FROM pg_catalog.pg_constraint con
JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND con.conname = $3::text
  AND con.contype IN ('p', 'u', 'f', 'x', 'c')
"#;

/// A user trigger's CREATE TRIGGER text; `$1` schema, `$2` relation, `$3` trigger, `$4` cap.
const DEF_TRIGGER_SQL: &str = r#"
SELECT left(pg_catalog.pg_get_triggerdef(t.oid, true), $4::int4)
FROM pg_catalog.pg_trigger t
JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND t.tgname = $3::text AND NOT t.tgisinternal
"#;

/// A routine's kind and its CREATE text: the engine's for a function or procedure, composed
/// from `pg_aggregate` for an aggregate; `$1` schema, `$2` name, `$3` identity arguments,
/// `$4` cap. SORTOP is written `OPERATOR(schema.op)`, the only form that parses there.
const DEF_ROUTINE_SQL: &str = r#"
SELECT p.prokind::text,
  left(CASE WHEN p.prokind = 'a' THEN
    pg_catalog.format('CREATE AGGREGATE %I.%I(%s) (SFUNC = %s, STYPE = %s', n.nspname, p.proname,
      pg_catalog.pg_get_function_identity_arguments(p.oid), a.aggtransfn::regproc,
      pg_catalog.format_type(a.aggtranstype, NULL))
    || CASE WHEN a.aggfinalfn <> 0 THEN ', FINALFUNC = ' || a.aggfinalfn::regproc::text ELSE '' END
    || CASE WHEN a.aggcombinefn <> 0 THEN ', COMBINEFUNC = ' || a.aggcombinefn::regproc::text ELSE '' END
    || CASE WHEN a.agginitval IS NOT NULL THEN ', INITCOND = ' || pg_catalog.quote_literal(a.agginitval) ELSE '' END
    || CASE WHEN a.aggsortop <> 0
         THEN pg_catalog.format(', SORTOP = OPERATOR(%I.%s)', son.nspname, so.oprname) ELSE '' END
    || ')'
  ELSE pg_catalog.pg_get_functiondef(p.oid) END, $4::int4)
FROM pg_catalog.pg_proc p
JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
LEFT JOIN pg_catalog.pg_aggregate a ON a.aggfnoid = p.oid
LEFT JOIN pg_catalog.pg_operator so ON so.oid = a.aggsortop
LEFT JOIN pg_catalog.pg_namespace son ON son.oid = so.oprnamespace
WHERE n.nspname = $1::text AND p.proname = $2::text
  AND pg_catalog.pg_get_function_identity_arguments(p.oid) = $3::text
  AND p.prokind IN ('f', 'w', 'p', 'a')
"#;

/// A user type's CREATE text composed from the catalog; `$1` schema, `$2` type name, `$3` cap.
/// Only enums, domains, ranges and free-standing composites match: a table's row type,
/// a multirange or an array finds no row. A domain's NOT VALID checks cannot sit in CREATE
/// DOMAIN, so each follows as its own `ALTER DOMAIN .. NOT VALID` statement.
const DEF_TYPE_SQL: &str = r#"
SELECT t.typtype::text,
  left(CASE t.typtype
    WHEN 'e' THEN pg_catalog.format('CREATE TYPE %I.%I AS ENUM (%s)', n.nspname, t.typname,
      (SELECT pg_catalog.string_agg(pg_catalog.quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder)
       FROM pg_catalog.pg_enum e WHERE e.enumtypid = t.oid))
    WHEN 'd' THEN pg_catalog.format('CREATE DOMAIN %I.%I AS %s', n.nspname, t.typname,
        pg_catalog.format_type(t.typbasetype, t.typtypmod))
      || CASE WHEN t.typcollation <> 0 AND t.typcollation <> bt.typcollation
           THEN pg_catalog.format(' COLLATE %I.%I', cn.nspname, co.collname) ELSE '' END
      || CASE WHEN t.typdefaultbin IS NOT NULL
           THEN ' DEFAULT ' || pg_catalog.pg_get_expr(t.typdefaultbin, 0) ELSE '' END
      || CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END
      || COALESCE((SELECT pg_catalog.string_agg(pg_catalog.format(' CONSTRAINT %I ', k.conname)
                     || pg_catalog.pg_get_constraintdef(k.oid, true), '' ORDER BY k.conname)
                   FROM pg_catalog.pg_constraint k
                   WHERE k.contypid = t.oid AND k.contype = 'c' AND k.convalidated), '')
      || COALESCE((SELECT pg_catalog.string_agg(
                     pg_catalog.format(E';\nALTER DOMAIN %I.%I ADD CONSTRAINT %I ', n.nspname, t.typname, k.conname)
                     || pg_catalog.pg_get_constraintdef(k.oid, true), '' ORDER BY k.conname)
                   FROM pg_catalog.pg_constraint k
                   WHERE k.contypid = t.oid AND k.contype = 'c' AND NOT k.convalidated), '')
    WHEN 'c' THEN pg_catalog.format('CREATE TYPE %I.%I AS (%s)', n.nspname, t.typname,
      (SELECT pg_catalog.string_agg(pg_catalog.quote_ident(a.attname) || ' '
           || pg_catalog.format_type(a.atttypid, a.atttypmod)
           || CASE WHEN a.attcollation <> 0 AND a.attcollation <> at.typcollation
                THEN pg_catalog.format(' COLLATE %I.%I', acn.nspname, aco.collname) ELSE '' END,
         ', ' ORDER BY a.attnum)
       FROM pg_catalog.pg_attribute a
       JOIN pg_catalog.pg_type at ON at.oid = a.atttypid
       LEFT JOIN pg_catalog.pg_collation aco ON aco.oid = a.attcollation
       LEFT JOIN pg_catalog.pg_namespace acn ON acn.oid = aco.collnamespace
       WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND NOT a.attisdropped))
    WHEN 'r' THEN pg_catalog.format('CREATE TYPE %I.%I AS RANGE (SUBTYPE = %s', n.nspname, t.typname,
        pg_catalog.format_type(r.rngsubtype, NULL))
      || CASE WHEN NOT opc.opcdefault
           THEN pg_catalog.format(', SUBTYPE_OPCLASS = %I.%I', ocn.nspname, opc.opcname) ELSE '' END
      || CASE WHEN r.rngcollation <> 0 AND r.rngcollation <> st.typcollation
           THEN pg_catalog.format(', COLLATION = %I.%I', rcn.nspname, rco.collname) ELSE '' END
      || CASE WHEN r.rngcanonical <> 0 THEN ', CANONICAL = ' || r.rngcanonical::regproc::text ELSE '' END
      || CASE WHEN r.rngsubdiff <> 0 THEN ', SUBTYPE_DIFF = ' || r.rngsubdiff::regproc::text ELSE '' END
      || ')'
  END, $3::int4)
FROM pg_catalog.pg_type t
JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
LEFT JOIN pg_catalog.pg_class rel ON rel.oid = t.typrelid
LEFT JOIN pg_catalog.pg_type bt ON bt.oid = t.typbasetype
LEFT JOIN pg_catalog.pg_collation co ON co.oid = t.typcollation
LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid = co.collnamespace
LEFT JOIN pg_catalog.pg_range r ON r.rngtypid = t.oid
LEFT JOIN pg_catalog.pg_type st ON st.oid = r.rngsubtype
LEFT JOIN pg_catalog.pg_opclass opc ON opc.oid = r.rngsubopc
LEFT JOIN pg_catalog.pg_namespace ocn ON ocn.oid = opc.opcnamespace
LEFT JOIN pg_catalog.pg_collation rco ON rco.oid = r.rngcollation
LEFT JOIN pg_catalog.pg_namespace rcn ON rcn.oid = rco.collnamespace
WHERE n.nspname = $1::text AND t.typname = $2::text
  AND (t.typtype IN ('e', 'd', 'r') OR (t.typtype = 'c' AND rel.relkind = 'c'))
"#;

/// Indexes of relation `$2` in schema `$1` by name, with their key columns only
/// (ord <= indnkeyatts); an expression key has attnum 0 and shows as its text.
const INDEXES_SQL: &str = r#"
SELECT ic.relname::text,
  ARRAY(SELECT COALESCE(a.attname::text, pg_catalog.pg_get_indexdef(i.indexrelid, k.ord::int, true))
        FROM unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
        LEFT JOIN pg_catalog.pg_attribute a
          ON a.attrelid = i.indrelid AND a.attnum = k.attnum AND k.attnum > 0
        WHERE k.ord <= i.indnkeyatts ORDER BY k.ord),
  i.indisunique, i.indisprimary, i.indpred IS NOT NULL, am.amname::text
FROM pg_catalog.pg_index i
JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid
JOIN pg_catalog.pg_am am ON am.oid = ic.relam
JOIN pg_catalog.pg_class c ON c.oid = i.indrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text
ORDER BY ic.relname
"#;

/// Keys and checks of relation `$2` in schema `$1`, by kind then name; the filter keeps out
/// Postgres 18's NOT NULL rows ('n').
const CONSTRAINTS_SQL: &str = r#"
SELECT con.conname::text, con.contype::text,
  ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY AS k(n, o)
        JOIN pg_catalog.pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.o),
  fn.nspname::text, f.relname::text
FROM pg_catalog.pg_constraint con
JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_catalog.pg_class f ON f.oid = con.confrelid
LEFT JOIN pg_catalog.pg_namespace fn ON fn.oid = f.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND con.contype IN ('p', 'u', 'f', 'x', 'c')
ORDER BY pg_catalog.strpos('pufxc', con.contype::text), con.conname
"#;

/// User triggers of relation `$2` in schema `$1` by name, with when each fires.
const TRIGGERS_SQL: &str = r#"
SELECT t.tgname::text, t.tgenabled::text
FROM pg_catalog.pg_trigger t
JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1::text AND c.relname = $2::text AND NOT t.tgisinternal
ORDER BY t.tgname
"#;

/// Keys, indexes, checks and triggers of `rel` on Postgres; empty lists when it is unknown.
pub(crate) async fn pg_relation_objects(
    d: &mut PgDriver,
    rel: &RelationRef,
) -> Result<RelationObjects, IntrospectError> {
    let params: [&(dyn ToSql + Sync); 2] = [&rel.schema(), &rel.name()];
    let mut indexes = Vec::new();
    for row in d.catalog_pinned(INDEXES_SQL, &params).await? {
        indexes.push(IndexInfo::new(
            rel.clone(),
            cell::<String>(&row, 0)?,
            cell(&row, 1)?,
            cell(&row, 2)?,
            cell(&row, 3)?,
            cell(&row, 4)?,
            cell(&row, 5)?,
        ));
    }
    let mut constraints = Vec::new();
    for row in d.catalog_pinned(CONSTRAINTS_SQL, &params).await? {
        let kind = match cell::<String>(&row, 1)?.as_str() {
            "p" => ConstraintKind::PrimaryKey,
            "u" => ConstraintKind::Unique,
            "f" => ConstraintKind::ForeignKey,
            "x" => ConstraintKind::Exclusion,
            "c" => ConstraintKind::Check,
            _ => ConstraintKind::Other,
        };
        // Only a foreign key joins a far relation, so both names are present or both absent.
        let referenced = match (
            cell::<Option<String>>(&row, 3)?,
            cell::<Option<String>>(&row, 4)?,
        ) {
            (Some(schema), Some(name)) => Some(RelationRef::new(schema, name)),
            _ => None,
        };
        constraints.push(ConstraintInfo::new(
            rel.clone(),
            Some(cell(&row, 0)?),
            kind,
            cell(&row, 2)?,
            referenced,
        ));
    }
    let mut triggers = Vec::new();
    for row in d.catalog_pinned(TRIGGERS_SQL, &params).await? {
        // 'O' (origin) and 'A' (always) both fire in an ordinary session.
        let state = match cell::<String>(&row, 1)?.as_str() {
            "D" => TriggerState::Disabled,
            "R" => TriggerState::ReplicaOnly,
            _ => TriggerState::Enabled,
        };
        triggers.push(TriggerInfo::new(
            rel.clone(),
            cell::<String>(&row, 0)?,
            state,
        ));
    }
    Ok(RelationObjects::new(indexes, constraints, triggers))
}

/// User types of schema `$1` by name, at most `$2`: table row types (relkind <> 'c'), arrays
/// and multiranges are implicit and stay out, as do extension members.
const TYPES_SQL: &str = r#"
SELECT t.typname::text, t.typtype::text
FROM pg_catalog.pg_type t
JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
LEFT JOIN pg_catalog.pg_class c ON c.oid = t.typrelid
WHERE n.nspname = $1::text
  AND (t.typtype IN ('e', 'd', 'r') OR (t.typtype = 'c' AND c.relkind = 'c'))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d
                  WHERE d.classid = 'pg_catalog.pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e')
ORDER BY t.typname LIMIT $2
"#;

/// Routines of schema `$1` by name then identity arguments, at most `$2`; extension members
/// ('e') and range constructors ('i') stay out.
const ROUTINES_SQL: &str = r#"
SELECT p.proname::text, p.prokind::text, pg_catalog.pg_get_function_identity_arguments(p.oid),
       pg_catalog.pg_get_function_result(p.oid)
FROM pg_catalog.pg_proc p
JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = $1::text AND p.prokind IN ('f', 'w', 'p', 'a')
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d
                  WHERE d.classid = 'pg_catalog.pg_proc'::regclass AND d.objid = p.oid
                    AND d.deptype IN ('e', 'i'))
ORDER BY p.proname, pg_catalog.pg_get_function_identity_arguments(p.oid) COLLATE "C"
LIMIT $2
"#;

/// Types and routines of `schema` on Postgres, at most RELATION_CAP of each; empty when it is
/// unknown. Each query asks for one row past the cap so a longer list is marked truncated.
pub(crate) async fn pg_schema_objects(
    d: &mut PgDriver,
    schema: &str,
) -> Result<SchemaObjects, IntrospectError> {
    let limit = i64::try_from(RELATION_CAP + 1).unwrap_or(i64::MAX);
    let params: [&(dyn ToSql + Sync); 2] = [&schema, &limit];
    let mut types = Vec::new();
    for row in d.catalog_pinned(TYPES_SQL, &params).await? {
        let kind = match cell::<String>(&row, 1)?.as_str() {
            "e" => TypeKind::Enum,
            "d" => TypeKind::Domain,
            "c" => TypeKind::Composite,
            "r" => TypeKind::Range,
            _ => TypeKind::Other,
        };
        types.push(TypeInfo::new(schema, cell::<String>(&row, 0)?, kind));
    }
    let mut routines = Vec::new();
    for row in d.catalog_pinned(ROUTINES_SQL, &params).await? {
        let kind = match cell::<String>(&row, 1)?.as_str() {
            "p" => RoutineKind::Procedure,
            "a" => RoutineKind::Aggregate,
            _ => RoutineKind::Function,
        };
        routines.push(RoutineInfo::new(
            schema,
            cell::<String>(&row, 0)?,
            kind,
            cell::<String>(&row, 2)?,
            cell(&row, 3)?,
        ));
    }
    let truncated = types.len() > RELATION_CAP || routines.len() > RELATION_CAP;
    types.truncate(RELATION_CAP);
    routines.truncate(RELATION_CAP);
    Ok(SchemaObjects::new(types, routines, truncated))
}

/// The current definition of `object` on Postgres, at most [`DEFINITION_CAP`] bytes.
pub(crate) async fn pg_definition(
    d: &mut PgDriver,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    match object {
        ObjectRef::Relation(rel) => {
            let rows = lookup(
                d,
                DEF_RELATION_SQL,
                &[&rel.schema(), &rel.name(), &CAP_CHARS],
                object,
            )
            .await?;
            let row = first(&rows, object)?;
            let kind: String = cell(row, 0).map_err(|e| definition_error(e, object))?;
            match kind.as_str() {
                "r" | "p" => Err(DefinitionError::Unsupported {
                    what: format!("table {}", qualified(rel)),
                }),
                "f" => Err(DefinitionError::Unsupported {
                    what: format!("foreign table {}", qualified(rel)),
                }),
                _ => text_at(row, 1, false, object),
            }
        }
        ObjectRef::Index { rel, name } => {
            let params: [&(dyn ToSql + Sync); 4] = [&rel.schema(), &rel.name(), name, &CAP_CHARS];
            let rows = lookup(d, DEF_INDEX_SQL, &params, object).await?;
            text_at(first(&rows, object)?, 0, false, object)
        }
        ObjectRef::Constraint { rel, name, .. } => {
            // A nameless constraint cannot be looked up by name, so it reads as gone.
            let Some(name) = name else {
                return Err(DefinitionError::Missing {
                    object: object.label(),
                });
            };
            let params: [&(dyn ToSql + Sync); 4] = [&rel.schema(), &rel.name(), name, &CAP_CHARS];
            let rows = lookup(d, DEF_CONSTRAINT_SQL, &params, object).await?;
            text_at(first(&rows, object)?, 0, false, object)
        }
        ObjectRef::Trigger { rel, name } => {
            let params: [&(dyn ToSql + Sync); 4] = [&rel.schema(), &rel.name(), name, &CAP_CHARS];
            let rows = lookup(d, DEF_TRIGGER_SQL, &params, object).await?;
            text_at(first(&rows, object)?, 0, false, object)
        }
        ObjectRef::Routine {
            schema,
            name,
            arguments,
        } => {
            let params: [&(dyn ToSql + Sync); 4] = [schema, name, arguments, &CAP_CHARS];
            let rows = lookup(d, DEF_ROUTINE_SQL, &params, object).await?;
            let row = first(&rows, object)?;
            let kind: String = cell(row, 0).map_err(|e| definition_error(e, object))?;
            text_at(row, 1, kind == "a", object)
        }
        ObjectRef::Type { schema, name } => {
            let params: [&(dyn ToSql + Sync); 3] = [schema, name, &CAP_CHARS];
            let rows = lookup(d, DEF_TYPE_SQL, &params, object).await?;
            text_at(first(&rows, object)?, 1, true, object)
        }
    }
}

/// Runs one definition lookup on the pinned catalog path, naming `object` in any error.
async fn lookup(
    d: &mut PgDriver,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
    object: &ObjectRef,
) -> Result<Vec<Row>, DefinitionError> {
    d.catalog_pinned(sql, params)
        .await
        .map_err(|e| definition_error(e, object))
}

/// The lookup's one row; none means the object is gone.
fn first<'r>(rows: &'r [Row], object: &ObjectRef) -> Result<&'r Row, DefinitionError> {
    rows.first().ok_or_else(|| DefinitionError::Missing {
        object: object.label(),
    })
}

/// The text in column `ix` through the text contract; `composed` marks text built from the
/// catalog rather than printed by the engine.
fn text_at(
    row: &Row,
    ix: usize,
    composed: bool,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    let text = cell(row, ix).map_err(|e| definition_error(e, object))?;
    finish(text, DEFINITION_CAP, composed, object)
}
