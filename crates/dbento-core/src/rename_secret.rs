//! Connections an outside edit of `dbento.toml` renamed, so their saved secrets can follow.
//!
//! A rename shows up as one declared name vanishing and another appearing; the pair is kept
//! only when the two point at the same login and the pairing is unambiguous.

use crate::{
    ConnectionConfig, Engine, SecretError, SecretKey, SecretSource, SecretStore, SshHome,
    Workspace, resolve_ssh,
};

/// How closely a renamed connection matches the one it replaced.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameMatch {
    /// Every session setting is the same.
    Exact,
    /// Server and user are the same, but some session settings changed.
    SameLogin,
}

/// One connection renamed between two versions of a workspace, with its secret slots.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRename {
    /// The old name.
    pub from: String,
    /// The new name.
    pub to: String,
    /// How closely the two connections match.
    pub matched: RenameMatch,
    /// The old connection's display target.
    pub from_target: String,
    /// The new connection's display target.
    pub to_target: String,
    /// The dbento.toml keys that differ; empty when `matched` is `Exact`.
    pub changed: Vec<&'static str>,
    /// Both sides keep a password in the keychain.
    pub password: bool,
    /// Both sides have a client key whose passphrase may be saved.
    pub passphrase: bool,
    /// Both sides have an ssh table, so the SSH password and key passphrase may be saved.
    pub ssh: bool,
}

/// The keychain connections of `old` that `new` appears to have renamed, sorted by `from`.
pub fn secret_renames(old: &Workspace, new: &Workspace) -> Vec<SecretRename> {
    let gone = vanished(old, new);
    let came = vanished(new, old);
    let candidates: Vec<SecretRename> = gone
        .iter()
        .flat_map(|a| came.iter().filter_map(move |b| pair(a, b)))
        .collect();
    // A name claimed by more than one pair, at any match level, is ambiguous: drop all its pairs.
    let count = |name: &str, side: fn(&SecretRename) -> &str| {
        candidates.iter().filter(|r| side(r) == name).count()
    };
    let mut renames: Vec<SecretRename> = candidates
        .iter()
        .filter(|r| count(&r.from, |r| &r.from) == 1 && count(&r.to, |r| &r.to) == 1)
        .cloned()
        .collect();
    renames.sort_by(|a, b| a.from.cmp(&b.from));
    renames
}

/// The largest snapshot `seen_text` returns, in bytes.
pub const SEEN_TEXT_MAX: usize = 256 * 1024;

/// The text the app keeps as `ws`'s last-seen connection list: its validated dbento.toml
/// re-serialized without comments. None when serializing fails, the result is over
/// SEEN_TEXT_MAX bytes, or it does not validate again as a workspace with the same declared names.
pub fn seen_text(ws: &Workspace) -> Option<String> {
    // A round trip through a plain table keeps every key but no comment, so nothing a user
    // wrote beside a setting ends up in the app's state file.
    let table: toml::Table = toml::from_str(ws.source_text()).ok()?;
    let text = toml::to_string(&table).ok()?;
    if text.len() > SEEN_TEXT_MAX {
        return None;
    }
    let back = seen_workspace(&text, ws)?;
    let same = back.connections().len() == ws.connections().len()
        && back
            .connections()
            .iter()
            .zip(ws.connections())
            .all(|(a, b)| a.name() == b.name());
    same.then_some(text)
}

/// `text` validated as an earlier dbento.toml of `current`'s folder, so `secret_renames`
/// can compare it with `current`. None when it does not validate under today's rules.
pub fn seen_workspace(text: &str, current: &Workspace) -> Option<Workspace> {
    let dir = current.dir().to_path_buf();
    let file = dir.join("dbento.toml");
    Workspace::from_text(dir, &file, text).ok()
}

/// Declared connections of `this` missing by name from `other`, file engines left out.
fn vanished<'a>(this: &'a Workspace, other: &Workspace) -> Vec<&'a ConnectionConfig> {
    this.connections()
        .iter()
        .filter(|c| !matches!(c.engine(), Engine::Sqlite | Engine::Duckdb))
        .filter(|c| !other.connections().iter().any(|o| o.name() == c.name()))
        .collect()
}

/// The rename `a` to `b`, when both hold a secret slot and they share a login.
fn pair(a: &ConnectionConfig, b: &ConnectionConfig) -> Option<SecretRename> {
    let password = keychain_backed(a) && keychain_backed(b);
    let passphrase = a.tls_key().is_some() && b.tls_key().is_some();
    let ssh = a.ssh().is_some() && b.ssh().is_some();
    if !(password || passphrase || ssh) {
        return None;
    }
    let (matched, changed) = if a.same_session(b) {
        (RenameMatch::Exact, Vec::new())
    } else if a.same_login(b) {
        (RenameMatch::SameLogin, a.differing_keys(b))
    } else {
        return None;
    };
    Some(SecretRename {
        from: a.name().to_owned(),
        to: b.name().to_owned(),
        matched,
        from_target: a.display_target(),
        to_target: b.display_target(),
        changed,
        password,
        passphrase,
        ssh,
    })
}

/// Whether the connection's password lives in the keychain.
fn keychain_backed(c: &ConnectionConfig) -> bool {
    c.engine().capabilities().password && matches!(c.secret_source(), SecretSource::Keychain)
}

/// What `adopt_secret` did.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adopted {
    /// The secret now lives under the new name and the old entry is gone.
    Moved,
    /// The old name held no secret, so nothing was written.
    NothingStored,
    /// The new name already held a secret, so neither entry was touched.
    TargetTaken,
}

/// Moves `from`'s secret to `to` unless `to` already holds one; `from` is deleted only after
/// `to` is written, so a failed delete leaves the secret under both names, never under none.
pub fn adopt_secret(
    store: &dyn SecretStore,
    from: &SecretKey,
    to: &SecretKey,
) -> Result<Adopted, SecretError> {
    // Checking the target first also covers `from == to`: a held secret is never rewritten.
    if store.get(to)?.is_some() {
        return Ok(Adopted::TargetTaken);
    }
    if from == to {
        return Ok(Adopted::NothingStored);
    }
    let Some(secret) = store.get(from)? else {
        return Ok(Adopted::NothingStored);
    };
    store.set(to, &secret)?;
    store.delete(from)?;
    Ok(Adopted::Moved)
}

/// Where `adopt_ssh_secrets` left the pinned SSH password.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshPasswordMove {
    /// Both sides log in as the same `user@host:port`, so the entry followed the rename.
    Moved(Adopted),
    /// The entry stays under the old name; `message` says why and names
    /// `dbento secret set <to> --ssh-password`.
    Kept {
        /// Why the entry stayed, and the command that saves it for the new name.
        message: String,
    },
}

/// What `adopt_ssh_secrets` did with each SSH slot.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAdopted {
    /// The password, pinned to the resolved login.
    pub password: SshPasswordMove,
    /// The `identity_file` passphrase, which is not pinned.
    pub key_passphrase: Adopted,
}

/// Moves both SSH slots of `r`. The password moves only when both sides resolve (against
/// `home`) to the same login; otherwise it stays and the result says so.
pub fn adopt_ssh_secrets(
    store: &dyn SecretStore,
    old: &Workspace,
    new: &Workspace,
    r: &SecretRename,
    home: &SshHome,
) -> Result<SshAdopted, SecretError> {
    let (Some(from), Some(to)) = (declared(old, &r.from), declared(new, &r.to)) else {
        return Ok(SshAdopted {
            password: kept(r, "the connection is no longer declared"),
            key_passphrase: Adopted::NothingStored,
        });
    };
    let key_passphrase = adopt_secret(
        store,
        &SecretKey::ssh_key_passphrase_for(old, from),
        &SecretKey::ssh_key_passphrase_for(new, to),
    )?;

    // The password is pinned to the login it was saved for, so it follows only when the new
    // name still logs in to the same place; any doubt leaves it where it is.
    let resolve = |ws: &Workspace, conn: &ConnectionConfig| {
        resolve_ssh(conn, ws.dir(), home)
            .map(|resolved| resolved.login())
            .map_err(|e| format!("`{}` does not resolve: {e}", conn.name()))
    };
    let password = match (resolve(old, from), resolve(new, to)) {
        (Ok(was), Ok(now)) if was == now => SshPasswordMove::Moved(adopt_secret(
            store,
            &SecretKey::ssh_password_for(old, from, &was),
            &SecretKey::ssh_password_for(new, to, &now),
        )?),
        (Ok(was), Ok(now)) => kept(r, &format!("it logged in as {was}, `{}` as {now}", r.to)),
        (Err(reason), _) | (_, Err(reason)) => kept(r, &reason),
    };
    Ok(SshAdopted {
        password,
        key_passphrase,
    })
}

/// The connection `ws` declares as `name`, derived children left out.
fn declared<'a>(ws: &'a Workspace, name: &str) -> Option<&'a ConnectionConfig> {
    ws.connections().iter().find(|c| c.name() == name)
}

/// The `Kept` result for `r`'s SSH password, with `reason` and the command that sets it anew.
fn kept(r: &SecretRename, reason: &str) -> SshPasswordMove {
    SshPasswordMove::Kept {
        message: format!(
            "the SSH password of `{}` stays under its old entry: {reason}; run `dbento secret set {} --ssh-password`",
            r.from, r.to
        ),
    }
}
