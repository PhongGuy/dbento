//! The `dbento.toml` model, kept at its old path; it lives in `dbento-workspace`.

#[cfg(feature = "surrealdb")]
pub(crate) use dbento_workspace::is_identifier;
pub use dbento_workspace::{
    AuthLevel, Color, ConnectionConfig, Databases, Engine, Folder, InitError, MAX_QUERY_FILE_BYTES,
    OpenError, QueryFileError, ResolveError, SavedQuery, SecretSource, TEMPLATE, TlsMode,
    Workspace, valid_database_name,
};
pub(crate) use dbento_workspace::{color_word, is_local_host, timeout_in_range, valid_name};
