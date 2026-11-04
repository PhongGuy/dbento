//! Schema objects beyond relations: keys, indexes, checks, triggers, types and routines.

use crate::introspect::{IntrospectError, RelationRef};
use crate::session::ExecuteError;

mod pg;
mod sqlite;
pub(crate) use pg::{pg_definition, pg_relation_objects, pg_schema_objects};
pub(crate) use sqlite::{sqlite_definition, sqlite_relation_objects};

/// The most bytes of definition text one lookup returns.
pub const DEFINITION_CAP: usize = 1 << 20;

/// What kind of table constraint a key or check is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintKind {
    /// A primary key.
    PrimaryKey,
    /// A unique key.
    Unique,
    /// A foreign key.
    ForeignKey,
    /// A check constraint.
    Check,
    /// An exclusion constraint (Postgres).
    Exclusion,
    /// Any other kind the engine reports.
    Other,
}

/// What kind of user-defined type a type is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    /// An enum type.
    Enum,
    /// A domain over another type.
    Domain,
    /// A composite (row) type.
    Composite,
    /// A range type.
    Range,
    /// Any other kind the engine reports.
    Other,
}

/// What kind of routine a routine is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutineKind {
    /// A function.
    Function,
    /// A procedure.
    Procedure,
    /// An aggregate function.
    Aggregate,
}

/// Names one schema object whose definition can be asked for.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectRef {
    /// A table, view or other relation.
    #[non_exhaustive]
    Relation(RelationRef),
    /// An index on a relation.
    #[non_exhaustive]
    Index {
        /// The indexed relation.
        rel: RelationRef,
        /// The index name.
        name: String,
    },
    /// A constraint on a relation; nameless where the engine keeps no name.
    #[non_exhaustive]
    Constraint {
        /// The constrained relation.
        rel: RelationRef,
        /// The constraint name, if any.
        name: Option<String>,
        /// The constraint kind.
        kind: ConstraintKind,
    },
    /// A trigger on a relation.
    #[non_exhaustive]
    Trigger {
        /// The relation the trigger fires on.
        rel: RelationRef,
        /// The trigger name.
        name: String,
    },
    /// A user-defined type.
    #[non_exhaustive]
    Type {
        /// The type's schema.
        schema: String,
        /// The type name.
        name: String,
    },
    /// A routine, told apart from its overloads by its identity arguments.
    #[non_exhaustive]
    Routine {
        /// The routine's schema.
        schema: String,
        /// The routine name.
        name: String,
        /// The identity argument list, as the engine prints it.
        arguments: String,
    },
}

impl ObjectRef {
    /// The reference to a relation's own definition.
    pub fn relation(rel: RelationRef) -> Self {
        Self::Relation(rel)
    }

    /// The object as errors name it: its kind, its name and the relation or schema it is in.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Relation(rel) => format!("relation {}", qualified(rel)),
            Self::Index { rel, name } => format!("index {name} on {}", qualified(rel)),
            Self::Constraint {
                rel,
                name: Some(name),
                ..
            } => format!("constraint {name} on {}", qualified(rel)),
            Self::Constraint {
                rel,
                name: None,
                kind,
            } => {
                let words = match kind {
                    ConstraintKind::PrimaryKey => "primary key",
                    ConstraintKind::Unique => "unique key",
                    ConstraintKind::ForeignKey => "foreign key",
                    ConstraintKind::Check => "check",
                    ConstraintKind::Exclusion => "exclusion constraint",
                    ConstraintKind::Other => "constraint",
                };
                format!("{words} on {}", qualified(rel))
            }
            Self::Trigger { rel, name } => format!("trigger {name} on {}", qualified(rel)),
            Self::Type { schema, name } => format!("type {schema}.{name}"),
            Self::Routine {
                schema,
                name,
                arguments,
            } => format!("routine {schema}.{name}({arguments})"),
        }
    }
}

/// `schema.name` of `rel`, unquoted, as labels and errors write it.
pub(crate) fn qualified(rel: &RelationRef) -> String {
    format!("{}.{}", rel.schema(), rel.name())
}

/// The text contract: None is Missing; trailing whitespace trimmed; a text longer than `cap`
/// bytes before trimming, or whose complete form (with one `;` added when it lacks one) exceeds
/// `cap`, is cut at a char boundary at or below `cap`, marked truncated and gets no `;`.
pub(crate) fn finish(
    text: Option<String>,
    cap: usize,
    composed: bool,
    object: &ObjectRef,
) -> Result<Definition, DefinitionError> {
    let Some(text) = text else {
        return Err(DefinitionError::Missing {
            object: object.label(),
        });
    };
    // The fetch stops just past the cap, so trailing whitespace may sit at the cut itself.
    let over = text.len() > cap;
    let text = text.trim_end();
    let semicolon = !text.ends_with(';');
    if !over && text.len() + usize::from(semicolon) <= cap {
        let end = if semicolon { ";" } else { "" };
        return Ok(Definition::new(format!("{text}{end}"), false, composed));
    }
    let cut = &text[..text.floor_char_boundary(cap)];
    Ok(Definition::new(cut, true, composed))
}

/// A catalog failure as a definition failure: XX000 "cache lookup failed" is Missing (a drop
/// race), Execute and Unexpected pass through, IntrospectError::Missing becomes Missing.
pub(crate) fn definition_error(e: IntrospectError, object: &ObjectRef) -> DefinitionError {
    match e {
        IntrospectError::Execute(ExecuteError::Database { code, message, .. })
            if code == "XX000" && message.contains("cache lookup failed") =>
        {
            DefinitionError::Missing {
                object: object.label(),
            }
        }
        IntrospectError::Execute(e) => DefinitionError::Execute(e),
        IntrospectError::Unexpected { message } => DefinitionError::Unexpected { message },
        IntrospectError::Missing { .. } => DefinitionError::Missing {
            object: object.label(),
        },
        IntrospectError::Unsupported { .. } => DefinitionError::Unsupported {
            what: object.label(),
        },
    }
}

/// The keys, indexes, checks and triggers of one relation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationObjects {
    indexes: Vec<IndexInfo>,
    constraints: Vec<ConstraintInfo>,
    triggers: Vec<TriggerInfo>,
    unreadable: Option<String>,
}

impl RelationObjects {
    /// A relation's objects, each list in its listed order.
    pub fn new(
        indexes: Vec<IndexInfo>,
        constraints: Vec<ConstraintInfo>,
        triggers: Vec<TriggerInfo>,
    ) -> Self {
        Self {
            indexes,
            constraints,
            triggers,
            unreadable: None,
        }
    }

    /// A relation that exists but whose objects the engine cannot read, and why.
    pub fn unreadable(reason: impl Into<String>) -> Self {
        Self {
            unreadable: Some(reason.into()),
            ..Self::new(Vec::new(), Vec::new(), Vec::new())
        }
    }

    /// Why the objects could not be read; None when the lists are the relation's own.
    pub fn unreadable_reason(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    /// The relation's indexes, constraint-backed ones included.
    pub fn indexes(&self) -> &[IndexInfo] {
        &self.indexes
    }

    /// The relation's keys and checks.
    pub fn constraints(&self) -> &[ConstraintInfo] {
        &self.constraints
    }

    /// The relation's triggers.
    pub fn triggers(&self) -> &[TriggerInfo] {
        &self.triggers
    }
}

/// One index of a relation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexInfo {
    rel: RelationRef,
    name: String,
    columns: Vec<String>,
    unique: bool,
    primary: bool,
    partial: bool,
    method: Option<String>,
}

impl IndexInfo {
    /// An index of `rel` over `columns` (a name or an expression's text each).
    pub fn new(
        rel: RelationRef,
        name: impl Into<String>,
        columns: Vec<String>,
        unique: bool,
        primary: bool,
        partial: bool,
        method: Option<String>,
    ) -> Self {
        Self {
            rel,
            name: name.into(),
            columns,
            unique,
            primary,
            partial,
            method,
        }
    }

    /// The index name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The key columns or expressions, in key order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Whether the index enforces uniqueness.
    pub fn unique(&self) -> bool {
        self.unique
    }

    /// Whether the index backs the primary key.
    pub fn primary(&self) -> bool {
        self.primary
    }

    /// Whether the index has a WHERE predicate.
    pub fn partial(&self) -> bool {
        self.partial
    }

    /// The access method, such as `btree`; None where the engine has one kind.
    pub fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// The reference that asks for this index's definition.
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::Index {
            rel: self.rel.clone(),
            name: self.name.clone(),
        }
    }
}

/// One key or check of a relation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstraintInfo {
    rel: RelationRef,
    name: Option<String>,
    kind: ConstraintKind,
    columns: Vec<String>,
    referenced: Option<RelationRef>,
}

impl ConstraintInfo {
    /// A constraint of `rel`; `referenced` is the far relation of a foreign key.
    pub fn new(
        rel: RelationRef,
        name: Option<String>,
        kind: ConstraintKind,
        columns: Vec<String>,
        referenced: Option<RelationRef>,
    ) -> Self {
        Self {
            rel,
            name,
            kind,
            columns,
            referenced,
        }
    }

    /// The constraint name; None where the engine keeps none.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The constraint kind.
    pub fn kind(&self) -> ConstraintKind {
        self.kind
    }

    /// The constrained columns, in key order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The relation a foreign key references; None for other kinds.
    pub fn referenced(&self) -> Option<&RelationRef> {
        self.referenced.as_ref()
    }

    /// The reference that asks for this constraint's definition.
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::Constraint {
            rel: self.rel.clone(),
            name: self.name.clone(),
            kind: self.kind,
        }
    }
}

/// When a trigger fires.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerState {
    /// It fires in an ordinary session.
    Enabled,
    /// It never fires.
    Disabled,
    /// It fires only while the session replays replicated changes.
    ReplicaOnly,
}

/// One trigger of a relation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TriggerInfo {
    rel: RelationRef,
    name: String,
    state: TriggerState,
}

impl TriggerInfo {
    /// A trigger on `rel`.
    pub fn new(rel: RelationRef, name: impl Into<String>, state: TriggerState) -> Self {
        Self {
            rel,
            name: name.into(),
            state,
        }
    }

    /// The trigger name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// When the trigger fires.
    pub fn state(&self) -> TriggerState {
        self.state
    }

    /// Whether the trigger fires in an ordinary session.
    pub fn enabled(&self) -> bool {
        self.state == TriggerState::Enabled
    }

    /// The reference that asks for this trigger's definition.
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::Trigger {
            rel: self.rel.clone(),
            name: self.name.clone(),
        }
    }
}

/// The types and routines of one schema.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaObjects {
    types: Vec<TypeInfo>,
    routines: Vec<RoutineInfo>,
    truncated: bool,
}

impl SchemaObjects {
    /// A schema's objects; `truncated` when either list hit its cap.
    pub fn new(types: Vec<TypeInfo>, routines: Vec<RoutineInfo>, truncated: bool) -> Self {
        Self {
            types,
            routines,
            truncated,
        }
    }

    /// The schema's user-defined types.
    pub fn types(&self) -> &[TypeInfo] {
        &self.types
    }

    /// The schema's functions, procedures and aggregates.
    pub fn routines(&self) -> &[RoutineInfo] {
        &self.routines
    }

    /// Whether either list stopped at its cap.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// One user-defined type of a schema.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeInfo {
    schema: String,
    name: String,
    kind: TypeKind,
}

impl TypeInfo {
    /// A type in `schema`.
    pub fn new(schema: impl Into<String>, name: impl Into<String>, kind: TypeKind) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
            kind,
        }
    }

    /// The type name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The type kind.
    pub fn kind(&self) -> TypeKind {
        self.kind
    }

    /// The reference that asks for this type's definition.
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::Type {
            schema: self.schema.clone(),
            name: self.name.clone(),
        }
    }
}

/// One function, procedure or aggregate of a schema.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoutineInfo {
    schema: String,
    name: String,
    kind: RoutineKind,
    arguments: String,
    returns: Option<String>,
}

impl RoutineInfo {
    /// A routine in `schema` with its identity arguments and result type.
    pub fn new(
        schema: impl Into<String>,
        name: impl Into<String>,
        kind: RoutineKind,
        arguments: impl Into<String>,
        returns: Option<String>,
    ) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
            kind,
            arguments: arguments.into(),
            returns,
        }
    }

    /// The routine name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The routine kind.
    pub fn kind(&self) -> RoutineKind {
        self.kind
    }

    /// The identity argument list, as the engine prints it.
    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    /// The result type; None for a procedure.
    pub fn returns(&self) -> Option<&str> {
        self.returns.as_deref()
    }

    /// The reference that asks for this routine's definition.
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::Routine {
            schema: self.schema.clone(),
            name: self.name.clone(),
            arguments: self.arguments.clone(),
        }
    }
}

/// The text that recreates one object, as the engine reports it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    text: String,
    truncated: bool,
    composed: bool,
}

impl Definition {
    /// A definition; `composed` when it was assembled from catalog parts.
    pub fn new(text: impl Into<String>, truncated: bool, composed: bool) -> Self {
        Self {
            text: text.into(),
            truncated,
            composed,
        }
    }

    /// The definition text, at most [`DEFINITION_CAP`] bytes.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the text stopped at the cap.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Whether the text was assembled from catalog parts rather than stored.
    pub fn composed(&self) -> bool {
        self.composed
    }
}

/// Why an object's definition could not be read.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum DefinitionError {
    /// The catalog query failed.
    #[error(transparent)]
    Execute(#[from] ExecuteError),
    /// The catalog answered in a shape the lookup does not expect.
    #[error("unexpected catalog answer: {message}")]
    Unexpected {
        /// What was unexpected.
        message: String,
    },
    /// The object was dropped or renamed since it was listed.
    #[error("{object} no longer exists")]
    Missing {
        /// The object, as its label reads.
        object: String,
    },
    /// The engine keeps no definition for this kind of object.
    #[error("{what} has no definition on this engine")]
    Unsupported {
        /// The object, as its label reads.
        what: String,
    },
}

#[cfg(test)]
mod tests;
