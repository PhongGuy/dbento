//! The reusable import layer: an importer reads into an `ImportSet`, `plan` decides every
//! action against a workspace, and `ImportPlan::apply` writes it. No password is ever written.

mod color;
mod model;
mod mysql;
mod names;
mod pg;
mod plan;
mod sqlite;
mod store;
mod tls;

pub use model::{
    ConnectionDraft, ImportSet, ImportedConnection, ImportedQuery, ItemKind, SecretSetting,
    SkipReason, Skipped, SourceTls, Warning, WarningKind,
};
pub use plan::{Action, Applied, ApplyError, ImportPlan, Imported, OnConflict, PlanError, plan};
pub use store::{ImportSecrets, SecretOutcome, StoredSecrets, store_secrets};

pub mod beekeeper;
pub mod datagrip;
pub mod dbeaver;
