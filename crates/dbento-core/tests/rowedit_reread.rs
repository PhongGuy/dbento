//! `reread` returns the values a browse showed, under the browse session's pins.
use crate::rowedit_guard_my::TYPES;
use crate::rowedit_guard_pg::COMPOSITE;
use crate::rowedit_guard_sqlite::SETUP;
use crate::rowedit_pg_torture::TORTURE;
use crate::support::{loaded, my_fixtures, pairs, pg_fixture, row_key, sqlite_fixture, val};
use dbento_core::{Change, SessionOptions, reread};

#[tokio::test]
async fn sqlite_reread_returns_the_browse_values_on_a_read_only_connection() {
    let fx = sqlite_fixture(SETUP, "read_only = true");
    let target = fx.target("odd").await;
    let browsed = loaded(&target, &fx.row("SELECT * FROM odd WHERE id = 1").await);
    let keys = [
        row_key(&[("id", val("1"))]),
        row_key(&[("id", val("9"))]),
        row_key(&[("nope", val("1"))]),
    ];
    let conn = fx.ws.declared("lite").unwrap();
    let got = reread(
        &fx.ws,
        conn,
        None,
        SessionOptions::default(),
        &target,
        &keys,
    )
    .await
    .unwrap();
    assert_eq!(got, vec![Some(browsed), None, None]);
}

#[tokio::test]
async fn postgres_reread_and_the_guard_read_under_the_browse_sessions_time_zone() {
    let fx = pg_fixture("").await;
    let db = fx.db.database().to_owned();
    let admin = fx.db.admin().await;
    admin
        .batch_execute(&format!(
            "CREATE TABLE z(id int PRIMARY KEY, at timestamptz, v text); \
             INSERT INTO z VALUES (1, '2024-01-02 03:04:05+00', 'a'); \
             ALTER DATABASE \"{db}\" SET TimeZone = 'Asia/Kolkata'"
        ))
        .await
        .unwrap();
    let target = fx.target("public", "z").await;
    let browsed = loaded(&target, &fx.row("SELECT * FROM z").await);
    assert_eq!(browsed[1].1, val("2024-01-02 08:34:05+05:30"));
    admin
        .batch_execute(&format!("ALTER DATABASE \"{db}\" RESET TimeZone"))
        .await
        .unwrap();
    let key = row_key(&[("id", val("1"))]);
    let got = reread(
        &fx.ws,
        fx.conn(),
        Some(fx.secret()),
        SessionOptions::default(),
        &target,
        std::slice::from_ref(&key),
    )
    .await
    .unwrap();
    assert_eq!(got, vec![Some(browsed.clone())]);
    let applied = fx
        .apply_changes(
            &target,
            vec![Change::Update {
                key,
                set: pairs(&[("v", val("b"))]),
                expected: browsed[1..].to_vec(),
            }],
        )
        .await
        .unwrap();
    applied.rollback().await.unwrap();
}

#[tokio::test]
async fn postgres_reread_on_a_read_only_connection_reads_an_all_null_composite_as_text() {
    let fx = pg_fixture("read_only = true").await;
    fx.db.admin().await.batch_execute(COMPOSITE).await.unwrap();
    let target = fx.target("public", "cn").await;
    let mut browsed = Vec::new();
    for id in ["1", "2"] {
        browsed.push(Some(loaded(
            &target,
            &fx.row(&format!("SELECT * FROM cn WHERE id = {id}")).await,
        )));
    }
    let keys = [row_key(&[("id", val("1"))]), row_key(&[("id", val("2"))])];
    let got = reread(
        &fx.ws,
        fx.conn(),
        Some(fx.secret()),
        SessionOptions::default(),
        &target,
        &keys,
    )
    .await
    .unwrap();
    assert_eq!(got, browsed);
}

#[tokio::test]
async fn postgres_reread_of_the_type_torture_row_equals_its_browse() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
    let target = fx.target("public", "tt").await;
    let browsed = loaded(&target, &fx.row("SELECT * FROM tt WHERE id = 1").await);
    let got = reread(
        &fx.ws,
        fx.conn(),
        Some(fx.secret()),
        SessionOptions::default(),
        &target,
        &[row_key(&[("id", val("1"))])],
    )
    .await
    .unwrap();
    assert_eq!(got, vec![Some(browsed)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_reread_of_every_guarded_type_equals_its_browse() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(TYPES).await;
        let target = fx.target("ty").await;
        let browsed = loaded(&target, &fx.row("SELECT * FROM ty").await);
        let keys = [row_key(&[("id", val("1"))]), row_key(&[("id", val("2"))])];
        let got = reread(
            &fx.ws,
            fx.conn(),
            Some(fx.secret()),
            SessionOptions::default(),
            &target,
            &keys,
        )
        .await
        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(got, vec![Some(browsed), None], "{label}");
    }
}
