//! Row and byte cuts on MySQL: Drain keeps the session, Close kills, Park waits for extend.
use std::time::{Duration, Instant};

use crate::support::{Cell, MyFixture, my_fixtures, run, text};
use dbento_core::{
    CellRef, ExecuteError, ExtendError, Limits, OnTruncate, ResultStream, Session, SessionOptions,
    TruncatedBy,
};
use dbento_testkit::NUMS_SQL;
use mysql_async::prelude::*;

/// 1,000,000 rows, and 1,000,000,000 rows, which no drain finishes within the cap.
const BIG: &str = "SELECT a.n * 1000 + b.n AS x FROM nums a, nums b";
const HUGE: &str = "SELECT a.n FROM nums a, nums b, nums c";

fn limits(rows: Option<u64>, bytes: Option<u64>, on: OnTruncate) -> Limits {
    let mut l = Limits::default();
    l.max_rows = rows;
    l.max_bytes = bytes;
    l.on_truncate = on;
    l
}

async fn values(stream: &mut ResultStream<'_>) -> Vec<i64> {
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        for r in 0..b.len() {
            match b.cell(r, 0) {
                CellRef::Text(t) => out.push(t.parse().unwrap()),
                other @ (CellRef::Null | CellRef::Bytes(_) | _) => {
                    panic!("unexpected cell {other:?}")
                }
            }
        }
    }
    out
}

async fn fixtures() -> Vec<(String, MyFixture)> {
    let fxs = my_fixtures("").await;
    for (_, fx) in &fxs {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
    }
    fxs
}

async fn connection_id(s: &mut Session) -> String {
    match &run(s, "SELECT CONNECTION_ID()", Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows[0][0]
    {
        Cell::Text(t) => t.clone(),
        other @ (Cell::Null | Cell::Bytes(_)) => panic!("{other:?}"),
    }
}

/// True once the server runs nothing on connection `id`, polled for up to 5 s.
async fn stops_running(fx: &MyFixture, id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    let sql = format!(
        "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id} AND INFO IS NOT NULL"
    );
    while Instant::now() < deadline {
        if fx.db.scalar(&sql).await.as_deref() == Some("0") {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn a_drain_cut_keeps_the_session_for_the_next_statement() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script(BIG);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(10), None, OnTruncate::Drain))
            .await
            .unwrap();
        assert_eq!(values(&mut stream).await.len(), 10, "{label}");
        let o = stream.outcome().cloned().unwrap();
        assert_eq!(
            (o.rows, o.truncated, o.truncated_by),
            (10, true, Some(TruncatedBy::Rows)),
            "{label}"
        );
        assert!(
            matches!(stream.extend(None).await, Err(ExtendError::NotParked)),
            "{label}"
        );
        drop(stream);
        drop(script);
        let started = Instant::now();
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]]
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{label}: the drain hit the cap"
        );
    }
}

#[tokio::test]
async fn a_close_cut_kills_the_statement_and_closes_the_session() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let id = connection_id(&mut s).await;
        let mut script = s.script(HUGE);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(10), None, OnTruncate::Close))
            .await
            .unwrap();
        assert_eq!(values(&mut stream).await.len(), 10, "{label}");
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Rows),
            "{label}"
        );
        drop(stream);
        drop(script);
        let err = run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .err()
            .unwrap();
        assert!(
            matches!(err, ExecuteError::SessionClosed),
            "{label}: {err:?}"
        );
        assert!(
            stops_running(&fx, &id).await,
            "{label}: the statement kept running"
        );
    }
}

#[tokio::test]
async fn park_then_extend_delivers_the_rest_in_order() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script("SELECT n FROM nums ORDER BY n");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(300), None, OnTruncate::Park))
            .await
            .unwrap();
        assert_eq!(
            values(&mut stream).await,
            (0..300).collect::<Vec<i64>>(),
            "{label}"
        );
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Rows),
            "{label}"
        );
        stream.extend(Some(300)).await.unwrap();
        assert_eq!(
            values(&mut stream).await,
            (300..600).collect::<Vec<i64>>(),
            "{label}"
        );
        stream.extend(None).await.unwrap();
        assert_eq!(
            values(&mut stream).await,
            (600..1000).collect::<Vec<i64>>(),
            "{label}"
        );
        let o = stream.outcome().cloned().unwrap();
        assert_eq!((o.rows, o.truncated), (1000, false), "{label}");
        assert!(
            matches!(stream.extend(None).await, Err(ExtendError::NotParked)),
            "{label}"
        );
        drop(stream);
        drop(script);
        let ran = run(
            &mut s,
            "SELECT @@SESSION.net_write_timeout",
            Limits::default(),
        )
        .await
        .remove(0)
        .unwrap();
        assert_eq!(ran.rows, vec![vec![text("3600")]], "{label}");
        let mut fresh = fx.session(SessionOptions::default()).await;
        let ran = run(
            &mut fresh,
            "SELECT @@SESSION.net_write_timeout",
            Limits::default(),
        )
        .await
        .remove(0)
        .unwrap();
        assert_ne!(
            ran.rows,
            vec![vec![text("3600")]],
            "{label}: only a parking statement raises it"
        );
    }
}

#[tokio::test]
async fn a_byte_cut_drains_even_under_park() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script(BIG);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(None, Some(64), OnTruncate::Park))
            .await
            .unwrap();
        values(&mut stream).await;
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Bytes),
            "{label}"
        );
        assert!(
            matches!(stream.extend(None).await, Err(ExtendError::NotParked)),
            "{label}"
        );
        drop(stream);
        drop(script);
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]]
        );
    }
}

#[tokio::test]
async fn close_after_a_cut_result_drains_it_under_the_cap() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script(BIG);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(10), None, OnTruncate::Drain))
            .await
            .unwrap();
        assert_eq!(values(&mut stream).await.len(), 10, "{label}");
        drop(stream);
        drop(script);
        let started = Instant::now();
        s.close().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "{label}");
    }
}

#[tokio::test]
async fn a_cut_call_drains_its_other_result_sets() {
    for (label, fx) in fixtures().await {
        fx.db
            .admin()
            .await
            .query_drop("CREATE PROCEDURE big() BEGIN SELECT n FROM nums; SELECT 2 AS b; END")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let ran = run(
            &mut s,
            "CALL big()",
            limits(Some(5), None, OnTruncate::Drain),
        )
        .await
        .remove(0)
        .unwrap();
        assert!(ran.outcome.truncated, "{label}");
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]]
        );
    }
}

#[tokio::test]
async fn a_drain_past_the_cap_kills_the_statement_and_closes_the_session() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let id = connection_id(&mut s).await;
        let mut script = s.script(HUGE);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(1), None, OnTruncate::Drain))
            .await
            .unwrap();
        assert_eq!(values(&mut stream).await.len(), 1, "{label}");
        drop(stream);
        drop(script);
        let started = Instant::now();
        let err = run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .err()
            .unwrap();
        assert!(
            matches!(err, ExecuteError::StoppedAtDrainCap),
            "{label}: {err:?}"
        );
        let took = started.elapsed();
        assert!(
            took >= Duration::from_secs(10) && took < Duration::from_secs(20),
            "{label}: {took:?}"
        );
        let again = run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .err()
            .unwrap();
        assert!(
            matches!(again, ExecuteError::SessionClosed),
            "{label}: {again:?}"
        );
        assert!(
            stops_running(&fx, &id).await,
            "{label}: the statement kept running"
        );
    }
}

#[tokio::test]
async fn a_parked_result_the_server_drops_ends_the_session() {
    for (label, fx) in fixtures().await {
        let mut s = fx.session(SessionOptions::default()).await;
        let id = connection_id(&mut s).await;
        let mut script = s.script(HUGE);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits(Some(10), None, OnTruncate::Park))
            .await
            .unwrap();
        assert_eq!(values(&mut stream).await.len(), 10, "{label}");
        fx.db
            .admin()
            .await
            .query_drop(format!("KILL {id}"))
            .await
            .unwrap();
        stream.extend(None).await.unwrap();
        let failed = loop {
            match stream.next_batch().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("{label}: the killed result ended cleanly"),
                Err(e) => break e,
            }
        };
        drop(stream);
        drop(script);
        let after = run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .err()
            .unwrap();
        assert!(
            matches!(after, ExecuteError::SessionClosed),
            "{label}: {failed:?} then {after:?}"
        );
    }
}
