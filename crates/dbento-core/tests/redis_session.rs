//! Connecting to Valkey and running command lines.
#![cfg(feature = "redis")]
mod support;

use dbento_core::{ConnectError, ExecuteError, Flavor, SessionOptions};
use dbento_testkit::{
    BINARY_VALUE,
    RdUser::{Reader, Writer},
};
use support::{Cell, code_of, failure, rd, read, text};

#[tokio::test]
async fn a_writable_session_names_valkey_and_has_no_level() {
    let fx = rd("").await;
    let s = fx.session(Writer, SessionOptions::default()).await;
    let server = s.server().unwrap();
    assert_eq!((server.flavor, server.version.0), (Flavor::Valkey, 9));
    assert_eq!(s.read_only_enforcement(), None);
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_wrong_password_is_open_and_never_echoed() {
    let fx = rd("").await;
    let wrong = "not-the-pw-71";
    let login = fx.db.user(Reader).unwrap().to_owned();
    match fx
        .session_for(&login, wrong, SessionOptions::default())
        .await
    {
        Err(ConnectError::Open { message }) => {
            assert!(message.contains("WRONGPASS"), "{message}");
            assert!(!message.contains(wrong), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn replies_become_rows_by_shape() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let got = read(&mut s, "GET str").await;
    assert_eq!(
        (got.columns[0].0.as_str(), got.rows.clone()),
        ("value", vec![vec![text("hello")]])
    );
    let hash = read(&mut s, "HGETALL hash").await;
    let names: Vec<_> = hash.columns.iter().map(|c| c.0.as_str()).collect();
    assert_eq!((names, hash.rows.len()), (vec!["key", "value"], 2));
    let bin = read(&mut s, "GET \"bin:\\xff\\x00name\"").await;
    assert_eq!(bin.rows, vec![vec![Cell::Bytes(BINARY_VALUE.to_vec())]]);
    assert_eq!(
        read(&mut s, "GET nosuch").await.rows,
        vec![vec![Cell::Null]]
    );
}

#[tokio::test]
async fn a_write_reports_its_integer_as_rows_affected() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let del = read(&mut s, "DEL many:1 many:2 nosuch").await;
    assert_eq!(
        (del.outcome.rows_affected, del.rows),
        (Some(2), vec![vec![text("2")]])
    );
    let set = read(&mut s, "SET fresh 1").await;
    assert!(
        set.columns.is_empty() && set.rows.is_empty(),
        "{:?}",
        set.columns
    );
    assert_eq!(fx.db.text(&["GET", "fresh"]).await.as_deref(), Some("1"));
}

#[tokio::test]
async fn server_errors_carry_their_code_and_refusals_end_only_their_script() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    assert_eq!(code_of(failure(&mut s, "LPUSH str x").await), "WRONGTYPE");
    assert_eq!(code_of(failure(&mut s, "NOSUCHCMD a").await), "ERR");
    let sub = failure(&mut s, "SUBSCRIBE news").await;
    assert!(matches!(sub, ExecuteError::Unsupported { .. }), "{sub:?}");
    let bad = failure(&mut s, "GET \"open").await;
    assert!(matches!(bad, ExecuteError::Syntax { .. }), "{bad:?}");
    assert_eq!(
        read(&mut s, "GET str").await.rows,
        vec![vec![text("hello")]]
    );
}
