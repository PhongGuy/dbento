mod support;

use dbento_core::{
    Browse, BrowseError, EditTarget, Engine, FormatOptions, Limits, SessionOptions, Sort,
    browse_sql, format_sql,
};
use support::{PgFixture, pg_fixture, run, sqlite_fixture, text};

const PG_SETUP: &str = r#"
CREATE TABLE t(id int PRIMARY KEY, "Sort Me" text);
INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');
CREATE TABLE par(a int);
CREATE TABLE kid() INHERITS (par);
INSERT INTO par VALUES (1);
INSERT INTO kid VALUES (2);
"#;

fn browse(filter: Option<&str>, sort: Option<(&str, bool)>) -> Browse {
    Browse::new(
        filter.map(str::to_owned),
        sort.map(|(c, d)| Sort::new(c, d)),
    )
}

async fn pg_setup() -> PgFixture {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(PG_SETUP).await.unwrap();
    fx
}

#[tokio::test]
async fn postgres_browse_sql_has_locators_only_filter_and_sort() {
    let fx = pg_setup().await;
    let t = fx.target("public", "t").await;
    let par = fx.target("public", "par").await;
    assert_eq!(
        browse_sql(&t, &Browse::default()).unwrap(),
        "SELECT *\nFROM public.t\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(&t, &browse(Some("   "), None)).unwrap(),
        "SELECT *\nFROM public.t\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(&t, &browse(None, Some(("id", false)))).unwrap(),
        "SELECT *\nFROM public.t\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(
            &t,
            &browse(Some("id > 1 -- trailing"), Some(("Sort Me", true)))
        )
        .unwrap(),
        "SELECT *\nFROM public.t\nWHERE (\nid > 1 -- trailing\n)\nORDER BY \"Sort Me\" DESC, id ASC"
    );
    assert_eq!(
        browse_sql(&par, &Browse::default()).unwrap(),
        "SELECT ctid, xmin, *\nFROM ONLY public.par"
    );
    assert_eq!(
        browse_sql(&par, &browse(None, Some(("ctid", true)))).unwrap(),
        "SELECT ctid, xmin, *\nFROM ONLY public.par\nORDER BY ctid DESC"
    );
    // xid has no ordering operator, so a browse sorted by xmin could never run.
    assert!(par.sortable("ctid") && par.sortable("a") && !par.sortable("xmin"));
    assert_eq!(
        browse_sql(&par, &browse(None, Some(("xmin", false)))),
        Err(BrowseError::Unsortable {
            column: "xmin".to_owned()
        })
    );
}

#[tokio::test]
async fn browse_sql_of_a_pk_table_is_the_formatted_select() {
    let fx = pg_setup().await;
    let t = fx.target("public", "t").await;
    assert_eq!(
        browse_sql(&t, &Browse::default()).unwrap(),
        format!(
            "{}\nORDER BY id ASC",
            format_sql(
                Engine::Postgres,
                "SELECT * FROM public.t",
                &FormatOptions::default()
            )
        )
    );
    let lite = sqlite_fixture("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);", "");
    assert_eq!(
        browse_sql(&lite.target("t").await, &Browse::default()).unwrap(),
        format!(
            "{}\nORDER BY id ASC",
            format_sql(
                Engine::Sqlite,
                "SELECT * FROM main.t",
                &FormatOptions::default()
            )
        )
    );
}

fn assert_unterminated(t: &EditTarget) {
    for filter in [
        "id > 1) /*",
        "id > 1 /* note",
        "name = 'x",
        "id > 1) --\n/*",
    ] {
        for sort in [None, Some(("id", false))] {
            assert_eq!(
                browse_sql(t, &browse(Some(filter), sort)),
                Err(BrowseError::Unterminated),
                "{filter} {sort:?}"
            );
        }
    }
    for filter in ["id > 1 /* note */", "id > 1 -- note", "name = 'a''b'"] {
        assert!(
            browse_sql(t, &browse(Some(filter), Some(("id", false)))).is_ok(),
            "{filter}"
        );
    }
}

#[tokio::test]
async fn a_filter_that_leaves_a_comment_or_quote_open_is_refused() {
    let fx = pg_setup().await;
    let pg = fx.target("public", "t").await;
    assert_unterminated(&pg);
    // Dollar quotes are Postgres strings too, tagged or not.
    for filter in ["name = $$x", "name = $q$x$$", "name = $q$x"] {
        assert_eq!(
            browse_sql(&pg, &browse(Some(filter), Some(("id", false)))),
            Err(BrowseError::Unterminated),
            "{filter}"
        );
    }
    for filter in ["name = $$x$$", "name = $q$x$$y$q$"] {
        assert!(
            browse_sql(&pg, &browse(Some(filter), Some(("id", false)))).is_ok(),
            "{filter}"
        );
    }
    let lite = sqlite_fixture("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);", "");
    assert_unterminated(&lite.target("t").await);
}

#[tokio::test]
async fn postgres_browse_runs_with_locators_first_and_parent_rows_only() {
    let fx = pg_setup().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let par = fx.target("public", "par").await;
    let ran = run(
        &mut s,
        &browse_sql(&par, &Browse::default()).unwrap(),
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    let names: Vec<&str> = ran.columns.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["ctid", "xmin", "a"]);
    assert_eq!(ran.rows.len(), 1);
    assert_eq!(ran.rows[0][2], text("1"));
    let t = fx.target("public", "t").await;
    let sql = browse_sql(
        &t,
        &browse(Some("id > 1 -- trailing"), Some(("Sort Me", true))),
    )
    .unwrap();
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let ids: Vec<_> = ran.rows.iter().map(|r| r[0].clone()).collect();
    assert_eq!(ids, [text("3"), text("2")]);
}

fn assert_filters(t: &EditTarget) {
    for filter in [
        "1=1; DELETE FROM t",
        "1=1); DELETE FROM t; SELECT (1",
        "id = 1;",
    ] {
        assert_eq!(
            browse_sql(t, &browse(Some(filter), None)),
            Err(BrowseError::MultipleStatements),
            "{filter}"
        );
    }
    assert!(browse_sql(t, &browse(Some("id <> 0 OR 'a;b' = 'a;b'"), None)).is_ok());
    assert_eq!(
        browse_sql(t, &browse(None, Some(("nope", false)))),
        Err(BrowseError::UnknownSort {
            column: "nope".to_owned()
        })
    );
}

#[tokio::test]
async fn a_filter_cannot_add_a_statement() {
    let fx = pg_setup().await;
    assert_filters(&fx.target("public", "t").await);
    let lite = sqlite_fixture("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);", "");
    assert_filters(&lite.target("t").await);
}

#[tokio::test]
async fn sqlite_browse_uses_the_rowid_alias_and_runs() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c');
         CREATE TABLE plain(a, b); INSERT INTO plain VALUES (1, 'p');",
        "",
    );
    let plain = fx.target("plain").await;
    let t = fx.target("t").await;
    assert_eq!(
        browse_sql(&plain, &Browse::default()).unwrap(),
        "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC"
    );
    let sql = browse_sql(&t, &browse(Some("id > 1"), Some(("id", true)))).unwrap();
    assert_eq!(
        sql,
        "SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY id DESC"
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = run(
        &mut s,
        &browse_sql(&plain, &Browse::default()).unwrap(),
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    let names: Vec<&str> = ran.columns.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["rowid", "a", "b"]);
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let ids: Vec<_> = ran.rows.iter().map(|r| r[0].clone()).collect();
    assert_eq!(ids, [text("3"), text("2")]);
}
