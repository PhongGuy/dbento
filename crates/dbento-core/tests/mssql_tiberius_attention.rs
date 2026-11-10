//! P2: an attention handle stops the request in flight from another thread, and the
//! acknowledgement is read through, so the next request reads its own response.
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use tiberius::{AttentionHandle, QueryItem};

type Client = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;

const CUBE: &str = "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
                    CROSS JOIN sys.all_objects c";

fn attention_after(handle: AttentionHandle, after: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        handle.request();
    })
}

/// Reads `stream` to its end: the rows seen, and whether its last Done acknowledged Attention.
async fn read_out(stream: &mut tiberius::QueryStream<'_>) -> (usize, bool) {
    let (mut rows, mut acked) = (0, false);
    while let Some(item) = stream.try_next().await.unwrap() {
        match item {
            QueryItem::Row(_) => rows += 1,
            QueryItem::Done(d) => acked = d.attention,
            QueryItem::Metadata(_) | QueryItem::Error(_) => {}
        }
    }
    (rows, acked)
}

async fn one(c: &mut Client, sql: &str) -> i32 {
    let row = c.simple_query(sql).await.unwrap().into_row().await.unwrap();
    row.unwrap().get::<i32, _>(0).unwrap()
}

#[tokio::test]
async fn attention_during_waitfor_ends_the_batch_and_later_queries_read_their_own_rows() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    for round in 0..3 {
        let started = Instant::now();
        let t = attention_after(c.attention_handle(), Duration::from_millis(300));
        let mut stream = c
            .simple_query_items("WAITFOR DELAY '00:00:10'; SELECT 'after' AS x")
            .await
            .unwrap();
        let (rows, acked) = read_out(&mut stream).await;
        drop(stream);
        t.join().unwrap();
        assert_eq!((rows, acked), (0, true), "round {round}");
        assert!(started.elapsed() < Duration::from_secs(3), "round {round}");
        assert_eq!(one(&mut c, "SELECT 1 AS one").await, 1, "round {round}");
        assert_eq!(one(&mut c, "SELECT 2 AS two").await, 2, "round {round}");
    }
}

#[tokio::test]
async fn attention_mid_stream_stops_the_rows_and_its_acknowledgement_is_read_through() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    let handle = c.attention_handle();
    let mut stream = c.simple_query_items(CUBE).await.unwrap();
    let mut rows = 0;
    while rows < 10 {
        if let Some(QueryItem::Row(_)) = stream.try_next().await.unwrap() {
            rows += 1;
        }
    }
    let asked = Instant::now();
    handle.request();
    let (more, acked) = read_out(&mut stream).await;
    let took = asked.elapsed();
    drop(stream);
    assert!(
        acked,
        "{more} rows after the request and no acknowledgement"
    );
    assert!(took < Duration::from_secs(2), "{took:?}");
    println!("attention acknowledged {took:?} after the request, {more} rows later");
    assert_eq!(one(&mut c, "SELECT 1 AS one").await, 1);
}

#[tokio::test]
async fn a_request_with_nothing_in_flight_is_forgotten_by_the_next_send() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    c.attention_handle().request();
    let mut stream = c
        .simple_query_items("WAITFOR DELAY '00:00:00.200'; SELECT 3 AS three")
        .await
        .unwrap();
    let (rows, acked) = read_out(&mut stream).await;
    drop(stream);
    assert_eq!((rows, acked), (1, false));
    assert_eq!(one(&mut c, "SELECT 4 AS four").await, 4);
}

#[tokio::test]
async fn a_stream_dropped_after_a_request_is_flushed_through_its_acknowledgement() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    let handle = c.attention_handle();
    let mut stream = c.simple_query_items(CUBE).await.unwrap();
    stream.try_next().await.unwrap();
    handle.request();
    drop(stream);
    let started = Instant::now();
    assert_eq!(one(&mut c, "SELECT 5 AS five").await, 5);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancel_query_after_a_dropped_waitfor_leaves_the_connection_in_step() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    for round in 0..3 {
        let dropped = tokio::time::timeout(
            Duration::from_millis(300),
            c.simple_query("WAITFOR DELAY '00:00:10'; SELECT 9 AS nine"),
        )
        .await
        .is_err();
        assert!(dropped, "round {round}");
        c.cancel_query().await.unwrap();
        assert_eq!(one(&mut c, "SELECT 1 AS one").await, 1, "round {round}");
        assert_eq!(one(&mut c, "SELECT 2 AS two").await, 2, "round {round}");
    }
}

#[tokio::test]
async fn drain_attention_discards_a_full_socket_of_rows_without_decoding_them() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    for asked in [false, true] {
        let handle = c.attention_handle();
        let mut stream = c.simple_query_items(CUBE).await.unwrap();
        stream.try_next().await.unwrap();
        // The server fills the socket's buffers while nothing reads.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if asked {
            handle.request();
            stream.try_next().await.unwrap();
        }
        drop(stream);
        let started = Instant::now();
        c.drain_attention().await.unwrap();
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(1000),
            "asked {asked}: {took:?}"
        );
        assert_eq!(one(&mut c, "SELECT 1 AS one").await, 1, "asked {asked}");
    }
}
