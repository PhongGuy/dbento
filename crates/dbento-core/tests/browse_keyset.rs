//! Browse order ends with the row key; a keyset continues an unsorted browse after its
//! last key; a batch drops its first rows.

use crate::support::{Cell, pg_fixture, row_key, run, sqlite_fixture, text, val};
use dbento_core::{
    Browse, BrowseError, EditValue, Keyset, Limits, SessionOptions, Sort, browse_sql,
};

const LITE: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, g INTEGER, v TEXT); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2500) \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM r; \
    CREATE TABLE pair(a TEXT, b INTEGER, n TEXT, PRIMARY KEY (a, b)); \
    INSERT INTO pair VALUES ('x', 1, 'p'), ('x', 2, 'q'), ('y', 1, 'r'); \
    CREATE TABLE plain(a, b); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 20) \
    INSERT INTO plain SELECT x, 'p' FROM r;";

const PG: &str = "CREATE TABLE t(id int PRIMARY KEY, g int, v text); \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM generate_series(1, 2500) x; \
    CREATE TABLE pair(a text, b int, n text, PRIMARY KEY (a, b)); \
    INSERT INTO pair VALUES ('x', 1, 'p'), ('x', 2, 'q'), ('y', 1, 'r'); \
    CREATE TABLE par(a int); CREATE TABLE kid() INHERITS (par); \
    INSERT INTO par VALUES (1), (2);";

fn sorted(column: &str, descending: bool) -> Browse {
    Browse::new(None, Some(Sort::new(column, descending)))
}
fn after(p: &[(&str, EditValue)]) -> Keyset {
    Keyset::new(row_key(p))
}
fn firsts(rows: &[Vec<Cell>]) -> Vec<Cell> {
    rows.iter().map(|r| r[0].clone()).collect()
}

#[tokio::test]
async fn sqlite_browses_end_their_order_with_the_key() {
    let fx = sqlite_fixture(LITE, "");
    let (t, pair, plain) = (
        fx.target("t").await,
        fx.target("pair").await,
        fx.target("plain").await,
    );
    assert!(t.stable_key() && pair.stable_key() && plain.stable_key());
    assert_eq!(
        browse_sql(&t, &Browse::default()).unwrap(),
        "SELECT *\nFROM main.t\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(&t, &sorted("g", true)).unwrap(),
        "SELECT *\nFROM main.t\nORDER BY g DESC, id ASC"
    );
    assert_eq!(
        browse_sql(&t, &sorted("id", true)).unwrap(),
        "SELECT *\nFROM main.t\nORDER BY id DESC"
    );
    assert_eq!(
        browse_sql(&pair, &Browse::default()).unwrap(),
        "SELECT *\nFROM main.pair\nORDER BY a ASC, b ASC"
    );
    assert_eq!(
        browse_sql(&pair, &sorted("b", true)).unwrap(),
        "SELECT *\nFROM main.pair\nORDER BY b DESC, a ASC"
    );
    assert_eq!(
        browse_sql(&plain, &Browse::default()).unwrap(),
        "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC"
    );
}

#[tokio::test]
async fn a_keyset_continues_an_unsorted_browse_after_its_last_key() {
    let fx = sqlite_fixture(LITE, "");
    let (t, pair, plain) = (
        fx.target("t").await,
        fx.target("pair").await,
        fx.target("plain").await,
    );
    let k = after(&[("id", val("1000"))]);
    assert_eq!(
        browse_sql(&t, &Browse::default().with_after(k.clone())).unwrap(),
        "SELECT *\nFROM main.t\nWHERE (id) > ('1000')\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(
            &t,
            &Browse::new(Some("g = 3".into()), None).with_after(k.clone())
        )
        .unwrap(),
        "SELECT *\nFROM main.t\nWHERE (\ng = 3\n)\nAND (id) > ('1000')\nORDER BY id ASC"
    );
    assert_eq!(
        browse_sql(
            &pair,
            &Browse::default().with_after(after(&[("a", val("x")), ("b", val("1"))]))
        )
        .unwrap(),
        "SELECT *\nFROM main.pair\nWHERE (a, b) > ('x', '1')\nORDER BY a ASC, b ASC"
    );
    assert_eq!(
        browse_sql(
            &plain,
            &Browse::default().with_after(after(&[("rowid", val("9"))]))
        )
        .unwrap(),
        "SELECT rowid, *\nFROM main.plain\nWHERE (rowid) > ('9')\nORDER BY rowid ASC"
    );
    for refused in [
        sorted("g", false).with_after(k.clone()),
        Browse::default().with_after(after(&[("g", val("1"))])),
        Browse::default().with_after(after(&[("id", EditValue::Null)])),
    ] {
        assert_eq!(
            browse_sql(&t, &refused),
            Err(BrowseError::Keyset),
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn sqlite_keysets_compare_keys_as_numbers() {
    let fx = sqlite_fixture(LITE, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let t = fx.target("t").await;
    let sql = browse_sql(
        &t,
        &Browse::default().with_after(after(&[("id", val("999"))])),
    )
    .unwrap();
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.rows.len(), 1501);
    assert_eq!(ran.rows[0][0], text("1000"));
    let plain = fx.target("plain").await;
    let sql = browse_sql(
        &plain,
        &Browse::default().with_after(after(&[("rowid", val("9"))])),
    )
    .unwrap();
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let want: Vec<Cell> = (10..=20).map(|i| text(&i.to_string())).collect();
    assert_eq!(firsts(&ran.rows), want);
    let pair = fx.target("pair").await;
    let sql = browse_sql(
        &pair,
        &Browse::default().with_after(after(&[("a", val("x")), ("b", val("1"))])),
    )
    .unwrap();
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let ns: Vec<Cell> = ran.rows.iter().map(|r| r[2].clone()).collect();
    assert_eq!(ns, [text("q"), text("r")]);
}

#[tokio::test]
async fn a_batch_skips_its_first_rows() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script(
        "SELECT 1 AS n, NULL AS z, x'00ff' AS b \
         UNION ALL SELECT 2, 'two', x'' UNION ALL SELECT 3, NULL, NULL",
    );
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    let batch = stream.next_batch().await.unwrap().unwrap();
    assert_eq!(batch.len(), 3);
    let rest = batch.skip(1);
    assert_eq!((rest.len(), rest.column_count()), (2, 3));
    for r in 0..2 {
        for c in 0..3 {
            assert_eq!(rest.cell(r, c), batch.cell(r + 1, c), "row {r} col {c}");
        }
    }
    assert_eq!(batch.skip(0).len(), 3);
    assert!(batch.skip(3).is_empty());
    assert!(batch.skip(7).is_empty());
}

#[tokio::test]
async fn postgres_pages_by_keyset_join_up_to_one_full_run() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(PG).await.unwrap();
    let t = fx.target("public", "t").await;
    assert!(t.stable_key());
    assert_eq!(
        browse_sql(&t, &Browse::default()).unwrap(),
        "SELECT *\nFROM public.t\nORDER BY id ASC"
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let full = run(
        &mut s,
        &browse_sql(&t, &Browse::default()).unwrap(),
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap()
    .rows;
    assert_eq!(full.len(), 2500);
    let Cell::Text(last) = &full[999][0] else {
        panic!("an id")
    };
    let sql = browse_sql(
        &t,
        &Browse::default().with_after(after(&[("id", val(last))])),
    )
    .unwrap();
    let rest = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows;
    assert_eq!([&full[..1000], &rest[..]].concat(), full);
    let pair = fx.target("public", "pair").await;
    let sql = browse_sql(
        &pair,
        &Browse::default().with_after(after(&[("a", val("x")), ("b", val("1"))])),
    )
    .unwrap();
    let ran = run(&mut s, &sql, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let ns: Vec<Cell> = ran.rows.iter().map(|r| r[2].clone()).collect();
    assert_eq!(ns, [text("q"), text("r")]);
}

#[tokio::test]
async fn postgres_ctid_browses_keep_their_order_and_refuse_a_keyset() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(PG).await.unwrap();
    let par = fx.target("public", "par").await;
    assert!(!par.stable_key());
    assert_eq!(
        browse_sql(&par, &Browse::default()).unwrap(),
        "SELECT ctid, xmin, *\nFROM ONLY public.par"
    );
    assert_eq!(
        browse_sql(&par, &sorted("a", true)).unwrap(),
        "SELECT ctid, xmin, *\nFROM ONLY public.par\nORDER BY a DESC"
    );
    assert_eq!(
        browse_sql(
            &par,
            &Browse::default().with_after(after(&[("ctid", val("(0,1)")), ("xmin", val("1"))]))
        ),
        Err(BrowseError::Keyset)
    );
}
