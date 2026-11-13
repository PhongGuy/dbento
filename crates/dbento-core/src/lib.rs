//! UI-free core of Dbento: the workspace format, secrets and SQL execution.
//!
//! The CLI, the MCP server and the desktop app are thin shells over this crate's public API.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::wildcard_enum_match_arm)]

mod value;
pub use value::{CellRef, ColumnInfo, ColumnSource, DocumentRef, ResultBatch};

mod folder;
pub use folder::{FolderPath, FolderPathError, WorkspaceItem};

mod workspace;
pub use workspace::{
    AuthLevel, Color, ConnectionConfig, Databases, Engine, Folder, InitError, MAX_QUERY_FILE_BYTES,
    OpenError, QueryFileError, ResolveError, SavedQuery, SecretSource, TlsMode, Workspace,
    valid_database_name,
};

mod form;
pub use form::{ConnectionForm, FieldKind, FormField, FormSection, connection_form};

mod edit;
mod write;
pub use edit::{ConnectionSpec, Edit, EditError, EditPlan, WriteError, plan_edit};

mod approval;
pub use approval::{
    ApprovalError, ApprovalRequest, ApprovalStore, Claim, Decision, GrantState,
    MAX_DENY_REASON_CHARS, Verdict, approval_id,
};

mod history;
pub use history::{
    HISTORY_COMPACT_BYTES, HISTORY_COMPACT_LINES, HISTORY_KEEP_BYTES, HISTORY_KEEP_LINES,
    HISTORY_MAX_MESSAGE_BYTES, HISTORY_MAX_SQL_BYTES, History, HistoryEntry, HistoryError,
    HistoryFilter, HistoryOutcome, HistorySource,
};

pub mod paths;

mod secrets;
pub use secrets::{
    MemoryStore, Secret, SecretError, SecretKey, SecretStore, TimedStore, key_passphrase_env_var,
    move_secret, resolve_key_passphrase, resolve_secret, secret_env_var, secret_hint,
    tls_key_is_encrypted,
};

#[cfg(feature = "keychain")]
mod keychain;
#[cfg(feature = "keychain")]
pub use keychain::KeychainStore;

mod introspect;
pub use introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, default_schema, quote_ident, quote_literal,
};

mod classify;
pub use classify::leading_keyword;
mod explain;
pub use explain::{
    Actual, ExplainError, ExplainMode, ExplainOutput, ExplainRequest, Plan, PlanNode,
    PlanParseError, explain_request, explain_text,
};
mod fkeys;
pub use fkeys::{FkDirection, ForeignKey};
mod format;
pub use format::{FormatOptions, KeywordCase, format_sql, line_comment_starts, splits_token};
mod objects;
pub use objects::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, IndexInfo,
    ObjectRef, RelationObjects, RoutineInfo, RoutineKind, SchemaObjects, TriggerInfo, TriggerState,
    TypeInfo, TypeKind,
};
mod mongo;
mod mssql;
mod mysql;
mod numtext;
mod postgres;
mod ranges;
mod redis;
pub use crate::redis::browse::{
    KeyBrowseError, KeyCursor, KeyEntry, KeyKind, KeyPage, KeyRead, KeyScan, KeyTtl, KeyValuePage,
    ValueCursor,
};
pub use crate::redis::catalog::{ArgDoc, CommandDoc};
mod tsql;
pub use ranges::{result_ranges, statement_count, statement_ranges};
mod capacity;
pub use capacity::CapacityUnits;
mod session;
mod split;
mod surreal;
pub use session::{
    CancelHandle, Capabilities, Classification, ConnectError, ExecuteError, ExtendError, Flavor,
    Limits, OnTruncate, Outcome, Pending, QueryLanguage, ReadOnlyEnforcement, ResultStream, Script,
    ServerInfo, Session, SessionOptions, TruncatedBy, Unguarded, Writes, connect,
    read_only_residual,
};

mod source;
mod sources;
pub use source::single_source;
pub use sources::named_source_columns;
#[cfg(feature = "duckdb")]
mod duckdb;
mod dynamo;
mod sqlite;

mod rowedit;
pub use rowedit::{
    Applied, ApplyError, BatchError, Browse, BrowseError, Change, ChangeOutcome, EditBatch,
    EditColumn, EditTarget, EditValue, Keyset, RowKey, Sort, apply, apply_with, browse_sql,
    key_filter, render_sql,
};

pub mod import;

/// The crate version, as reported by `dbento --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
