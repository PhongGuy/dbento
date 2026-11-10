//! Storing the secrets of imported connections in the keychain after `apply`.

use super::plan::Imported;
use crate::{Secret, SecretKey, SecretSource, SecretStore, Workspace, secret_env_var};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// Saved passwords read from a source tool, keyed by the source connection id.
#[non_exhaustive]
#[derive(Clone, Default)]
pub struct ImportSecrets {
    by_id: BTreeMap<String, Secret>,
}

impl ImportSecrets {
    pub(crate) fn insert(&mut self, source_id: String, secret: Secret) {
        self.by_id.insert(source_id, secret);
    }

    /// The secret saved for the source connection `source_id`, if any.
    pub fn get(&self, source_id: &str) -> Option<&Secret> {
        self.by_id.get(source_id)
    }

    /// The source ids that hold a secret, in id order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.by_id.keys().map(String::as_str)
    }

    /// How many secrets are held.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// True when no secret is held.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl fmt::Debug for ImportSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportSecrets")
            .field("ids", &self.ids().collect::<Vec<_>>())
            .finish()
    }
}

/// What happened to one imported connection's secret.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOutcome {
    /// The saved password was written to the keychain.
    Stored,
    /// A keychain entry or a `DBENTO_SECRET_<NAME>` variable already supplies it.
    Present,
    /// Reading or writing the keychain failed; holds the error text.
    Failed(String),
    /// The source saved no password, so the user must supply one.
    Missing,
}

/// The secret outcome of every imported keychain Postgres connection, in import order.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredSecrets {
    outcomes: Vec<(String, SecretOutcome)>,
}

impl StoredSecrets {
    /// Each connection's final name with its outcome.
    pub fn outcomes(&self) -> &[(String, SecretOutcome)] {
        &self.outcomes
    }

    /// Names whose secret was stored.
    pub fn stored(&self) -> Vec<&str> {
        self.names(|o| *o == SecretOutcome::Stored)
    }

    /// Names whose secret was already supplied.
    pub fn present(&self) -> Vec<&str> {
        self.names(|o| *o == SecretOutcome::Present)
    }

    /// Names whose keychain call failed, with the error text.
    pub fn failed(&self) -> Vec<(&str, &str)> {
        self.outcomes
            .iter()
            .filter_map(|(n, o)| match o {
                SecretOutcome::Failed(e) => Some((n.as_str(), e.as_str())),
                SecretOutcome::Stored | SecretOutcome::Present | SecretOutcome::Missing => None,
            })
            .collect()
    }

    /// Names that still need a secret from the user: failed and missing, in order.
    pub fn needs_secret(&self) -> Vec<&str> {
        self.names(|o| matches!(o, SecretOutcome::Failed(_) | SecretOutcome::Missing))
    }

    fn names(&self, keep: impl Fn(&SecretOutcome) -> bool) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|(_, o)| keep(o))
            .map(|(n, _)| n.as_str())
            .collect()
    }
}

/// Stores each imported keychain Postgres connection's secret after `apply`; never errors.
///
/// An added connection gets its saved secret; an unchanged one only fills an empty entry, so a
/// secret the user set (keychain or `env`) is never replaced. Failures are reported per name.
pub fn store_secrets(
    ws_dir: &Path,
    imported: &[Imported],
    secrets: &ImportSecrets,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> StoredSecrets {
    let ws = match Workspace::open(ws_dir) {
        Ok(ws) => ws,
        Err(e) => {
            // Without the workspace no key can be built, so every secret on hand is lost.
            let text = e.to_string();
            let outcomes = imported
                .iter()
                .filter(|i| secrets.get(i.source_id()).is_some())
                .map(|i| (i.name().to_owned(), SecretOutcome::Failed(text.clone())))
                .collect();
            return StoredSecrets { outcomes };
        }
    };

    let mut outcomes = Vec::new();
    for i in imported {
        let Some(conn) = ws.connection(i.name()) else {
            continue;
        };
        if !conn.engine().capabilities().password || *conn.secret_source() != SecretSource::Keychain
        {
            continue;
        }
        let key = SecretKey::for_connection(&ws, conn);
        let secret = secrets.get(i.source_id());
        let set = |s: &Secret| match store.set(&key, s) {
            Ok(()) => SecretOutcome::Stored,
            Err(e) => SecretOutcome::Failed(e.to_string()),
        };
        let outcome = if i.added() {
            secret.map_or(SecretOutcome::Missing, set)
        } else if env(&secret_env_var(i.name())).is_some() {
            // resolve_secret reads the keychain first, so storing here would shadow the variable.
            SecretOutcome::Present
        } else {
            match store.get(&key) {
                Ok(Some(_)) => SecretOutcome::Present,
                Ok(None) => secret.map_or(SecretOutcome::Missing, set),
                Err(e) => match secret {
                    Some(_) => SecretOutcome::Failed(e.to_string()),
                    None => SecretOutcome::Missing,
                },
            }
        };
        outcomes.push((i.name().to_owned(), outcome));
    }
    StoredSecrets { outcomes }
}

#[cfg(test)]
mod tests;
