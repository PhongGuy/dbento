//! Postgres through the explorer: Chinook opens album as a SELECT, and a schema shows its objects.
use crate::support;
use crate::support::{label_of, wait_real};
use dbento_app::app::{AppPaths, Services, start};
use dbento_app::explorer::part_id;
use dbento_app::schema_tree::RowKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};
use std::sync::Arc;

fn tree(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("tree-row", i)))
        .collect()
}

#[gpui_kit::test]
fn chinook_expands_and_opens_album(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    // The container's drop needs a runtime, and it outlives `db`.
    let _enter = rt.enter();
    let (db, dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute(dbento_testkit::chinook_pg_sql())
            .await
            .unwrap()
    });
    let files = tempfile::tempdir().unwrap();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: Arc::clone(&spec.store),
        env: Arc::clone(&spec.env),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    assert!(support::present(cx, win, "schema-tree"));
    cx.update_window(win, |_, w, cx| {
        w.click(part_id("connection-toggle", "pg"), cx)
    })
    .unwrap();
    let tables = [
        "album",
        "artist",
        "customer",
        "employee",
        "genre",
        "invoice",
        "invoice_line",
        "media_type",
        "playlist",
        "playlist_track",
        "track",
    ];
    let mut want = vec!["pg.public".to_owned(), "pg.public/tables".to_owned()];
    want.extend(tables.iter().map(|t| format!("pg.public.{t}")));
    wait_real(cx, "tables", |cx| tree(cx, win) == want);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_real(cx, "the tab", |cx| {
        shell.read_with(cx, |s, _| s.tab().is_some())
    });
    let model = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone());
    wait_real(cx, "the result", |cx| {
        model.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.results()[0].sql,
            "SELECT *\nFROM public.album\nORDER BY album_id ASC"
        );
        assert_eq!(m.results()[0].store.rows(), 347);
    });
}

const OBJECTS: &str = "CREATE TYPE mood AS ENUM ('sad', 'ok'); \
    CREATE TABLE t(id int PRIMARY KEY, email text UNIQUE, m mood); \
    CREATE INDEX t_lower_email ON t (lower(email)); \
    CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql AS 'SELECT a + b'; \
    CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'; \
    CREATE TRIGGER t_touch BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION touch();";

#[gpui_kit::test]
fn a_postgres_schema_shows_types_routines_and_table_objects(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (db, dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async { db.admin().await.batch_execute(OBJECTS).await.unwrap() });
    let files = tempfile::tempdir().unwrap();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: Arc::clone(&spec.store),
        env: Arc::clone(&spec.env),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
        cx.run_until_parked();
    };
    let at = |cx: &mut TestAppContext, aria: &str| {
        tree(cx, win)
            .iter()
            .position(|r| r == aria)
            .unwrap_or_else(|| panic!("no {aria} in {:?}", tree(cx, win)))
    };
    let shows = |cx: &mut TestAppContext, rows: &[&str]| {
        let now = tree(cx, win);
        rows.iter().all(|r| now.iter().any(|n| n == r))
    };
    click(cx, part_id("connection-toggle", "pg"));
    let top = [
        "pg.public",
        "pg.public/tables",
        "pg.public.t",
        "pg.public/types",
        "pg.public/routines",
    ];
    wait_real(cx, "the schema's groups", |cx| tree(cx, win) == top);
    let ix = at(cx, "pg.public/routines");
    click(cx, ("tree-toggle", ix).into());
    let routines = [
        "pg.public/routines.add(a integer, b integer)",
        "pg.public/routines.touch()",
    ];
    wait_real(cx, "the routines", |cx| shows(cx, &routines));
    let ix = at(cx, "pg.public/types");
    click(cx, ("tree-toggle", ix).into());
    wait_real(cx, "the enum", |cx| shows(cx, &["pg.public/types.mood"]));
    let ix = at(cx, "pg.public.t");
    click(cx, ("tree-toggle", ix).into());
    let groups = [
        "pg.public.t/keys",
        "pg.public.t/indexes",
        "pg.public.t/triggers",
    ];
    wait_real(cx, "t's groups", |cx| shows(cx, &groups));
    for group in [
        "pg.public.t/keys",
        "pg.public.t/indexes",
        "pg.public.t/triggers",
    ] {
        let ix = at(cx, group);
        click(cx, ("tree-toggle", ix).into());
    }
    let objects = [
        "pg.public.t/keys.t_pkey",
        "pg.public.t/keys.t_email_key",
        "pg.public.t/indexes.t_lower_email",
        "pg.public.t/triggers.t_touch",
    ];
    wait_real(cx, "t's objects", |cx| shows(cx, &objects));
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let detail = |cx: &mut TestAppContext, aria: &str| {
        sidebar.read_with(cx, |s, _| {
            s.tree()
                .rows("pg", 1, "")
                .into_iter()
                .find(|r| r.aria == aria)
                .and_then(|r| match r.kind {
                    RowKind::Object { detail, .. } => Some(detail),
                    RowKind::Schema
                    | RowKind::Kind(_)
                    | RowKind::Relation(_)
                    | RowKind::Column { .. }
                    | RowKind::Group(_)
                    | RowKind::Loading
                    | RowKind::Failed { .. }
                    | RowKind::Truncated
                    | RowKind::Empty
                    | RowKind::KeyFolder { .. }
                    | RowKind::Key(_)
                    | RowKind::LoadMore
                    | RowKind::Database { .. } => None,
                })
        })
    };
    assert_eq!(detail(cx, "pg.public/types.mood").as_deref(), Some("enum"));
    assert_eq!(detail(cx, routines[0]).as_deref(), Some("integer"));
    assert_eq!(
        detail(cx, "pg.public.t/keys.t_pkey").as_deref(),
        Some("primary key")
    );
    assert_eq!(
        detail(cx, "pg.public.t/indexes.t_lower_email").as_deref(),
        Some("btree")
    );
    // Enter on the function opens its engine text in the dock.
    let ix = at(cx, routines[0]);
    click(cx, ("tree-row", ix).into());
    cx.update_window(win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    let panel = shell.read_with(cx, |s, _| s.definition_panel().clone());
    wait_real(cx, "the function's definition", |cx| {
        panel.read_with(cx, |p, _| {
            p.text().is_some_and(|t| {
                t.starts_with("CREATE OR REPLACE FUNCTION public.add(a integer, b integer)")
            })
        })
    });
    assert_eq!(
        label_of(cx, win, "definition-title").as_deref(),
        Some("Routine public.add(a integer, b integer)")
    );
}
