use crate::support;
use dbento_app::copy::{
    CopyFormat, cell_copy, copy_as, copy_rows, copy_text, document_json, insert_maps_nothing,
    insert_unwritable, json_literal,
};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::ResultsPane;
use dbento_app::tab_model::TabModel;
use dbento_core::{CellRef, Engine, RelationRef};
use gpui_kit::component::table::TableSelection;
use gpui_kit::{AppContext as _, TestAppContext};

const SQL: &str = r#"SELECT 1 AS a, 'x' || char(9) || 'y' AS b, NULL AS c, x'00ff' AS d
UNION ALL SELECT 2, 'say "hi"', 'l1' || char(10) || 'l2', x''"#;

fn store(sql: &str) -> ResultStore {
    let (columns, batches) = support::batches(sql);
    let mut s = ResultStore::new(columns);
    for b in batches {
        s.push(b);
    }
    s
}
#[test]
fn a_cell_copies_its_whole_raw_value() {
    let s = store(SQL);
    assert_eq!(
        copy_text(&s, TableSelection::Cell(0, 2)).as_deref(),
        Some("x\ty")
    );
    assert_eq!(
        copy_text(&s, TableSelection::Cell(0, 3)).as_deref(),
        Some("")
    );
    assert_eq!(
        copy_text(&s, TableSelection::Cell(0, 4)).as_deref(),
        Some("\\x00ff")
    );
    assert_eq!(cell_copy(CellRef::Bytes(&[])), "\\x");
    let long = store("SELECT replace(hex(zeroblob(150)), '0', 'a') AS t");
    assert_eq!(
        copy_text(&long, TableSelection::Cell(0, 1)).map(|t| t.len()),
        Some(300)
    );
}
#[test]
fn a_row_is_tsv_with_quoting() {
    let s = store(SQL);
    assert_eq!(
        copy_text(&s, TableSelection::Row(0)).as_deref(),
        Some("1\t\"x\ty\"\t\t\\x00ff")
    );
    assert_eq!(
        copy_text(&s, TableSelection::Row(1)).as_deref(),
        Some("2\t\"say \"\"hi\"\"\"\t\"l1\nl2\"\t\\x")
    );
}
#[test]
fn the_gutter_copies_the_row_without_its_number() {
    let s = store(SQL);
    assert_eq!(
        copy_text(&s, TableSelection::Cell(1, 0)),
        copy_text(&s, TableSelection::Row(1))
    );
    assert_eq!(copy_text(&s, TableSelection::Column(0)), None);
}
#[test]
fn a_column_is_its_loaded_rows_and_nothing_copies_nothing() {
    let s = store(SQL);
    assert_eq!(
        copy_text(&s, TableSelection::Column(1)).as_deref(),
        Some("1\n2")
    );
    assert_eq!(
        copy_text(&s, TableSelection::Column(3)).as_deref(),
        Some("\n\"l1\nl2\"")
    );
    assert_eq!(copy_text(&s, TableSelection::None), None);
    assert_eq!(copy_text(&s, TableSelection::Row(5)), None);
    assert_eq!(copy_text(&s, TableSelection::Cell(0, 9)), None);
}
#[gpui_kit::test]
fn the_pane_puts_the_selection_on_the_clipboard(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("CREATE TABLE t(a, b); INSERT INTO t VALUES (7, 'x');", "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    // `open_window` hands back the root view it built (gpui-kit-0.7.1 lib.rs:149-153).
    let (_, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| m.run("SELECT a, b FROM t".into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    pane.update(cx, |p, cx| {
        p.table()
            .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
    });
    let copied = pane.update(cx, |p, cx| p.copy_selection(cx));
    assert_eq!(copied.as_deref(), Some("7\tx"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("7\tx")
    );
}

#[test]
fn a_carriage_return_quotes_the_field_and_a_far_column_copies_nothing() {
    let s = store("SELECT 'a' || char(13) || 'b' AS a");
    assert_eq!(
        copy_text(&s, TableSelection::Row(0)).as_deref(),
        Some("\"a\rb\"")
    );
    assert_eq!(copy_text(&s, TableSelection::Column(99)), None);
}

#[gpui_kit::test]
fn a_new_result_starts_with_nothing_selected(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("CREATE TABLE t(a, b); INSERT INTO t VALUES (7, 'x');", "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    let (_, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| m.run("SELECT a, b FROM t".into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    pane.update(cx, |p, cx| {
        p.table()
            .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
    });
    model.update(cx, |m, cx| m.run("SELECT b, a FROM t".into(), cx));
    support::wait_real(cx, "the rerun", |cx| {
        model.read_with(cx, |m, _| !m.running())
    });
    let sel = pane.update(cx, |p, cx| p.table().read(cx).selection());
    assert_eq!(sel, TableSelection::None);
    assert_eq!(pane.update(cx, |p, cx| p.copy_selection(cx)), None);
}

#[gpui_kit::test]
fn the_gutter_header_marks_nothing(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("CREATE TABLE t(a, b); INSERT INTO t VALUES (7, 'x');", "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    let (_, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| m.run("SELECT a, b FROM t".into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    let mirrored = |cx: &mut TestAppContext, sel| {
        pane.update(cx, |p, cx| {
            p.table().update(cx, |t, cx| t.set_selection(sel, cx))
        });
        cx.run_until_parked();
        pane.read_with(cx, |p, cx| p.table().read(cx).delegate().selection)
    };
    assert_eq!(
        mirrored(cx, TableSelection::Column(1)),
        TableSelection::Column(1)
    );
    assert_eq!(
        mirrored(cx, TableSelection::Column(0)),
        TableSelection::None
    );
}

#[test]
fn tsv_and_csv_quote_what_needs_it() {
    let s = store(SQL);
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &CopyFormat::Tsv, false),
        copy_text(&s, TableSelection::Row(0))
    );
    assert_eq!(
        copy_as(&s, TableSelection::Cell(1, 1), &CopyFormat::Tsv, true).as_deref(),
        Some("a\n2")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Row(1), &CopyFormat::Csv, true).as_deref(),
        Some("a,b,c,d\n2,\"say \"\"hi\"\"\",\"l1\nl2\",\\x")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Column(2), &CopyFormat::Csv, false).as_deref(),
        Some("x\ty\n\"say \"\"hi\"\"\"")
    );
}

#[test]
fn json_markdown_and_insert_name_their_columns() {
    let s = store(SQL);
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &CopyFormat::Json, false).as_deref(),
        Some("[\n  {\"a\": 1, \"b\": \"x\\ty\", \"c\": null, \"d\": \"\\\\x00ff\"}\n]")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Row(1), &CopyFormat::Markdown, false).as_deref(),
        Some("| a | b | c | d |\n| --- | --- | --- | --- |\n| 2 | say \"hi\" | l1<br>l2 | \\x |")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Cell(0, 3), &CopyFormat::Markdown, false).as_deref(),
        Some("| c |\n| --- |\n| NULL |")
    );
    let lite = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: labels(&s),
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &lite, false).as_deref(),
        Some("INSERT INTO main.t (a, b, c, d) VALUES (1, 'x\ty', NULL, X'00ff');")
    );
    let pg = CopyFormat::Insert {
        engine: Engine::Postgres,
        table: RelationRef::new("shop", "Order"),
        columns: labels(&s),
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(1), &pg, false).as_deref(),
        Some(
            "INSERT INTO shop.\"Order\" (a, b, c, d) VALUES (2, 'say \"hi\"', 'l1\nl2', E'\\\\x'::bytea);"
        )
    );
    assert_eq!(
        copy_as(&s, TableSelection::None, &CopyFormat::Json, false),
        None
    );
}

#[test]
fn json_literals_follow_the_column_type() {
    assert_eq!(
        json_literal(CellRef::Text("t"), "bool", Some(16), false),
        "true"
    );
    assert_eq!(
        json_literal(CellRef::Text("false"), "boolean", None, false),
        "false"
    );
    assert_eq!(
        json_literal(CellRef::Text("1.5e3"), "numeric", Some(1700), false),
        "1.5e3"
    );
    assert_eq!(
        json_literal(CellRef::Text("12"), "text", Some(25), false),
        "\"12\""
    );
    assert_eq!(
        json_literal(CellRef::Text("007"), "", None, false),
        "\"007\""
    );
    assert_eq!(json_literal(CellRef::Text("12"), "", None, false), "\"12\"");
    assert_eq!(json_literal(CellRef::Text("12"), "TEXT", None, true), "12");
    assert_eq!(json_literal(CellRef::Null, "text", Some(25), false), "null");
    assert_eq!(
        json_literal(CellRef::Text("NaN"), "float8", Some(701), false),
        "\"NaN\""
    );
    assert_eq!(
        json_literal(CellRef::Text("Infinity"), "numeric", Some(1700), false),
        "\"Infinity\""
    );
}

#[test]
fn a_quote_inside_a_value_is_doubled_in_an_insert() {
    let s = store("SELECT 'it''s' AS a");
    let lite = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: labels(&s),
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &lite, false).as_deref(),
        Some("INSERT INTO main.t (a) VALUES ('it''s');")
    );
}

#[test]
fn a_cell_the_store_cannot_supply_drops_its_row_instead_of_reading_null() {
    // A batch narrower than the store's columns leaves row 1 without its `b` cell.
    let (columns, wide) = support::batches("SELECT 1 AS a, 'x' AS b");
    let (_, narrow) = support::batches("SELECT 2 AS a");
    let mut s = ResultStore::new(columns);
    for b in wide.into_iter().chain(narrow) {
        s.push(b);
    }
    let insert = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: labels(&s),
    };
    let cases = [
        (CopyFormat::Csv, "a,b\n1,x"),
        (CopyFormat::Json, "[\n  {\"a\": 1, \"b\": \"x\"}\n]"),
        (CopyFormat::Markdown, "| a | b |\n| --- | --- |\n| 1 | x |"),
        (insert, "INSERT INTO main.t (a, b) VALUES (1, 'x');"),
    ];
    for (format, want) in cases {
        assert_eq!(
            copy_rows(&s, 0..2, &[0, 1], &format, true),
            want,
            "{format:?}"
        );
    }
}

/// Every column of `s` written under its own label, as when the labels are the table's columns.
fn labels(s: &ResultStore) -> Vec<Option<String>> {
    s.columns()
        .iter()
        .map(|c| Some(c.name().to_owned()))
        .collect()
}

fn insert(engine: Engine, schema: &str, s: &ResultStore) -> CopyFormat {
    CopyFormat::Insert {
        engine,
        table: RelationRef::new(schema, "t"),
        columns: labels(s),
    }
}

#[test]
fn a_backslash_makes_a_postgres_string_an_escape_literal() {
    let s = store(r"SELECT 'a\b''c' AS v, 'plain' AS w");
    assert_eq!(
        copy_as(
            &s,
            TableSelection::Row(0),
            &insert(Engine::Postgres, "public", &s),
            false
        )
        .as_deref(),
        Some(r"INSERT INTO public.t (v, w) VALUES (E'a\\b''c', 'plain');")
    );
    assert_eq!(
        copy_as(
            &s,
            TableSelection::Row(0),
            &insert(Engine::Sqlite, "main", &s),
            false
        )
        .as_deref(),
        Some(r"INSERT INTO main.t (v, w) VALUES ('a\b''c', 'plain');")
    );
}

/// The copied INSERT reads back the exact value under either `standard_conforming_strings`.
#[test]
fn a_postgres_insert_with_backslashes_round_trips() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (db, _dir, spec) = support::pg_spec("").await;
        let original = r"a\b\\c\'d'e\n";
        let conn = spec.workspace.connection("pg").unwrap();
        let secret = dbento_core::Secret::new(db.password());
        let mut session =
            dbento_core::connect(&spec.workspace, conn, Some(secret), Default::default())
                .await
                .unwrap();
        let sql = format!("SELECT $q${original}$q$ AS v, '\\x00ff5c'::bytea AS b");
        let mut script = session.script(&sql);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Default::default())
            .await
            .unwrap();
        let mut s = ResultStore::new(stream.columns().into());
        while let Some(b) = stream.next_batch().await.unwrap() {
            s.push(b);
        }
        drop(stream);
        drop(script);
        session.close().await.unwrap();
        let stmt = copy_rows(
            &s,
            0..1,
            &[0, 1],
            &insert(Engine::Postgres, "public", &s),
            false,
        );
        let admin = db.admin().await;
        admin
            .batch_execute("CREATE TABLE t (v text, b bytea)")
            .await
            .unwrap();
        for scs in ["on", "off"] {
            // A simple query is parsed whole before it runs, so the SET goes first on its own.
            admin
                .batch_execute(&format!(
                    "SET standard_conforming_strings = {scs}; SET escape_string_warning = off"
                ))
                .await
                .unwrap();
            let shown = admin
                .query_one("SHOW standard_conforming_strings", &[])
                .await;
            assert_eq!(shown.unwrap().get::<_, String>(0), scs);
            admin
                .batch_execute(&format!("TRUNCATE t; {stmt}"))
                .await
                .unwrap();
            let back = admin.query_one("SELECT v, b FROM t", &[]).await.unwrap();
            assert_eq!(back.get::<_, String>(0), original, "text, scs = {scs}");
            let bytes: Vec<u8> = back.get(1);
            assert_eq!(bytes, [0x00, 0xff, 0x5c], "bytea, scs = {scs}");
        }
    });
}

#[test]
fn an_untyped_sqlite_column_quotes_by_the_stored_type() {
    let s = store("SELECT '12' AS v UNION ALL SELECT 12 UNION ALL SELECT 1.5");
    assert_eq!(
        copy_rows(&s, 0..3, &[0], &insert(Engine::Sqlite, "main", &s), false),
        "INSERT INTO main.t (v) VALUES ('12');\nINSERT INTO main.t (v) VALUES (12);\n\
         INSERT INTO main.t (v) VALUES (1.5);"
    );
    assert_eq!(
        copy_rows(&s, 0..3, &[0], &CopyFormat::Json, false),
        "[\n  {\"v\": \"12\"},\n  {\"v\": 12},\n  {\"v\": 1.5}\n]"
    );
}

/// SQLite overflows `9e999` to an infinite REAL, rendered "inf"; the INSERT writes the overflow
/// back, as SQLite's own quote() does, while text spelled "inf" stays text.
#[test]
fn an_infinite_sqlite_real_copies_as_an_overflowing_literal_that_reads_back_infinite() {
    let s = store("SELECT 9e999 AS v, -9e999 AS w, 'inf' AS t");
    let stmt = copy_rows(
        &s,
        0..1,
        &[0, 1, 2],
        &insert(Engine::Sqlite, "main", &s),
        false,
    );
    assert_eq!(
        stmt,
        "INSERT INTO main.t (v, w, t) VALUES (9e999, -9e999, 'inf');"
    );
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(&format!("CREATE TABLE t (v, w, t); {stmt}"))
        .unwrap();
    let back: (f64, f64, String, String) = db
        .query_row("SELECT v, w, t, typeof(v) FROM t", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap();
    assert_eq!(
        back,
        (
            f64::INFINITY,
            f64::NEG_INFINITY,
            "inf".to_owned(),
            "real".to_owned()
        )
    );
}

#[test]
fn an_insert_writes_table_column_names_and_names_what_it_skips() {
    let s = store("SELECT 1 AS n, 2 AS total, 'x' AS b, 3 AS more");
    let f = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: vec![Some("a".into()), None, Some("b".into()), None],
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &f, false).as_deref(),
        Some(
            "-- Skipped columns not in main.t: total, more\nINSERT INTO main.t (a, b) VALUES (1, 'x');"
        )
    );
    assert_eq!(
        copy_as(&s, TableSelection::Cell(0, 3), &f, false).as_deref(),
        Some("INSERT INTO main.t (b) VALUES ('x');"),
        "only the selected columns are named"
    );
    let twice = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: vec![Some("a".into()), None, Some("a".into()), None],
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &twice, false).as_deref(),
        Some(
            "-- Skipped columns not in main.t: total, b, more\nINSERT INTO main.t (a) VALUES (1);"
        ),
        "a second column reading the same table column is skipped"
    );
    let broken = store("SELECT 1 AS a, 2 AS \"x\ny\"");
    let f2 = CopyFormat::Insert {
        engine: Engine::Sqlite,
        table: RelationRef::new("main", "t"),
        columns: vec![Some("a".into()), None],
    };
    assert_eq!(
        copy_as(&broken, TableSelection::Row(0), &f2, false).as_deref(),
        Some("-- Skipped columns not in main.t: x y\nINSERT INTO main.t (a) VALUES (1);"),
        "a line break in a label cannot end the comment"
    );
    assert!(insert_maps_nothing(&s, TableSelection::Cell(0, 2), &f));
    assert!(!insert_maps_nothing(&s, TableSelection::Row(0), &f));
    assert!(!insert_maps_nothing(
        &s,
        TableSelection::Row(0),
        &CopyFormat::Csv
    ));
}

/// SurrealDB has no bytes literal here, so an INSERT never guesses one: the copy is refused
/// naming the column, and `copy_rows` says which row it left out instead of dropping it unseen.
#[test]
fn a_value_with_no_sql_literal_is_named_never_dropped_silently() {
    let s = store("SELECT 1 AS a, x'00ff' AS d UNION ALL SELECT 2, NULL");
    let f = insert(Engine::Surrealdb, "main", &s);
    assert_eq!(
        insert_unwritable(&s, TableSelection::Row(0), &f).as_deref(),
        Some("d")
    );
    assert_eq!(
        insert_unwritable(&s, TableSelection::Column(2), &f).as_deref(),
        Some("d"),
        "a column selection covers every loaded row"
    );
    assert_eq!(insert_unwritable(&s, TableSelection::Row(1), &f), None);
    assert_eq!(insert_unwritable(&s, TableSelection::Cell(0, 1), &f), None);
    let unmapped = CopyFormat::Insert {
        engine: Engine::Surrealdb,
        table: RelationRef::new("main", "t"),
        columns: vec![Some("a".into()), None],
    };
    assert_eq!(
        insert_unwritable(&s, TableSelection::Row(0), &unmapped),
        None,
        "a column left out of the INSERT writes no value"
    );
    assert_eq!(
        insert_unwritable(&s, TableSelection::Row(0), &CopyFormat::Csv),
        None
    );
    assert_eq!(
        copy_rows(&s, 0..2, &[0, 1], &f, false),
        "-- Skipped row 1: d holds a value with no SQL literal\n\
         INSERT INTO `main`.`t` (`a`, `d`) VALUES (2, NULL);"
    );
}

const ALIAS_SETUP: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                           INSERT INTO t VALUES (1, 'b', 'x');";

/// Runs `sql` in the opened tab, selects its first row and copies it as SQL INSERT.
fn copy_first_row_as_insert(cx: &mut TestAppContext, o: &support::Opened, sql: &str) {
    o.run(cx, sql);
    o.idle(cx);
    let pane = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    pane.update(cx, |p, cx| {
        p.table()
            .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
    });
    cx.update_window(o.win, |_, w, cx| {
        pane.update(cx, |p, cx| p.copy_selection_as_insert(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|c| c.text())
}

#[gpui_kit::test]
fn a_sqlite_insert_copy_uses_real_column_names_and_skips_computed_ones(cx: &mut TestAppContext) {
    let o = support::opened(cx, ALIAS_SETUP, "", 0);
    copy_first_row_as_insert(cx, &o, "SELECT id AS n, name, id + 1 AS total FROM t");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some(
            "-- Skipped columns not in main.t: total\nINSERT INTO main.t (id, name) VALUES (1, 'b');"
        )
    );
    copy_first_row_as_insert(cx, &o, "SELECT id, name, id AS id2 FROM t");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some(
            "-- Skipped columns not in main.t: id2\nINSERT INTO main.t (id, name) VALUES (1, 'b');"
        )
    );
    copy_first_row_as_insert(cx, &o, "SELECT * FROM t");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("INSERT INTO main.t (id, name, note) VALUES (1, 'b', 'x');")
    );
}

#[gpui_kit::test]
fn an_all_computed_select_copies_nothing_and_says_why(cx: &mut TestAppContext) {
    let o = support::opened(cx, ALIAS_SETUP, "", 0);
    copy_first_row_as_insert(cx, &o, "SELECT id + 1 AS id, upper(name) AS name FROM t");
    assert_eq!(clipboard(cx), None);
    assert_eq!(
        support::label_of(cx, o.win, "notice").as_deref(),
        Some("Nothing copied as SQL INSERT: no selected column comes from main.t")
    );
}

#[gpui_kit::test]
fn a_postgres_insert_copy_resolves_aliases_through_the_catalog(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (db, _dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute("CREATE TABLE t(a int, b text); INSERT INTO t VALUES (1, 'x');")
            .await
            .unwrap()
    });
    let model = cx.new(|_| TabModel::new(spec));
    let m = model.clone();
    let (win, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    for (sql, want) in [
        (
            "SELECT a AS n, a + 1 AS b FROM t",
            "-- Skipped columns not in public.t: b\nINSERT INTO public.t (a) VALUES (1);",
        ),
        (
            "SELECT a, a AS a2 FROM t",
            "-- Skipped columns not in public.t: a2\nINSERT INTO public.t (a) VALUES (1);",
        ),
        (
            "SELECT * FROM t",
            "INSERT INTO public.t (a, b) VALUES (1, 'x');",
        ),
    ] {
        model.update(cx, |m, cx| m.run(sql.into(), cx));
        support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
        pane.update(cx, |p, cx| {
            p.table()
                .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
        });
        cx.update_window(win, |_, w, cx| {
            pane.update(cx, |p, cx| p.copy_selection_as_insert(w, cx))
        })
        .unwrap();
        support::wait_real(cx, "the copy", |cx| clipboard(cx).as_deref() == Some(want));
    }
}

/// A kept Postgres result copies and labels its error as Postgres after the tab switches to
/// SQLite, and asks the SQLite session for no catalog lookup on its behalf.
#[gpui_kit::test]
fn a_kept_postgres_result_keeps_its_engine_after_a_switch_to_sqlite(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (db, dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute(
                r"CREATE TABLE t(a int, b text, c bytea);
                  INSERT INTO t VALUES (1, 'x\y', '\x01ff');",
            )
            .await
            .unwrap()
    });
    let toml = dir.path().join("dbento.toml");
    let both = std::fs::read_to_string(&toml).unwrap()
        + "\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n";
    std::fs::write(&toml, both).unwrap();
    let ws = std::sync::Arc::new(dbento_core::Workspace::open(dir.path()).unwrap());
    let model = cx.new(|_| TabModel::new(spec));
    let m = model.clone();
    let (win, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    let run_then_switch = |cx: &mut TestAppContext, sql: &str| {
        model.update(cx, |m, cx| m.run(sql.into(), cx));
        support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
        let switched = model.update(cx, |m, cx| m.switch_connection("lite", ws.clone(), cx));
        assert!(switched.is_ok());
        cx.run_until_parked();
    };
    run_then_switch(cx, "SELECT a AS n, b, c FROM t");
    pane.update(cx, |p, cx| {
        p.table()
            .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
    });
    cx.update_window(win, |_, w, cx| {
        pane.update(cx, |p, cx| p.copy_selection_as_insert(w, cx))
    })
    .unwrap();
    // Without the Postgres session the alias stays as the result names it. Postgres sends
    // bytea as its hex text, which an escape literal carries back.
    let want = r"INSERT INTO public.t (n, b, c) VALUES (1, E'x\\y', E'\\x01ff');";
    support::wait_real(cx, "the copy", |cx| clipboard(cx).as_deref() == Some(want));
    let back = model.update(cx, |m, cx| m.switch_connection("pg", ws.clone(), cx));
    assert!(back.is_ok());
    run_then_switch(cx, "SELECT nope FROM t");
    assert_eq!(
        support::label_of(cx, win, "result-error-code").as_deref(),
        Some("SQLSTATE 42703")
    );
}

/// Runs `SELECT a AS x, b FROM t` on a fresh Postgres database, points the tab at a second
/// database on the same server through `repoint` (given the second database's connection
/// TOML), and copies the kept row as an INSERT. Both databases hold their own `t`, so the
/// first one's OIDs name nothing in the second.
fn copy_after_repointing(
    cx: &mut TestAppContext,
    repoint: impl FnOnce(
        &mut TestAppContext,
        &gpui_kit::Entity<TabModel>,
        &std::path::Path,
        &dbento_testkit::PgDb,
    ),
) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (a, dir, spec) = rt.block_on(support::pg_spec(""));
    let b = rt.block_on(dbento_testkit::pg_db_at(&a.server_url()));
    for db in [&a, &b] {
        rt.block_on(async {
            db.admin()
                .await
                .batch_execute("CREATE TABLE t(a int, b text); INSERT INTO t VALUES (1, 'x');")
                .await
                .unwrap()
        });
    }
    let model = cx.new(|_| TabModel::new(spec));
    let m = model.clone();
    let (win, pane) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| m.run("SELECT a AS x, b FROM t".into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    repoint(cx, &model, dir.path(), &b);
    cx.run_until_parked();
    pane.update(cx, |p, cx| {
        p.table()
            .update(cx, |t, cx| t.set_selection(TableSelection::Row(0), cx))
    });
    cx.update_window(win, |_, w, cx| {
        pane.update(cx, |p, cx| p.copy_selection_as_insert(w, cx))
    })
    .unwrap();
    // The new database cannot name the first one's sources, so the result's labels stand.
    let want = "INSERT INTO public.t (x, b) VALUES (1, 'x');";
    support::wait_real(cx, "the copy", |cx| clipboard(cx).as_deref() == Some(want));
}

/// A kept Postgres result copies under its own labels after the tab switches to another
/// Postgres connection, whose catalog says nothing about the first one's OIDs.
#[gpui_kit::test]
fn a_kept_postgres_result_skips_the_lookup_after_a_switch_to_another_postgres(
    cx: &mut TestAppContext,
) {
    copy_after_repointing(cx, |cx, model, dir, b| {
        let toml = dir.join("dbento.toml");
        let both = std::fs::read_to_string(&toml).unwrap() + &b.connection_toml("pg2", "");
        std::fs::write(&toml, both).unwrap();
        let ws = std::sync::Arc::new(dbento_core::Workspace::open(dir).unwrap());
        let switched = model.update(cx, |m, cx| m.switch_connection("pg2", ws, cx));
        assert!(switched.is_ok());
    });
}

/// The same holds when the connection keeps its name but its database is edited and the
/// tab reconnects.
#[gpui_kit::test]
fn a_kept_postgres_result_skips_the_lookup_after_its_connection_is_edited(cx: &mut TestAppContext) {
    copy_after_repointing(cx, |cx, model, dir, b| {
        let toml = format!("dbento = 1\n{}", b.connection_toml("pg", ""));
        std::fs::write(dir.join("dbento.toml"), toml).unwrap();
        let ws = std::sync::Arc::new(dbento_core::Workspace::open(dir).unwrap());
        let _ = model.update(cx, |m, cx| m.reconnect(ws, cx));
    });
}

#[test]
fn a_mysql_insert_quotes_with_backticks_and_writes_bytes_as_hex() {
    let s = store(SQL);
    let my = CopyFormat::Insert {
        engine: Engine::Mysql,
        table: RelationRef::new("shop", "Order"),
        columns: labels(&s),
    };
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &my, false).as_deref(),
        Some("INSERT INTO `shop`.`Order` (`a`, `b`, `c`, `d`) VALUES (1, 'x\ty', NULL, X'00ff');")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Row(1), &my, false).as_deref(),
        Some(
            "INSERT INTO `shop`.`Order` (`a`, `b`, `c`, `d`) VALUES (2, 'say \"hi\"', 'l1\nl2', X'');"
        )
    );
}

#[test]
fn a_duckdb_insert_writes_bytes_as_a_blob_and_quotes_infinity() {
    let s = store(SQL);
    assert_eq!(
        copy_as(
            &s,
            TableSelection::Row(0),
            &insert(Engine::Duckdb, "main", &s),
            false
        )
        .as_deref(),
        Some("INSERT INTO main.t (a, b, c, d) VALUES (1, 'x\ty', NULL, '\\x00\\xff'::BLOB);")
    );
    assert_eq!(
        copy_as(
            &s,
            TableSelection::Row(1),
            &insert(Engine::Duckdb, "main", &s),
            false
        )
        .as_deref(),
        Some("INSERT INTO main.t (a, b, c, d) VALUES (2, 'say \"hi\"', 'l1\nl2', ''::BLOB);")
    );
    let inf = store("SELECT 9e999 AS v, -9e999 AS w, 'inf' AS t");
    assert_eq!(
        copy_as(
            &inf,
            TableSelection::Row(0),
            &insert(Engine::Duckdb, "main", &inf),
            false
        )
        .as_deref(),
        Some("INSERT INTO main.t (v, w, t) VALUES ('inf', '-inf', 'inf');")
    );
}

#[test]
fn a_sql_server_insert_brackets_names_and_writes_bytes_as_hex() {
    let s = store(SQL);
    let ms = insert(Engine::Mssql, "dbo", &s);
    assert_eq!(
        copy_as(&s, TableSelection::Row(0), &ms, false).as_deref(),
        Some("INSERT INTO [dbo].[t] ([a], [b], [c], [d]) VALUES (1, N'x\ty', NULL, 0x00ff);")
    );
    assert_eq!(
        copy_as(&s, TableSelection::Row(1), &ms, false).as_deref(),
        Some(
            "INSERT INTO [dbo].[t] ([a], [b], [c], [d]) VALUES (2, N'say \"hi\"', N'l1\nl2', 0x);"
        )
    );
    let q = store("SELECT 'it''s' AS v");
    assert_eq!(
        copy_as(
            &q,
            TableSelection::Row(0),
            &insert(Engine::Mssql, "dbo", &q),
            false
        )
        .as_deref(),
        Some("INSERT INTO [dbo].[t] ([v]) VALUES (N'it''s');")
    );
}

#[test]
fn an_sql_result_carries_no_documents_and_copies_json_from_its_cells() {
    let s = store("SELECT 1 AS a");
    assert!(s.document(0).is_none());
    assert_eq!(
        copy_rows(&s, 0..1, &[0], &CopyFormat::Json, false),
        "[\n  {\"a\": 1}\n]"
    );
}

#[test]
fn a_document_that_will_not_decode_copies_as_a_json_string() {
    assert_eq!(document_json("{\"a\":1}"), "{\"a\":1}");
    assert_eq!(
        document_json("/* undecodable document: x */"),
        "\"/* undecodable document: x */\""
    );
}
