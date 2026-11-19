//! Carries a renamed connection's saved keychain secrets to its new name.
//!
//! The pure pieces: which keychain entries a rename pairs, which of them hold a secret,
//! and the move itself. The shell sequences them after an outside reload.

use std::path::Path;

use dbento_core::{
    ConnectionConfig, RenameMatch, SecretError, SecretKey, SecretRename, SecretStore, Workspace,
    adopt_secret, key_passphrase_env_var, secret_env_var,
};

use crate::app::EnvLookup;
use crate::confirm::{Ask, Choice};

/// The keychain entries one rename moves, each as (key under the old name, key under the new).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameKeys {
    /// The password entry pair, when both sides keep a password in the keychain.
    pub password: Option<(SecretKey, SecretKey)>,
    /// The key passphrase entry pair, when both sides have a client key.
    pub passphrase: Option<(SecretKey, SecretKey)>,
}

/// Per slot: whether the old name holds a secret, and whether the new name already does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    /// The old name holds a password.
    pub password: bool,
    /// The old name holds a key passphrase.
    pub passphrase: bool,
    /// The new name already holds a password.
    pub password_taken: bool,
    /// The new name already holds a key passphrase.
    pub passphrase_taken: bool,
}

impl Held {
    /// Whether some slot holds a secret under the old name and none under the new.
    pub fn movable(&self) -> bool {
        (self.password && !self.password_taken) || (self.passphrase && !self.passphrase_taken)
    }
}

/// The key pairs for `r`'s carried slots, taken only from each workspace's declared targets.
pub fn rename_keys(old: &Workspace, new: &Workspace, r: &SecretRename) -> RenameKeys {
    let both = |a: Option<SecretKey>, b: Option<SecretKey>| a.zip(b);
    RenameKeys {
        password: r
            .password
            .then(|| both(old.secret_target(&r.from), new.secret_target(&r.to)))
            .flatten(),
        passphrase: r
            .passphrase
            .then(|| {
                both(
                    old.key_passphrase_target(&r.from),
                    new.key_passphrase_target(&r.to),
                )
            })
            .flatten(),
    }
}

/// Reads both names of every carried slot; an unavailable keychain reads as empty, as
/// secret resolution treats it, and any other failure is returned.
pub fn probe(store: &dyn SecretStore, keys: &RenameKeys) -> Result<Held, SecretError> {
    let has = |key: &SecretKey| match store.get(key) {
        Ok(found) => Ok(found.is_some()),
        Err(SecretError::Unavailable { .. }) => Ok(false),
        Err(e) => Err(e),
    };
    let mut held = Held::default();
    if let Some((from, to)) = &keys.password {
        held.password = has(from)?;
        held.password_taken = has(to)?;
    }
    if let Some((from, to)) = &keys.passphrase {
        held.passphrase = has(from)?;
        held.passphrase_taken = has(to)?;
    }
    Ok(held)
}

/// Moves each slot `held` marks as present and not taken; a slot that turns out taken or
/// empty by now is left alone.
pub fn adopt_held(
    store: &dyn SecretStore,
    keys: &RenameKeys,
    held: Held,
) -> Result<(), SecretError> {
    let slots = [
        (&keys.password, held.password && !held.password_taken),
        (&keys.passphrase, held.passphrase && !held.passphrase_taken),
    ];
    for (pair, wanted) in slots {
        if let (Some((from, to)), true) = (pair, wanted) {
            // TargetTaken and NothingStored both leave the store as it was, which is fine here.
            adopt_secret(store, from, to)?;
        }
    }
    Ok(())
}

/// The question for a SameLogin rename: Keep (0, default and cancel) or Move (1).
pub fn move_ask(r: &SecretRename, held: Held) -> Ask {
    // Name only the slots the move would carry: held under the old name, free under the new.
    let what = match (
        held.password && !held.password_taken,
        held.passphrase && !held.passphrase_taken,
    ) {
        (true, true) => "password and key passphrase",
        (false, true) => "key passphrase",
        (true, false) | (false, false) => "password",
    };
    let detail = format!(
        "{from} was renamed to {to} in dbento.toml, and these settings changed too: {keys}. \
         Saved {what} for: {from_target}. Now: {to_target}. Move it only if this is the same login.",
        from = r.from,
        to = r.to,
        keys = r.changed.join(", "),
        from_target = r.from_target,
        to_target = r.to_target,
    );
    Ask {
        title: format!("Move the saved secret to {}?", r.to).into(),
        detail: Some(detail.into()),
        sql: None,
        language: "text",
        choices: vec![
            Choice {
                id: "rename-secret-keep",
                label: format!("Keep under {}", r.from).into(),
                danger: false,
            },
            Choice {
                id: "rename-secret-move",
                label: "Move".into(),
                danger: false,
            },
        ],
        default: 0,
        cancel: 0,
    }
}

/// Whether `now` still says what was asked: same directory, `to` declared and same_session
/// with `asked`, `from` undeclared.
pub fn still_renamed(
    now: &Workspace,
    dir: &Path,
    r: &SecretRename,
    asked: &ConnectionConfig,
) -> bool {
    now.dir() == dir
        && now.declared(&r.from).is_none()
        && now.declared(&r.to).is_some_and(|c| c.same_session(asked))
}

/// One line per carried slot with nothing under either name in the keychain whose old variable
/// is set and new one is not; only whether a variable is set is read.
pub fn env_lines(r: &SecretRename, held: Held, env: &EnvLookup) -> Vec<String> {
    let set = |var: &str| env(var).is_some();
    let slots = [
        (
            r.password,
            held.password || held.password_taken,
            secret_env_var as fn(&str) -> String,
        ),
        (
            r.passphrase,
            held.passphrase || held.passphrase_taken,
            key_passphrase_env_var,
        ),
    ];
    // A SameLogin line repeats the targets, since the user decides whether it is the same login.
    let tail = if r.matched == RenameMatch::Exact {
        String::new()
    } else {
        format!(
            ": saved for {}, now {} ({} changed); rename it only if this is the same login",
            r.from_target,
            r.to_target,
            r.changed.join(", "),
        )
    };
    slots
        .into_iter()
        .filter(|(carried, in_keychain, _)| *carried && !in_keychain)
        .map(|(_, _, var)| (var(&r.from), var(&r.to)))
        .filter(|(old, new)| set(old) && !set(new))
        .map(|(old, new)| format!("{old} -> {new}{tail}"))
        .collect()
}

/// The one notice per reload listing `lines` after the reason; its only choice is `rename-env-ok`.
pub fn env_notice(lines: &[String]) -> Ask {
    let mut detail = String::from(
        "These passwords come from environment variables, which Dbento cannot rename. \
         Rename them and restart Dbento.",
    );
    for line in lines {
        detail.push('\n');
        detail.push_str(line);
    }
    Ask {
        title: "Rename the environment variables".into(),
        detail: Some(detail.into()),
        sql: None,
        language: "text",
        choices: vec![Choice {
            id: "rename-env-ok",
            label: "OK".into(),
            danger: false,
        }],
        default: 0,
        cancel: 0,
    }
}
