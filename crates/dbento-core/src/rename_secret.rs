//! Connections an outside edit of `dbento.toml` renamed, so their saved secrets can follow.
//!
//! A rename shows up as one declared name vanishing and another appearing; the pair is kept
//! only when the two point at the same login and the pairing is unambiguous.

use crate::{
    ConnectionConfig, Engine, SecretError, SecretKey, SecretSource, SecretStore, Workspace,
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
    if !(password || passphrase) {
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
