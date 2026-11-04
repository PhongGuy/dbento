use xtask::stress::*;

fn scalar(c: &rusqlite::Connection, sql: &str) -> String {
    c.query_row(sql, [], |r| r.get::<_, rusqlite::types::Value>(0))
        .map(|v| format!("{v:?}"))
        .unwrap()
}

#[test]
fn the_sqlite_stress_tables_have_the_spec_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.sqlite");
    std::fs::write(&path, "stale").unwrap();
    build_sqlite_stress(&path, 3000, 200).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(scalar(&c, "SELECT count(*) FROM big"), "Integer(3000)");
    assert_eq!(scalar(&c, "SELECT count(*) FROM wide"), "Integer(200)");
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM pragma_table_info('wide')"),
        "Integer(20)"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT typeof(id) || typeof(h) || typeof(ts) || length(h) FROM big LIMIT 1"
        ),
        "Text(\"integertexttext32\")"
    );
}

#[test]
fn readiness_needs_both_tables_at_the_exact_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.sqlite");
    assert!(!sqlite_stress_ready(&path, 300, 20));
    std::fs::write(&path, "not a database").unwrap();
    assert!(!sqlite_stress_ready(&path, 300, 20));
    build_sqlite_stress(&path, 300, 20).unwrap();
    assert!(sqlite_stress_ready(&path, 300, 20));
    assert!(!sqlite_stress_ready(&path, BIG_ROWS, WIDE_ROWS));
}

#[test]
fn the_postgres_statements_run_one_by_one_and_end_with_vacuum() {
    let st = pg_stress_statements(5, 2);
    assert_eq!(st.len(), 6);
    assert_eq!(st[4], "VACUUM ANALYZE big");
    assert_eq!(st[5], "VACUUM ANALYZE wide");
    assert!(st.iter().all(|s| !s.trim_end().ends_with(';')));
}

#[tokio::test]
async fn the_postgres_stress_tables_have_the_spec_shape() {
    let db = dbento_testkit::pg_db().await;
    let admin = db.admin().await;
    for s in pg_stress_statements(3000, 200) {
        admin.batch_execute(&s).await.unwrap();
    }
    let one = |sql: &'static str| {
        let admin = &admin;
        async move { admin.query_one(sql, &[]).await.unwrap().get::<_, String>(0) }
    };
    assert_eq!(one("SELECT count(*)::text FROM big").await, "3000");
    assert_eq!(one("SELECT count(*)::text FROM wide").await, "200");
    assert_eq!(
        one("SELECT count(*)::text FROM information_schema.columns WHERE table_name = 'wide'")
            .await,
        "20"
    );
    assert_eq!(
        one("SELECT pg_typeof(id)::text || ',' || pg_typeof(ts)::text FROM big LIMIT 1").await,
        "bigint,timestamp with time zone"
    );
}

#[test]
fn readiness_rejects_a_partial_build_and_a_wrong_big_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.sqlite");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE big (id); INSERT INTO big VALUES (1),(2),(3);")
        .unwrap();
    // Only `big` exists, at the right count.
    assert!(!sqlite_stress_ready(&path, 3, 2));
    c.execute_batch("CREATE TABLE wide (id); INSERT INTO wide VALUES (1),(2);")
        .unwrap();
    assert!(sqlite_stress_ready(&path, 3, 2));
    // `wide` is right but `big` is not.
    assert!(!sqlite_stress_ready(&path, 4, 2));
}
