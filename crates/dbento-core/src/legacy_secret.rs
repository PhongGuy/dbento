//! Passwords an older Dbento saved under the bare connection name, before they were pinned
//! to the server they belong to.

use crate::rename_secret::keychain_backed;
use crate::{
    Adopted, ConnectionConfig, SecretError, SecretKey, SecretStore, Workspace, adopt_secret,
};

/// What `migrate_legacy_passwords` did with one connection.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMigration {
    /// The legacy entry now lives under the connection's pinned key.
    Pinned,
    /// A pinned entry already existed, so the legacy one was deleted.
    LegacyDropped,
    /// Neither entry held a password.
    NothingStored,
}

/// One result per declared connection of `baseline` with a keychain password, in file order.
pub fn migrate_legacy_passwords(
    store: &dyn SecretStore,
    baseline: &Workspace,
) -> Vec<(String, Result<LegacyMigration, SecretError>)> {
    baseline
        .connections()
        .iter()
        .filter(|c| keychain_backed(c))
        .map(|c| (c.name().to_owned(), migrate_one(store, baseline, c)))
        .collect()
}

/// Pins `baseline`'s legacy entry for `conn` to the target `baseline` gives it. The pinned key
/// is written before the legacy one is deleted, so a failed delete leaves both, never neither.
fn migrate_one(
    store: &dyn SecretStore,
    baseline: &Workspace,
    conn: &ConnectionConfig,
) -> Result<LegacyMigration, SecretError> {
    let pinned = SecretKey::for_connection(baseline, conn);
    let legacy = SecretKey::legacy_password(baseline.dir(), conn.name());
    if store.get(&pinned)?.is_some() {
        store.delete(&legacy)?;
        return Ok(LegacyMigration::LegacyDropped);
    }
    let Some(secret) = store.get(&legacy)? else {
        return Ok(LegacyMigration::NothingStored);
    };
    store.set(&pinned, &secret)?;
    store.delete(&legacy)?;
    Ok(LegacyMigration::Pinned)
}

/// Human-confirmed: pins `name`'s legacy entry to `ws`'s current target. A name that is not
/// declared or keeps no keychain password returns `NothingStored` without asking the store.
pub fn adopt_legacy_password(
    store: &dyn SecretStore,
    ws: &Workspace,
    name: &str,
) -> Result<Adopted, SecretError> {
    let Some(conn) = ws.declared(name).filter(|c| keychain_backed(c)) else {
        return Ok(Adopted::NothingStored);
    };
    adopt_secret(
        store,
        &SecretKey::legacy_password(ws.dir(), conn.name()),
        &SecretKey::for_connection(ws, conn),
    )
}
