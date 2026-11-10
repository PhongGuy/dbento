//! The read-only probe and the gate, against the server's ACLs.
#![cfg(feature = "redis")]

use crate::support::{client_field, code_of, failure, forced, rd};
use dbento_core::{ExecuteError, ReadOnlyEnforcement::*, Session, SessionOptions};
use dbento_testkit::{
    RdUser::{Reader, Writer},
    SEED_KEYS,
};

async fn sent(s: &mut Session) -> u64 {
    client_field(s, "tot-cmds").await.parse().unwrap()
}

#[tokio::test]
async fn the_reader_acl_is_database_and_the_probes_write_nothing() {
    let fx = rd("").await;
    let s = fx.session(Reader, forced()).await;
    assert_eq!(s.read_only_enforcement(), Some(Database));
    let w = fx.session(Writer, forced()).await;
    assert_eq!(w.read_only_enforcement(), Some(BestEffort));
    assert_eq!(fx.db.int(&["DBSIZE"]).await, SEED_KEYS as i64);
    assert_eq!(fx.db.text(&["GET", "str"]).await.as_deref(), Some("hello"));
    assert_eq!(fx.db.int(&["EXISTS", "dbento-probe"]).await, 0);
}

#[tokio::test]
async fn the_read_only_key_probes_too() {
    let fx = rd("read_only = true").await;
    let s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(s.read_only_enforcement(), Some(Database));
}

#[tokio::test]
async fn without_multi_the_probe_sends_nothing_and_is_best_effort() {
    let fx = rd("").await;
    let (name, pw) = fx.db.create_user("nomulti", "~* &* +@all -multi").await;
    let s = fx.session_for(&name, &pw, forced()).await.unwrap();
    assert_eq!(s.read_only_enforcement(), Some(BestEffort));
    assert_eq!(fx.db.int(&["EXISTS", "dbento-probe"]).await, 0);
    assert_eq!(fx.db.int(&["DBSIZE"]).await, SEED_KEYS as i64);
}

#[tokio::test]
async fn a_key_permission_refusal_is_best_effort() {
    let fx = rd("").await;
    let rules = "%R~* &* -@all +@read +@write +hello +command +multi +discard +client|setinfo";
    let (name, pw) = fx.db.create_user("keyro", rules).await;
    let s = fx.session_for(&name, &pw, forced()).await.unwrap();
    assert_eq!(s.read_only_enforcement(), Some(BestEffort));
}

#[tokio::test]
async fn best_effort_refuses_writes_and_unknowns_before_sending() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, forced()).await;
    let start = sent(&mut s).await;
    for line in [
        "SET str x",
        "EVAL \"return 1\" 0",
        "NOSUCHCMD a",
        "DEBUG SLEEP 0",
    ] {
        let e = failure(&mut s, line).await;
        assert!(
            matches!(e, ExecuteError::ReadOnlyViolation { .. }),
            "{line}: {e:?}"
        );
    }
    // CLIENT INFO counts the commands before it, so only the first one lies in between.
    assert_eq!(sent(&mut s).await, start + 1);
    assert_eq!(fx.db.text(&["GET", "str"]).await.as_deref(), Some("hello"));
}

#[tokio::test]
async fn database_refuses_writes_and_unprobeables_and_lets_the_server_fence_the_rest() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let start = sent(&mut s).await;
    for line in ["SET str x", "SAVE", "DEBUG SLEEP 0"] {
        let e = failure(&mut s, line).await;
        assert!(
            matches!(e, ExecuteError::ReadOnlyViolation { .. }),
            "{line}: {e:?}"
        );
    }
    assert_eq!(sent(&mut s).await, start + 1);
    match failure(&mut s, "CONFIG SET maxmemory 1").await {
        ExecuteError::ReadOnlyViolation { message } => {
            assert!(message.contains("no permissions"), "{message}")
        }
        other @ (ExecuteError::Database { .. }
        | ExecuteError::Timeout
        | ExecuteError::Cancelled
        | ExecuteError::Unsupported { .. }
        | ExecuteError::TransactionControl { .. }
        | ExecuteError::SessionClosed
        | ExecuteError::StoppedAtDrainCap
        | ExecuteError::Syntax { .. }
        | ExecuteError::ResultExpired
        | ExecuteError::SessionEnded { .. }
        | _) => panic!("{other:?}"),
    }
    assert_eq!(
        code_of(failure(&mut s, "CONFIG GET databases").await),
        "NOPERM"
    );
    assert_eq!(code_of(failure(&mut s, "NOSUCHCMD a").await), "ERR");
}
