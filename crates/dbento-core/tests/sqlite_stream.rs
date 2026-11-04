mod support;
use std::time::Duration;

use dbento_core::{Limits, Pending, ResultStream, Script, Session, SessionOptions};
use support::{MANY, collect, run, sqlite_fixture, text};

/// Yields rows forever, so only a stopped worker lets the session be used again.
const ENDLESS: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT x FROM r";

#[tokio::test]
async fn max_rows_truncates_only_when_more_rows_exist() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1),(2),(3),(4),(5)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(2);
    let ran = run(&mut s, "SELECT a FROM t ORDER BY a", limits)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.rows, vec![vec![text("1")], vec![text("2")]]);
    assert_eq!((ran.outcome.rows, ran.outcome.truncated), (2, true));
    limits.max_rows = Some(5);
    let ran = run(&mut s, "SELECT a FROM t", limits)
        .await
        .remove(0)
        .unwrap();
    assert_eq!((ran.outcome.rows, ran.outcome.truncated), (5, false));
    let ran = run(&mut s, MANY, Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(
        (ran.rows.len(), ran.outcome.rows, ran.outcome.truncated),
        (5000, 5000, false)
    );
}

#[tokio::test]
async fn a_dropped_stream_leaves_the_session_usable_under_drain() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(&format!("{MANY}; SELECT 'after'"));
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
        drop(stream);
        let next = script.next().await.unwrap().unwrap();
        assert_eq!(
            collect(next, Limits::default()).await.unwrap().rows,
            vec![vec![text("after")]]
        );
    }
    assert_eq!(
        run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        vec![vec![text("1")]]
    );
}

#[tokio::test]
async fn a_dropped_stream_over_an_endless_query_frees_the_worker() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = tokio::time::timeout(Duration::from_secs(10), async {
        {
            let mut script = s.script(ENDLESS);
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some());
        }
        run(&mut s, "SELECT 1", Limits::default()).await.remove(0)
    })
    .await
    .expect("the worker kept stepping a dropped stream");
    assert_eq!(ran.unwrap().rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn session_types_and_futures_are_send() {
    fn is_send<T: Send>(_: &T) {}
    fn send_type<T: Send>() {}
    send_type::<Session>();
    send_type::<Script<'static>>();
    send_type::<Pending<'static>>();
    send_type::<ResultStream<'static>>();
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT 1");
    let next = script.next();
    is_send(&next);
    let pending = next.await.unwrap().unwrap();
    let exec = pending.execute(Limits::default());
    is_send(&exec);
    let mut stream = exec.await.unwrap();
    let batch = stream.next_batch();
    is_send(&batch);
    assert!(batch.await.unwrap().is_some());
}
