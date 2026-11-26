//! What a written connection edit does to the database password in the keychain: the entry
//! follows a rename only while its secret pin stays, so a password never reaches another target.

use dbento_core::{
    ConnectionConfig, Secret, SecretError, SecretKey, SecretSource, SecretStore, Workspace,
    move_secret,
};

/// What a written connection edit does to its keychain password, applied in order: the move,
/// the typed password stored at `to`, then the drops.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PasswordFollow {
    /// The entry carried to the saved name, when the pin stayed and the name changed.
    pub moved: Option<(SecretKey, SecretKey)>,
    /// Entries nothing may read any more: the old pin's entry first, then the legacy one.
    pub drops: Vec<SecretKey>,
    /// Where a typed password goes; None when the saved connection keeps no keychain password.
    pub to: Option<SecretKey>,
}

/// Only a connection whose engine takes a password and reads the keychain owns a stored secret.
pub fn keychain_backed(conn: &ConnectionConfig) -> bool {
    conn.engine().capabilities().password && matches!(conn.secret_source(), SecretSource::Keychain)
}

// The declared connection `name` in `ws` when it keeps a keychain password.
fn keychain_conn<'a>(ws: &'a Workspace, name: &str) -> Option<&'a ConnectionConfig> {
    ws.declared(name).filter(|c| keychain_backed(c))
}

/// True when `old_name` in `base` and `name` in `now` both keep a keychain password and their
/// secret pins differ.
pub fn pin_moves(base: &Workspace, old_name: &str, now: &Workspace, name: &str) -> bool {
    match (keychain_conn(base, old_name), keychain_conn(now, name)) {
        (Some(was), Some(is)) => was.secret_pin() != is.secret_pin(),
        (Some(_) | None, None) | (None, Some(_)) => false,
    }
}

/// Same pin: the entry moves to the new name. Another pin, or no keychain password after the
/// edit: the old entry drops. `typed`, with a `to`, also drops the legacy entry of `name`.
pub fn password_follow(
    base: &Workspace,
    old_name: &str,
    now: &Workspace,
    name: &str,
    typed: bool,
) -> PasswordFollow {
    let from = keychain_conn(base, old_name).and_then(|_| base.secret_target(old_name));
    let to = keychain_conn(now, name).and_then(|_| now.secret_target(name));
    let mut follow = PasswordFollow {
        to: to.clone(),
        ..PasswordFollow::default()
    };
    if let Some(from) = from {
        match to {
            Some(_) if pin_moves(base, old_name, now, name) => follow.drops.push(from),
            Some(to) if from != to => follow.moved = Some((from, to)),
            Some(_) => {}
            None => follow.drops.push(from),
        }
    }
    // A password typed for the saved name supersedes whatever an older Dbento stored for it.
    if typed && follow.to.is_some() {
        follow
            .drops
            .push(SecretKey::legacy_password(now.dir(), name));
    }
    follow
}

/// Carries out `follow` on `store`: the move, then `typed` at `follow.to` (a typed password
/// without a `to` is not stored), then the drops.
pub fn apply_password_follow(
    store: &dyn SecretStore,
    follow: &PasswordFollow,
    typed: Option<&Secret>,
) -> Result<(), SecretError> {
    if let Some((from, to)) = &follow.moved {
        move_secret(store, from, to)?;
    }
    if let (Some(to), Some(secret)) = (&follow.to, typed) {
        store.set(to, secret)?;
    }
    for key in &follow.drops {
        store.delete(key)?;
    }
    Ok(())
}
