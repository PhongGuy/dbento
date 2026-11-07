//! The read-only probe: which writes the server's ACL fences for this user, learned inside a
//! transaction that is always discarded, and the gate a read-only session applies before sending.

use std::collections::BTreeSet;

use ::redis::aio::MultiplexedConnection;
use ::redis::{Cmd, Pipeline, Value};

use crate::redis::classify::{entry_writes, redis_classification};
use crate::redis::lex::RedisStatement;
use crate::redis::table::CommandTable;
use crate::session::{Classification, ConnectError, ExecuteError, ReadOnlyEnforcement, Writes};

/// What the probe learned about this user's writes.
pub(crate) struct Probe {
    pub(crate) level: ReadOnlyEnforcement,
    /// Entry names the server refuses inside a transaction before its ACL check, so the probe
    /// cannot tell whether the user may run them.
    pub(crate) unprobeable: BTreeSet<String>,
}

/// Sends MULTI alone; only on `OK` pipelines every call of `table.probe_calls()` that classifies
/// without a refusal, then DISCARD. A DISCARD that is not OK is `ConnectError::Open`.
pub(crate) async fn probe(
    conn: &mut MultiplexedConnection,
    table: &CommandTable,
) -> Result<Probe, ConnectError> {
    let failed = |e: ::redis::RedisError| ConnectError::Open {
        message: format!("the read-only probe failed: {e}"),
    };
    // A refused MULTI would let every call run for real, so nothing else goes out without its OK.
    let multi = conn
        .send_packed_command(&::redis::cmd("MULTI"))
        .await
        .map_err(failed)?;
    if multi != Value::Okay {
        return Ok(verdict(table, &[], &[]));
    }
    let calls: Vec<_> = table
        .probe_calls()
        .into_iter()
        .filter(|args| {
            let statement = RedisStatement {
                args: args.clone(),
                range: 0..0,
            };
            redis_classification(table, &statement).is_ok()
        })
        .collect();
    // Not atomic: each queued call's refusal must come back as its own reply.
    let mut pipe = Pipeline::new();
    for args in &calls {
        let mut cmd = Cmd::new();
        for word in args {
            cmd.arg(word.as_slice());
        }
        pipe.add_command(cmd);
    }
    pipe.cmd("DISCARD");
    let mut replies = conn
        .send_packed_commands(&pipe, 0, calls.len() + 1)
        .await
        .map_err(failed)?;
    match replies.pop() {
        Some(Value::Okay) => {}
        other => {
            return Err(ConnectError::Open {
                message: format!("the read-only probe's DISCARD answered {other:?}"),
            });
        }
    }
    let mut probe = verdict(table, &calls, &replies);
    // Leaves the server never queues cannot be probed at all.
    probe.unprobeable.extend(
        table
            .entries()
            .filter(|e| e.is_leaf() && e.has_flag("no_multi"))
            .filter(|e| matches!(entry_writes(e), Ok(Writes::Yes | Writes::Unknown)))
            .map(|e| e.name.clone()),
    );
    Ok(probe)
}

/// The level from the replies, one per call; `unprobeable` holds the entry names the server
/// refused before its ACL check.
pub(crate) fn verdict(table: &CommandTable, calls: &[Vec<Vec<u8>>], replies: &[Value]) -> Probe {
    let mut fenced = 0usize;
    let mut open = false;
    let mut unprobeable = BTreeSet::new();
    for (i, call) in calls.iter().enumerate() {
        let entry = table.entry(call);
        let first = call.first().map(Vec::as_slice).unwrap_or_default();
        let name = entry.map_or_else(
            || String::from_utf8_lossy(first).to_ascii_lowercase(),
            |e| e.name.clone(),
        );
        if entry.is_some_and(|e| e.has_flag("no_multi")) {
            unprobeable.insert(name);
            continue;
        }
        // Only a command-level NOPERM fences; a key or channel NOPERM, a QUEUED or any other
        // answer leaves the call open, so the session fails closed to `BestEffort`.
        match replies.get(i) {
            Some(Value::ServerError(e)) => {
                let details = e.details().unwrap_or_default();
                if e.code() == "NOPERM" && details.contains("no permissions to run the '") {
                    fenced += 1;
                } else if e.code() == "ERR" && refused_before_acl(details) {
                    unprobeable.insert(name);
                } else {
                    open = true;
                }
            }
            _ => open = true,
        }
    }
    let level = if fenced > 0 && !open {
        ReadOnlyEnforcement::Database
    } else {
        ReadOnlyEnforcement::BestEffort
    };
    Probe { level, unprobeable }
}

/// The server's refusals that come before it checks the ACL.
fn refused_before_acl(details: &str) -> bool {
    (details.starts_with("Command '") && details.contains("not allowed inside a transaction"))
        || details.starts_with("DEBUG command not allowed")
        || details.starts_with("MODULE command not allowed")
}

/// Why a read-only session refuses the statement before sending it, if it does.
pub(crate) fn gate(
    c: &Classification,
    unprobeable: bool,
    level: Option<ReadOnlyEnforcement>,
) -> Option<ExecuteError> {
    // Under `Database` the server's ACL fences what Dbento cannot classify, unless the
    // probe could not reach that command.
    let message = match (level?, &c.writes) {
        (_, Writes::No) => return None,
        (_, Writes::Yes) => "this command writes",
        (ReadOnlyEnforcement::Database, Writes::Unknown) if !unprobeable => return None,
        (ReadOnlyEnforcement::Database, Writes::Unknown) => {
            "the server refuses this command inside a transaction, so Dbento could not check \
             that this connection's user cannot run it"
        }
        (_, Writes::Unknown) => {
            "Dbento cannot tell whether this command writes, and this connection's user can write"
        }
    };
    Some(ExecuteError::ReadOnlyViolation {
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests;
