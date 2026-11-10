//! The mysql_async 0.37.1 behaviours the Dbento driver is built on, on every MySQL-family server.
use std::time::{Duration, Instant};

use dbento_testkit::{MyFlavor, NUMS_SQL, my_matrix};
use futures_util::FutureExt as _;
use mysql_async::prelude::*;
use mysql_async::{Conn, QueryResult, Row, TextProtocol};

/// The next row's first cell as a number, through a wrapper that lives for one row only.
async fn next_n(conn: &mut Conn) -> Option<i64> {
    let row: Option<Row> = QueryResult::<'_, '_, TextProtocol>::new(conn)
        .next()
        .await
        .unwrap();
    row.map(|r| r.get::<i64, usize>(0).unwrap())
}

fn names(conn: &mut Conn) -> Vec<String> {
    QueryResult::<'_, '_, TextProtocol>::new(conn)
        .columns_ref()
        .iter()
        .map(|c| c.name_str().into_owned())
        .collect()
}

#[tokio::test]
async fn the_harness_gives_a_fresh_utf8mb4_database_and_knows_its_flavor() {
    for (label, db) in my_matrix().await {
        let version = db.scalar("SELECT VERSION()").await.unwrap();
        assert_eq!(
            version.contains("MariaDB"),
            db.flavor() == MyFlavor::Mariadb,
            "{label}: {version}"
        );
        assert_eq!(
            db.scalar("SELECT DATABASE()").await.as_deref(),
            Some(db.database()),
            "{label}"
        );
        assert_eq!(
            db.scalar("SELECT @@character_set_database")
                .await
                .as_deref(),
            Some("utf8mb4"),
            "{label}"
        );
        assert!(db.database().starts_with("dbento_t_"), "{label}");
    }
}

#[tokio::test]
async fn a_result_resumes_across_short_lived_wrappers_and_a_stashed_read() {
    for (label, db) in my_matrix().await {
        let mut conn = db.admin().await;
        conn.query_drop(NUMS_SQL).await.unwrap();
        drop(
            conn.query_iter("SELECT n FROM nums ORDER BY n")
                .await
                .unwrap(),
        );
        let mut seen = Vec::new();
        for _ in 0..10 {
            seen.push(next_n(&mut conn).await.unwrap());
        }
        // A read that owns the connection may be polled once, kept, and finished later.
        let mut read = Box::pin(async move {
            let row = QueryResult::<'_, '_, TextProtocol>::new(&mut conn)
                .next()
                .await;
            (conn, row)
        });
        let (mut conn, row) = match (&mut read).now_or_never() {
            Some(done) => done,
            None => read.await,
        };
        seen.push(row.unwrap().unwrap().get::<i64, usize>(0).unwrap());
        while let Some(n) = next_n(&mut conn).await {
            seen.push(n);
        }
        assert_eq!(seen, (0..1000).collect::<Vec<i64>>(), "{label}");
        assert!(
            QueryResult::<'_, '_, TextProtocol>::new(&mut conn).is_empty(),
            "{label}"
        );
        let seven: Option<i64> = conn.query_first("SELECT 7").await.unwrap();
        assert_eq!(seven, Some(7), "{label}");
    }
}

#[tokio::test]
async fn kill_query_from_a_side_connection_ends_the_stream_and_keeps_the_connection() {
    for (label, db) in my_matrix().await {
        let mut conn = db.admin().await;
        conn.query_drop(NUMS_SQL).await.unwrap();
        let id = conn.id();
        drop(
            conn.query_iter("SELECT a.n FROM nums a, nums b, nums c")
                .await
                .unwrap(),
        );
        for _ in 0..1000 {
            next_n(&mut conn).await.unwrap();
        }
        db.admin()
            .await
            .query_drop(format!("KILL QUERY {id}"))
            .await
            .unwrap();
        let killed = Instant::now();
        let err = loop {
            match QueryResult::<'_, '_, TextProtocol>::new(&mut conn)
                .next()
                .await
            {
                Ok(Some(_)) => assert!(
                    killed.elapsed() < Duration::from_secs(5),
                    "{label}: still streaming"
                ),
                Ok(None) => panic!("{label}: the result ended without an error"),
                Err(e) => break e,
            }
        };
        match err {
            mysql_async::Error::Server(e) => assert_eq!(e.code, 1317, "{label}: {e}"),
            other @ (mysql_async::Error::Driver(_)
            | mysql_async::Error::Io(_)
            | mysql_async::Error::Other(_)
            | mysql_async::Error::Url(_)) => panic!("{label}: {other}"),
        }
        let seven: Option<i64> = conn.query_first("SELECT 7").await.unwrap();
        assert_eq!(seven, Some(7), "{label}");
    }
}

#[tokio::test]
async fn a_call_gives_one_result_set_per_select_then_reads_empty() {
    for (label, db) in my_matrix().await {
        let mut conn = db.admin().await;
        conn.query_drop("CREATE PROCEDURE two() BEGIN SELECT 1 AS a; SELECT 2 AS b, 3 AS c; END")
            .await
            .unwrap();
        drop(conn.query_iter("CALL two()").await.unwrap());
        assert_eq!(names(&mut conn), ["a"], "{label}");
        assert_eq!(next_n(&mut conn).await, Some(1), "{label}");
        assert_eq!(next_n(&mut conn).await, None, "{label}");
        assert!(
            !QueryResult::<'_, '_, TextProtocol>::new(&mut conn).is_empty(),
            "{label}"
        );
        assert_eq!(names(&mut conn), ["b", "c"], "{label}");
        assert_eq!(next_n(&mut conn).await, Some(2), "{label}");
        assert_eq!(next_n(&mut conn).await, None, "{label}");
        assert!(
            QueryResult::<'_, '_, TextProtocol>::new(&mut conn).is_empty(),
            "{label}"
        );
    }
}
