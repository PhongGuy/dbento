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
    AuthLevel, Color, ConnectionConfig, Databases, DurationError, Engine, Folder,
    FormattedDuration, InitError, MAX_QUERY_FILE_BYTES, OpenError, QueryFileError, ResolveError,
    SavedQuery, SecretSource, SshAuth, SshConfig, TlsMode, Workspace, format_duration,
    parse_duration, valid_database_name,
};

mod ssh;
pub use ssh::{
    ForgetHostKeyError, HostKey, HostKeyProblem, HostKeyStatus, ResolvedSsh, SshEnv, SshHome,
    SshResolveError, TrustHostKeyError, forget_host_key, probe_host_key, resolve_ssh,
    ssh_channel_check, ssh_warnings, trust_host_key, user_known_hosts_lists,
};

mod form;
pub use form::{ConnectionForm, FieldKind, FormField, FormSection, connection_form};

mod edit;
mod write;
pub use edit::{ConnectionSpec, Edit, EditError, EditPlan, SshSpec, WriteError, plan_edit};

mod approval;
pub use approval::{
    ApprovalError, ApprovalRequest, ApprovalStore, ApprovalTarget, Claim, Decision, GrantState,
    MAX_DENY_REASON_CHARS, Verdict, approval_id,
};

mod history;
pub use history::{
    HISTORY_COMPACT_BYTES, HISTORY_COMPACT_LINES, HISTORY_KEEP_BYTES, HISTORY_KEEP_LINES,
    HISTORY_MAX_MESSAGE_BYTES, HISTORY_MAX_SQL_BYTES, History, HistoryEntry, HistoryError,
    HistoryFilter, HistoryOutcome, HistorySource,
};

pub mod paths;
pub use paths::StatePaths;

mod rename_secret;
pub use rename_secret::{
    Adopted, RenameMatch, SEEN_TEXT_MAX, SecretRename, SshAdopted, SshPasswordMove, adopt_secret,
    adopt_ssh_secrets, secret_renames, seen_text, seen_workspace,
};

mod secrets;
pub use secrets::{
    MemoryStore, Secret, SecretError, SecretKey, SecretStore, SshSecrets, TimedStore,
    key_passphrase_env_var, move_secret, resolve_key_passphrase, resolve_secret,
    resolve_ssh_secrets, secret_env_var, secret_hint, ssh_key_passphrase_env_var,
    ssh_password_env_var, tls_key_is_encrypted,
};

#[cfg(feature = "keychain")]
mod keychain;
#[cfg(feature = "keychain")]
pub use keychain::KeychainStore;

#[cfg(feature = "update")]
pub mod update;

#[cfg(feature = "watch")]
mod watch;
#[cfg(feature = "watch")]
pub use watch::{WATCH_DEBOUNCE, WatchError, WatchEvent, WorkspaceWatcher};

#[cfg(feature = "team")]
mod git;
#[cfg(feature = "team")]
pub use git::{
    CancelFlag, CloneError, ConflictFile, ConflictKind, Finding, GitError, GitOptions, GitVersion,
    Pulled, PushOutcome, Refusal, RepoLock, SAVE_MESSAGE, SyncError, clone, commit_pending,
    git_available, git_available_with, parse_refusal, pull_rebase, push, redact_token, remote_tip,
    repo_lock, rule_text, squash_unpushed, unpushed,
};
#[cfg(feature = "team")]
mod team;
#[cfg(feature = "team")]
pub use keychain::KeychainTokenStore;
#[cfg(feature = "team")]
pub use team::{
    AccountRecord, ApiError, Binding, BindingRecord, ConflictStore, Credential, FolderSync,
    FolderSyncError, JoinError, LoginError, LogoutError, LogoutReport, MemoryTokenStore,
    PotluckClient, PotluckInfo, ReadBlock, RefreshError, RemoteWorkspace, Resolution, Role,
    ServerUrl, ServerUrlError, SignIn, SyncConfig, SyncOutcome, SyncState, SyncStatus, TeamAccount,
    TeamData, TeamError, TeamFile, TeamFileError, TeamResolveError, TeamSync, TeamToken,
    TimedTokenStore, TokenStore, WorkspacesError, public_url_of, refresh_role, sync_once,
    team_join, team_login, team_logout, team_resolve, team_sync, team_workspaces,
};

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
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, DefinitionKind,
    IndexInfo, ObjectRef, RelationObjects, RoutineInfo, RoutineKind, SchemaObjects, TriggerInfo,
    TriggerState, TypeInfo, TypeKind,
};
pub mod message;
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
#[cfg(any(feature = "dynamodb", feature = "s3"))]
mod aws;
#[cfg(feature = "duckdb")]
mod duckdb;
mod dynamo;
mod s3;
pub use s3::names::{display_key, suggested_file_name};
pub use s3::{
    DEFAULT_MAX_DOWNLOAD, DownloadError, DownloadOptions, DownloadReport, MAX_READ_BYTES,
    ObjectBrowseError, ObjectChunk, ObjectCursor, ObjectKey, ObjectList, ObjectMeta, ObjectPage,
    ObjectRead,
};
mod sqlite;

mod rowedit;
pub use rowedit::{
    Applied, ApplyError, BatchError, Browse, BrowseError, Change, ChangeOutcome, EditBatch,
    EditColumn, EditTarget, EditValue, Keyset, NotEditable, RereadError, RowKey, Sort, apply,
    apply_with, browse_sql, key_filter, render_sql, reread,
};

pub mod import;

/// The crate version, as reported by `dbento --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
