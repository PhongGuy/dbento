//! P3: the vendored tiberius yields DONE and ERROR tokens in the order the server sends them.
use futures_util::TryStreamExt;
use tiberius::{DoneKind, QueryItem};

/// One word per item: `M:<first column>`, `R`, a Done as `D`/`I`/`P` (kind) plus the row count
/// and `!` when its error bit is set, `E<number>` for an error token.
async fn items(db: &dbento_testkit::MsDb, sql: &str) -> (Vec<String>, Option<bool>) {
    let mut c = db.client().await;
    let mut stream = c.simple_query_items(sql).await.unwrap();
    let (mut out, mut last_more) = (Vec::new(), None);
    while let Some(item) = stream.try_next().await.unwrap() {
        out.push(match item {
            QueryItem::Metadata(m) => format!("M:{}", m.columns()[0].name()),
            QueryItem::Row(_) => "R".to_owned(),
            QueryItem::Done(d) => {
                last_more = Some(d.more);
                let kind = match d.kind {
                    DoneKind::Done => "D",
                    DoneKind::DoneInProc => "I",
                    DoneKind::DoneProc => "P",
                };
                let rows = d.rows.map(|n| n.to_string()).unwrap_or_default();
                format!("{kind}{rows}{}", if d.error { "!" } else { "" })
            }
            QueryItem::Error(e) => format!("E{}", e.code()),
        });
    }
    (out, last_more)
}

#[tokio::test]
async fn result_sets_row_counts_and_errors_arrive_in_wire_order() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    c.simple_query("CREATE TABLE dbo.t (id int PRIMARY KEY, v nvarchar(10)); INSERT dbo.t VALUES (1, N'x'), (2, N'y')")
        .await.unwrap().into_results().await.unwrap();
    drop(c);
    let cases: [(&str, &[&str]); 6] = [
        (
            "SELECT 1 AS a; SELECT 2 AS b",
            &["M:a", "R", "D1", "M:b", "R", "D1"],
        ),
        (
            "UPDATE dbo.t SET v = v WHERE id = 1; SELECT COUNT(*) AS n FROM dbo.t",
            &["D1", "M:n", "R", "D1"],
        ),
        (
            "SELECT 1 AS a; SELECT 1/0 AS boom; SELECT 2 AS b",
            &["M:a", "R", "D1", "M:boom", "E8134", "D!", "M:b", "R", "D1"],
        ),
        ("SELECT * FROM dbo.nope", &["E208", "D!"]),
        (
            "DECLARE @x int = 1; SET @x = 2; PRINT 'p'; SELECT @x AS x",
            &["D1", "D1", "D", "M:x", "R", "D1"],
        ),
        ("SELECT 1 AS a WHERE 1 = 0", &["M:a", "D0"]),
    ];
    for (sql, want) in cases {
        let (got, last_more) = items(&db, sql).await;
        assert_eq!(got, want, "{sql}");
        assert_eq!(
            last_more,
            Some(false),
            "{sql}: the final DONE has no MORE bit"
        );
    }
}

#[tokio::test]
async fn a_procedure_closes_its_statements_with_doneinproc() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let (got, _) = items(&db, "EXEC sp_executesql N'SELECT 1 AS a'").await;
    assert_eq!(got, ["M:a", "R", "I1", "P"]);
}

#[tokio::test]
async fn a_variable_assignment_counts_one_row_under_the_select_command() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    c.simple_query("CREATE TABLE dbo.u (id int)")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let mut stream = c
        .simple_query_items(
            "DECLARE @x int = 1; SELECT @x = 2; INSERT dbo.u VALUES (1); UPDATE dbo.u SET id = id",
        )
        .await
        .unwrap();
    let mut done = Vec::new();
    while let Some(item) = stream.try_next().await.unwrap() {
        if let QueryItem::Done(d) = item {
            done.push((d.cur_cmd, d.rows));
        }
    }
    assert_eq!(
        done,
        [
            (193, Some(1)),
            (193, Some(1)),
            (195, Some(1)),
            (197, Some(1))
        ]
    );
}

#[tokio::test]
async fn the_other_query_paths_still_hold_the_error_back() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    let err = c
        .simple_query("SELECT 1 AS a; SELECT 1/0 AS boom")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap_err();
    assert!(
        matches!(err, tiberius::error::Error::Server(ref t) if t.code() == 8134),
        "{err:?}"
    );
    let rows = c
        .simple_query("SELECT 7 AS s")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(7));
}

#[tokio::test]
async fn the_collecting_helpers_of_an_items_stream_return_the_server_error() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    let sql = "SELECT 1 AS a; SELECT 1/0 AS boom";
    let err = c
        .simple_query_items(sql)
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap_err();
    assert!(
        matches!(err, tiberius::error::Error::Server(ref t) if t.code() == 8134),
        "{err:?}"
    );
    let err = c
        .simple_query_items(sql)
        .await
        .unwrap()
        .into_row_stream()
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    assert!(
        matches!(err, tiberius::error::Error::Server(ref t) if t.code() == 8134),
        "{err:?}"
    );
}
