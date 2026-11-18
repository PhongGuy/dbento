//! The `dbento.toml` workspace format of Dbento: the connection model, its parser and validator,
//! and the `queries/` folder layout.
//!
//! The crate holds no database driver and no async runtime, so a tool can read and check a
//! workspace without pulling in either. Its non-error enums are closed vocabularies of the file
//! format: a new variant is a format change and a breaking release. The error enums stay
//! non-exhaustive, and each error keeps a stable code.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::wildcard_enum_match_arm)]

mod folder;
mod kinds;
mod mongo_uri;
mod secret_key;
mod spec;
mod workspace;

pub use folder::{FolderPath, FolderPathError, WorkspaceItem};
#[doc(hidden)]
pub use folder::{is_invisible, visible};
pub use kinds::{Capabilities, ConstraintKind, DefinitionKind, QueryLanguage, ReadOnlyEnforcement};
#[doc(hidden)]
pub use mongo_uri::BAD_PORT as MONGO_BAD_PORT;
pub use secret_key::SecretKey;
#[doc(hidden)]
pub use secret_key::Slot;
pub use spec::ConnectionSpec;
pub use workspace::{
    AuthLevel, Color, ConnectionConfig, Databases, Engine, Folder, InitError, MAX_QUERY_FILE_BYTES,
    OpenError, QueryFileError, ResolveError, SavedQuery, SecretSource, TEMPLATE, TlsMode,
    Workspace, valid_database_name,
};
#[doc(hidden)]
pub use workspace::{color_word, is_identifier, is_local_host, timeout_in_range, valid_name};
