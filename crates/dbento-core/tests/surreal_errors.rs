//! The error map on a real server: parse positions, a value with no CBOR form, a reply the
//! decoder cannot read, and the ways a session ends.
mod support;

use std::time::Duration;

use dbento_core::{ExecuteError, Limits, Session, SessionOptions};
use dbento_testkit::surreal_db;
use support::{collect, run, sr_proxied, sr_root, sr_session, text};

#[tokio::test]
async fn a_parse_error_points_at_its_byte_in_the_unit() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let unit = "RETURN 'é';\n\nSELEC 1;\n";
    let ran = run(&mut s, unit, Limits::default()).await;
    let bad = ran.iter().find_map(|r| r.as_ref().err()).unwrap();
    let ExecuteError::Database {
        position,
        code,
        detail,
        ..
    } = bad
    else {
        panic!("{bad:?}");
    };
    assert_eq!(code, "-32000");
    assert_eq!(detail.as_deref(), Some("Validation"));
    assert_eq!(
        *position,
        Some("SELEC ".len()),
        "offset into the unit's own text"
    );
}

#[tokio::test]
async fn a_regex_value_reports_the_cast_and_that_the_statement_may_have_run() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "RETURN 1; RETURN <regex>'a'; RETURN 2",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 2);
    let Err(ExecuteError::Database {
        code,
        message,
        hint,
        ..
    }) = &ran[1]
    else {
        panic!("{:?}", ran[1].as_ref().err());
    };
    assert_eq!(code, "-32700");
    assert!(message.contains("may have run"), "{message}");
    assert!(hint.as_deref().unwrap().contains("<string>"));
}

#[tokio::test]
async fn rows_before_a_late_error_reach_the_caller_first() {
    let db = surreal_db().await;
    db.admin("FOR $i IN 1..=40 { CREATE type::record('rx', $i) SET r = IF $i = 20 { <regex>'a' } ELSE { 'plain' } }")
        .await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut script = s.script("SELECT * FROM rx ORDER BY id");
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(Limits::default()).await.unwrap();
    let mut rows = 0;
    let failed = loop {
        match stream.next_batch().await {
            Ok(Some(batch)) => rows += batch.len(),
            Ok(None) => break None,
            Err(e) => break Some(e),
        }
    };
    assert!(rows > 0, "the first frames came through");
    assert!(
        matches!(failed, Some(ExecuteError::Database { .. })),
        "{failed:?}"
    );
}

const DEEPEST: usize = 256;

// Literals stop at the parser's depth limit, so the nesting is built at run time.
fn nested(depth: usize) -> String {
    format!("RETURN array::fold(array::range(1, {depth}), 1, |$a, $i| [$a])")
}

// The server nests deeper than the decoder reads. That is one statement's failure: the socket is
// fine, and the frame's envelope still says whose reply it was.
#[tokio::test]
async fn a_reply_nested_past_the_decoder_is_a_statement_error_and_the_session_lives() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        &format!("{}; RETURN 2", nested(DEEPEST)),
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 1, "it ends the script");
    let Err(ExecuteError::Database { message, hint, .. }) = &ran[0] else {
        panic!("{:?}", ran[0].as_ref().err());
    };
    assert!(message.contains("nesting"), "{message}");
    assert!(hint.is_some());
    let next = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(next[0].as_ref().unwrap().rows, [vec![text("alive")]]);
    let shallow = run(&mut s, &nested(100), Limits::default()).await;
    assert!(shallow[0].is_ok());
}

async fn ended(s: &mut Session, text_of: &str) -> ExecuteError {
    let ran = run(s, text_of, Limits::default()).await;
    ran.into_iter()
        .find_map(Result::err)
        .expect("the statement should fail")
}

#[tokio::test]
async fn a_reply_over_the_frame_limit_ends_the_session_and_names_the_limit() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let e = ended(
        &mut s,
        "RETURN {a: array::repeat(string::repeat('x', 1000000), 270)}",
    )
    .await;
    assert!(e.closes_session(), "{e:?}");
    assert!(e.to_string().contains("256 MiB"), "{e}");
    let next = ended(&mut s, "RETURN 1").await;
    assert!(matches!(next, ExecuteError::SessionClosed), "{next:?}");
}

#[tokio::test]
async fn an_expired_login_session_ends_the_dbento_session_and_says_so() {
    let db = surreal_db().await;
    db.admin(&format!(
        "DEFINE USER brief ON DATABASE PASSWORD '{}' ROLES EDITOR DURATION FOR SESSION 2s",
        db.password()
    ))
    .await;
    let mut s = sr_session(
        &db,
        Some(("brief", "database")),
        "",
        SessionOptions::default(),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(3_200)).await;
    let e = ended(&mut s, "RETURN 1").await;
    assert!(e.closes_session(), "{e:?}");
    assert!(e.to_string().contains("expired"), "{e}");
}

#[tokio::test]
async fn a_lost_socket_ends_a_running_statement_with_the_reason() {
    let db = surreal_db().await;
    let (mut s, pipe) = sr_proxied(&db, "", SessionOptions::default()).await;
    let mut script = s.script("SLEEP 30s");
    let pending = script.next().await.unwrap().unwrap();
    let cut = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut pipe = pipe;
        pipe.cut();
        pipe
    };
    let (failed, _pipe) = tokio::join!(collect(pending, Limits::default()), cut);
    let failed = failed.err().unwrap();
    assert!(failed.closes_session(), "{failed:?}");
    assert!(failed.to_string().contains("lost"), "{failed}");
    drop(script);
    let next = ended(&mut s, "RETURN 1").await;
    assert!(next.closes_session(), "{next:?}");
}

fn call_bound() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.bounds.call = Some(Duration::from_secs(1));
    opts
}

// A server that stops answering: the socket stays up and nothing comes back.
#[tokio::test]
async fn a_call_past_its_bound_ends_the_session_and_names_the_call() {
    let db = surreal_db().await;
    let (mut s, pipe) = sr_proxied(&db, "read_only = true", call_bound()).await;
    pipe.freeze();
    let e = ended(&mut s, "RETURN 1").await;
    assert!(
        matches!(&e, ExecuteError::SessionEnded { reason } if reason.contains("begin")),
        "{e:?}"
    );
    let next = ended(&mut s, "RETURN 1").await;
    assert!(matches!(next, ExecuteError::SessionClosed), "{next:?}");
}

#[tokio::test]
async fn a_stream_with_no_begin_frame_is_bounded_like_a_call() {
    let db = surreal_db().await;
    let (mut s, pipe) = sr_proxied(&db, "", call_bound()).await;
    pipe.freeze();
    let e = ended(&mut s, "RETURN 1").await;
    assert!(
        matches!(&e, ExecuteError::SessionEnded { reason } if reason.contains("query_stream")),
        "{e:?}"
    );
}

// The fence's `cancel` cannot answer on a frozen socket, and closing the socket is what rolls it
// back, so the write is gone though no `cancel` was ever read.
#[tokio::test]
async fn a_fence_whose_cancel_is_not_answered_is_rolled_back_by_the_close() {
    let db = surreal_db().await;
    db.admin("DEFINE FUNCTION fn::w() { CREATE fnt:1 SET x = 1; RETURN 1; }")
        .await;
    let (mut s, pipe) = sr_proxied(&db, "read_only = true", call_bound()).await;
    let mut script = s.script("RETURN fn::w(); RETURN 2");
    let pending = script.next().await.unwrap().unwrap();
    let ran = collect(pending, Limits::default()).await.unwrap();
    assert!(ran.outcome.rolled_back);
    drop(script);
    pipe.freeze();
    let e = ended(&mut s, "RETURN fn::w()").await;
    assert!(e.closes_session(), "{e:?}");
    drop(pipe);
    assert_eq!(db.count("fnt").await, 0);
}

// The lexer reads `RETURN ->? / 2; THROW 'x'; RETURN 7 / 2` as one statement and the server as
// three. The one result succeeds; the error of a statement it never saw is owed to the settle.
#[tokio::test]
async fn an_error_in_a_statement_the_lexer_did_not_see_is_owed_to_the_settle() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "RETURN ->? / 2; THROW 'unseen'; RETURN 7 / 2",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 1);
    assert!(ran[0].is_ok(), "{:?}", ran[0].as_ref().err());
    let settled = s.settle().await;
    assert!(
        matches!(&settled, Err(ExecuteError::Database { message, .. }) if message.contains("unseen")),
        "{settled:?}"
    );
    assert!(s.settle().await.is_ok(), "owed once");
}
