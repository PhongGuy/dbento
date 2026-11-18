//! Folder paths and workspace tree items, kept at their old path; they live in `dbento-workspace`.

pub use dbento_workspace::{FolderPath, FolderPathError, WorkspaceItem};
pub(crate) use dbento_workspace::{is_invisible, visible};
