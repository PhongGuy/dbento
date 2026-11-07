//! Write classification of Redis commands from the server's own command table.

use crate::redis::lex::RedisStatement;
use crate::redis::table::{CommandTable, Entry};
use crate::session::{Classification, Unguarded, Writes};

/// Commands that switch the connection into a reply mode a request-reply session cannot follow.
const REPLY_MODE: &[&str] = &[
    "subscribe",
    "psubscribe",
    "ssubscribe",
    "unsubscribe",
    "punsubscribe",
    "sunsubscribe",
    "monitor",
    "sync",
    "psync",
    "client|reply",
    "client|tracking",
    "reset",
];

/// Commands that carry credentials in their arguments.
const CREDENTIALS: &[&str] = &["auth", "hello"];

/// Session control and server reads that change no key, whatever their flags say.
const NO_NAMES: &[&str] = &[
    "select",
    "multi",
    "exec",
    "discard",
    "watch",
    "unwatch",
    "ping",
    "echo",
    "time",
    "client|id",
    "client|getname",
    "client|info",
    "client|list",
    "client|setname",
    "client|setinfo",
    "info",
    "lastsave",
    "role",
    "config|get",
    "slowlog|get",
    "slowlog|len",
    "latency|latest",
    "latency|history",
    "memory|stats",
    "memory|doctor",
    "pubsub|channels",
    "pubsub|numsub",
    "pubsub|numpat",
    "script|exists",
    "function|list",
    "function|stats",
    "acl|whoami",
    "acl|cat",
    "cluster|info",
    "cluster|nodes",
    "cluster|slots",
    "cluster|shards",
];

/// Commands that change no key yet reach other clients.
const YES_NAMES: &[&str] = &["publish", "spublish"];

const TRANSACTION_CONTROL: &[&str] = &["multi", "exec", "discard"];
const FLUSHES: &[&str] = &["flushdb", "flushall"];

/// The word that pads a probe call to the command's arity.
const PROBE_WORD: &[u8] = b"dbento-probe";

/// Why a command is refused in every session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) reason: String,
}

/// Rules 1 to 4 for one entry: refused, then `No`, then `Yes`, then `Unknown`.
pub(crate) fn entry_writes(entry: &Entry) -> Result<Writes, Refusal> {
    if let Some(refused) = refusal(&entry.name) {
        return Err(refused);
    }
    Ok(writes_of(&entry.name, Some(entry)))
}

/// The statement's classification, or the refusal that applies to it in every session.
pub(crate) fn redis_classification(
    table: &CommandTable,
    stmt: &RedisStatement,
) -> Result<Classification, Refusal> {
    // Refuse from the words before any lookup, so a table that lacks the command
    // (or is empty) cannot let a reply-mode or credential command through.
    let word = |i: usize| {
        stmt.args
            .get(i)
            .map(|w| String::from_utf8_lossy(w).to_ascii_lowercase())
    };
    let first = word(0).unwrap_or_default();
    if let Some(refused) = refusal(&first) {
        return Err(refused);
    }
    if let Some(second) = word(1)
        && let Some(refused) = refusal(&format!("{first}|{second}"))
    {
        return Err(refused);
    }

    let entry = table.entry(&stmt.args);
    let (name, writes) = match entry {
        Some(e) => (e.name.as_str(), entry_writes(e)?),
        None => (first.as_str(), writes_of(&first, None)),
    };
    let unguarded = if FLUSHES.contains(&name) {
        vec![Unguarded::FlushKeys]
    } else {
        Vec::new()
    };
    Ok(Classification {
        writes,
        unguarded,
        transaction_control: TRANSACTION_CONTROL.contains(&name),
        categories: entry.map(|e| e.categories.clone()).unwrap_or_default(),
    })
}

impl CommandTable {
    /// One arity-valid call per leaf that may write, in table order, skipping `no_multi`
    /// leaves; each is the name's words padded with a placeholder to the arity.
    pub(crate) fn probe_calls(&self) -> Vec<Vec<Vec<u8>>> {
        self.entries()
            .filter(|e| e.is_leaf() && !e.has_flag("no_multi"))
            .filter(|e| matches!(entry_writes(e), Ok(Writes::Yes | Writes::Unknown)))
            .map(|e| {
                let mut call: Vec<Vec<u8>> =
                    e.name.split('|').map(|w| w.as_bytes().to_vec()).collect();
                let arity = usize::try_from(e.arity.unsigned_abs()).unwrap_or(usize::MAX);
                while call.len() < arity {
                    call.push(PROBE_WORD.to_vec());
                }
                call
            })
            .collect()
    }
}

/// Rules 2 to 4 by name, using the entry's flags and key specs when the table holds one.
fn writes_of(name: &str, entry: Option<&Entry>) -> Writes {
    let flagged = |flag: &str| entry.is_some_and(|e| e.has_flag(flag));
    if flagged("readonly")
        || NO_NAMES.contains(&name)
        || name == "command"
        || name.starts_with("command|")
        || name.ends_with("|help")
    {
        return Writes::No;
    }
    let key_spec_writes = entry.is_some_and(|e| e.key_spec_writes);
    if flagged("write") || key_spec_writes || YES_NAMES.contains(&name) {
        return Writes::Yes;
    }
    Writes::Unknown
}

/// Rule 1: the refusal for a lowercase command name, naming it in capitals and never
/// echoing an argument.
fn refusal(name: &str) -> Option<Refusal> {
    let shown = name.replace('|', " ").to_ascii_uppercase();
    let reason = if REPLY_MODE.contains(&name) {
        format!("{shown} changes how the connection replies, which Dbento does not support")
    } else if CREDENTIALS.contains(&name) {
        format!(
            "{shown}: credentials never go in query text; set `user` and the secret on the connection"
        )
    } else if name == "quit" {
        "QUIT closes the connection; close the session instead".to_owned()
    } else {
        return None;
    };
    Some(Refusal { reason })
}

#[cfg(test)]
mod tests;
