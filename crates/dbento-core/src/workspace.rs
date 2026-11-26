//! The `dbento.toml` model, kept at its old path; it lives in `dbento-workspace`.

#[cfg(feature = "surrealdb")]
pub(crate) use dbento_workspace::is_identifier;
pub use dbento_workspace::{
    AuthLevel, Color, ConnectionConfig, Databases, DurationError, Engine, Folder,
    FormattedDuration, InitError, MAX_QUERY_FILE_BYTES, OpenError, QueryFileError, ResolveError,
    SavedQuery, SecretPin, SecretSource, SshAuth, SshConfig, TEMPLATE, TlsMode, Workspace,
    format_duration, parse_duration, valid_database_name,
};
pub(crate) use dbento_workspace::{color_word, is_local_host, timeout_in_range, valid_name};
