//! Which table a free SELECT reads, decided by each engine's own parser.
use dbento_core::{Engine, RelationRef, single_source};

fn pg(sql: &str) -> Option<RelationRef> {
    single_source(Engine::Postgres, sql, None)
}
fn lite(sql: &str) -> Option<RelationRef> {
    single_source(Engine::Sqlite, sql, None)
}
fn my(sql: &str, db: Option<&str>) -> Option<RelationRef> {
    single_source(Engine::Mysql, sql, db)
}

#[test]
fn a_plain_select_names_its_table() {
    assert_eq!(
        pg("SELECT * FROM orders"),
        Some(RelationRef::new("public", "orders"))
    );
    assert_eq!(
        pg("select id, total from shop.orders o where total > 3 order by id limit 5;"),
        Some(RelationRef::new("shop", "orders"))
    );
    assert_eq!(
        pg("SELECT * FROM Orders"),
        Some(RelationRef::new("public", "orders"))
    );
    assert_eq!(
        pg("SELECT * FROM \"Shop\".\"Orders\""),
        Some(RelationRef::new("Shop", "Orders"))
    );
    assert_eq!(
        pg("SELECT count(*) FROM orders GROUP BY status"),
        Some(RelationRef::new("public", "orders"))
    );
    assert_eq!(
        lite("SELECT * FROM Album"),
        Some(RelationRef::new("main", "Album"))
    );
    assert_eq!(
        lite("SELECT a.Title FROM main.Album AS a WHERE a.AlbumId = 1;"),
        Some(RelationRef::new("main", "Album"))
    );
    assert_eq!(
        lite("SELECT * FROM \"My Table\""),
        Some(RelationRef::new("main", "My Table"))
    );
}

#[test]
fn anything_but_one_plain_table_names_nothing() {
    for sql in [
        "SELECT * FROM a JOIN b ON a.id = b.id",
        "SELECT * FROM a, b",
        "SELECT * FROM (SELECT * FROM a) s",
        "WITH c AS (SELECT * FROM a) SELECT * FROM c",
        "SELECT * FROM a UNION SELECT * FROM b",
        "SELECT * FROM a WHERE id IN (SELECT id FROM b)",
        "SELECT (SELECT 1) AS x FROM a",
        "SELECT 1",
        "SELECT * FROM a; SELECT * FROM b",
        "UPDATE a SET x = 1",
        "DELETE FROM a",
        "not sql at all",
        "",
    ] {
        assert_eq!(pg(sql), None, "postgres: {sql}");
        assert_eq!(lite(sql), None, "sqlite: {sql}");
        assert_eq!(my(sql, Some("d")), None, "mysql: {sql}");
    }
    assert_eq!(pg("SELECT * FROM generate_series(1, 3)"), None);
    assert_eq!(lite("SELECT * FROM generate_series(1, 3)"), None);
}

#[test]
fn postgres_and_sqlite_ignore_the_default_schema() {
    assert_eq!(
        single_source(Engine::Postgres, "SELECT * FROM t", Some("x")),
        Some(RelationRef::new("public", "t"))
    );
    assert_eq!(
        single_source(Engine::Sqlite, "SELECT * FROM t", Some("x")),
        Some(RelationRef::new("main", "t"))
    );
}

#[test]
fn a_mysql_select_names_its_table_in_the_default_or_written_database() {
    assert_eq!(
        my("SELECT * FROM users", Some("shop")),
        Some(RelationRef::new("shop", "users"))
    );
    assert_eq!(my("SELECT * FROM users", None), None);
    assert_eq!(
        my(
            "select id from sales.Orders o where id > 3 order by id limit 5;",
            None
        ),
        Some(RelationRef::new("sales", "Orders"))
    );
    assert_eq!(
        my("SELECT * FROM `My Db`.`Order Items`", Some("d")),
        Some(RelationRef::new("My Db", "Order Items"))
    );
    assert_eq!(
        my(
            "# lead\nSELECT count(*) FROM t -- tail\nGROUP BY s",
            Some("d")
        ),
        Some(RelationRef::new("d", "t"))
    );
}

#[test]
fn a_mysql_select_that_is_not_one_plain_table_names_nothing() {
    for sql in [
        "SELECT * FROM t /*!50000 JOIN u ON 1 */",
        "/*!40101 SELECT * FROM t */",
        "SELECT * FROM a.b.c",
        "SELECT * FROM JSON_TABLE('[1]', '$[*]' COLUMNS (x INT PATH '$')) j",
        "SELECT x INTO @v FROM t",
        "SELECT * FROM t WHERE EXISTS (SELECT 1)",
        "SELECT 1 FROM DUAL",
        "select now() from dual",
    ] {
        assert_eq!(my(sql, Some("d")), None, "{sql}");
    }
}
