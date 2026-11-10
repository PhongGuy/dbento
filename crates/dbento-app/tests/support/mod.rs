//! Fixtures shared by the app's integration tests.
#![allow(dead_code)]
use dbento_app::app::{AppPaths, Services};
use dbento_app::explorer::connection_id;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, TestAppContext};
use std::path::Path;
use std::sync::Arc;

/// gpui-kit with the Dbento theme and reduced motion; every test window starts here.
pub fn init(cx: &mut TestAppContext) {
    init_then(cx, |_| {});
}
/// [`init`], then `then` in the same update, for tests of what happens before any effect runs.
pub fn init_then(cx: &mut TestAppContext, then: impl FnOnce(&mut gpui_kit::App)) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        dbento_app::theme::install(cx);
        cx.set_reduce_motion(true);
        then(cx);
    });
}
/// gpui-kit alone, without the theme or the keymap table, for tests of what runs before
/// `keymap::bind`; such a test opens no window.
pub fn init_kit_only(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
}
/// The tokio bridge, for tests whose runners touch real databases.
pub fn init_tokio(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(dbento_app::tokio_bridge::init);
}
/// Services over files in `dir`, an empty in-memory keychain and no environment.
pub fn services(dir: &Path) -> Services {
    Services {
        paths: AppPaths::in_dir(dir),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(|_| None),
        secrets_in_memory: false,
    }
}
pub fn present(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    cx.run_until_parked();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find(id).is_some()
    })
    .unwrap()
}
pub fn label_of(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    id: impl Into<ElementId>,
) -> Option<String> {
    let id = id.into();
    cx.run_until_parked();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find(id).and_then(|e| e.label().map(str::to_owned))
    })
    .unwrap()
}
/// The labels of the palette's rendered rows, in order.
pub fn palette_rows(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|n| label_of(cx, win, ("palette-row", n)))
        .collect()
}
/// Waits for the palette row labelled `label` and clicks it.
pub fn palette_pick(cx: &mut TestAppContext, win: AnyWindowHandle, label: &str) {
    wait_real(cx, label, |cx| {
        palette_rows(cx, win).iter().any(|r| r == label)
    });
    let n = palette_rows(cx, win)
        .iter()
        .position(|r| r == label)
        .unwrap();
    click_id(cx, win, ("palette-row", n));
}
/// Polls against a wall-clock deadline: real databases answer on tokio threads.
pub fn wait_real(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

use dbento_app::runner::RunnerSpec;
use dbento_core::Workspace;
pub struct Fx {
    pub dir: tempfile::TempDir,
    pub ws: Arc<Workspace>,
}
/// `lite`: SQLite over `data/app.db`, built by `setup`, with `extra` lines in its table.
/// `nopg`: Postgres on the closed port 127.0.0.1:9 with a keychain secret.
pub fn fixture(setup: &str, extra: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    rusqlite::Connection::open(dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch(setup)
        .unwrap();
    std::fs::write(dir.path().join("dbento.toml"), format!(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n{extra}\n[connections.nopg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n")).unwrap();
    let ws = Arc::new(Workspace::open(dir.path()).unwrap());
    Fx { dir, ws }
}
impl Fx {
    pub fn count(&self, sql: &str) -> i64 {
        rusqlite::Connection::open(self.dir.path().join("data/app.db"))
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }
}
pub fn spec(fx: &Fx, conn: &str) -> RunnerSpec {
    RunnerSpec {
        workspace: Arc::clone(&fx.ws),
        connection: conn.into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(|_| None),
    }
}
pub const MANY: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r";
pub const LONG: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT count(*) FROM r";

/// The columns and batches of `sql` on a fresh SQLite fixture, straight from core.
pub fn batches(
    sql: &str,
) -> (
    Arc<[dbento_core::ColumnInfo]>,
    Vec<dbento_core::ResultBatch>,
) {
    let fx = fixture("", "");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let conn = fx.ws.connection("lite").unwrap();
        let mut s = dbento_core::connect(&fx.ws, conn, None, Default::default())
            .await
            .unwrap();
        let mut script = s.script(sql);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Default::default())
            .await
            .unwrap();
        let columns: Arc<[dbento_core::ColumnInfo]> = stream.columns().into();
        let mut out = Vec::new();
        while let Some(b) = stream.next_batch().await.unwrap() {
            out.push(b);
        }
        drop(stream);
        drop(script);
        s.close().await.unwrap();
        (columns, out)
    })
}

/// `main.<table>` of the fixture's `lite` connection described for editing, straight from core.
pub fn edit_target(fx: &Fx, table: &str) -> dbento_core::EditTarget {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let conn = fx.ws.connection("lite").unwrap();
        let mut s = dbento_core::connect(&fx.ws, conn, None, Default::default())
            .await
            .unwrap();
        let t = s
            .edit_target(&dbento_core::RelationRef::new("main", table))
            .await
            .unwrap();
        s.close().await.unwrap();
        t
    })
}

pub struct Opened {
    pub fx: Fx,
    pub files: tempfile::TempDir,
    pub win: AnyWindowHandle,
    pub shell: gpui_kit::Entity<dbento_app::shell::Shell>,
}
/// Double-clicks connection `name`'s explorer row, which opens or activates its tab.
pub fn open_connection_row(cx: &mut TestAppContext, win: AnyWindowHandle, name: &str) {
    cx.update_window(win, |_, w, cx| w.double_click(connection_id(name), cx))
        .unwrap();
    cx.run_until_parked();
}
/// The app on a fixture workspace with connection `ix` double-clicked open.
pub fn opened(cx: &mut TestAppContext, setup: &str, extra: &str, ix: usize) -> Opened {
    init(cx);
    init_tokio(cx);
    let (fx, files) = (fixture(setup, extra), tempfile::tempdir().unwrap());
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    let name = fx.ws.connections()[ix].name().to_owned();
    open_connection_row(cx, win, &name);
    Opened {
        fx,
        files,
        win,
        shell,
    }
}
impl Opened {
    pub fn model(
        &self,
        cx: &mut TestAppContext,
    ) -> gpui_kit::Entity<dbento_app::tab_model::TabModel> {
        self.shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone())
    }
    pub fn run(&self, cx: &mut TestAppContext, sql: &str) {
        let tab = self.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
        cx.update_window(self.win, |_, window, cx| {
            tab.update(cx, |t, cx| t.set_sql(sql, window, cx))
        })
        .unwrap();
        cx.update_window(self.win, |_, w, cx| w.click("run-all", cx))
            .unwrap();
        cx.run_until_parked();
    }
    pub fn idle(&self, cx: &mut TestAppContext) {
        let m = self.model(cx);
        wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    }
}

/// The app on `fixture(setup, extra)` with nothing opened.
pub fn launched(cx: &mut TestAppContext, setup: &str, extra: &str) -> Opened {
    init(cx);
    init_tokio(cx);
    let (fx, files) = (fixture(setup, extra), tempfile::tempdir().unwrap());
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Opened {
        fx,
        files,
        win,
        shell,
    }
}
impl Opened {
    /// Opens `main.<name>` of connection 0 as the explorer's double click does.
    pub fn open_relation(
        &self,
        cx: &mut TestAppContext,
        name: &str,
        kind: dbento_core::RelationKind,
    ) {
        let shell = self.shell.clone();
        let rel = dbento_core::RelationRef::new("main", name);
        cx.update_window(self.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_relation(0, &rel, kind, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    /// The active tab's first ended result's SQL, its editor text and its browse SQL.
    pub fn browse_shown(
        &self,
        cx: &mut TestAppContext,
    ) -> Option<(String, String, Option<String>)> {
        self.shell.read_with(cx, |s, cx| {
            let tab = s.tab()?.read(cx);
            let m = tab.model().read(cx);
            let r = m.results().first()?;
            r.store.end()?;
            Some((
                r.sql.clone(),
                tab.sql(cx),
                m.browse().map(|b| b.sql().to_owned()),
            ))
        })
    }
    /// Waits until the active tab ran `sql`, shows it in the editor and browses with it.
    pub fn wait_browse(&self, cx: &mut TestAppContext, sql: &str) {
        let want = Some((sql.to_owned(), sql.to_owned(), Some(sql.to_owned())));
        wait_real(cx, sql, |cx| self.browse_shown(cx) == want);
    }
}

/// A folder tree: `lite` two levels down in blue `team/sub`, `top` at the top level.
pub const TREE: &str = "dbento = 1\n[folders.team]\ncolor = \"blue\"\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"l.db\"\nfolder = \"team/sub\"\n\n[connections.top]\nengine = \"sqlite\"\npath = \"t.db\"\n";
pub struct Started {
    pub dir: tempfile::TempDir,
    pub files: tempfile::TempDir,
    pub win: AnyWindowHandle,
    pub shell: gpui_kit::Entity<dbento_app::shell::Shell>,
}
/// The app on a workspace whose dbento.toml is `toml`, nothing opened.
pub fn started(cx: &mut TestAppContext, toml: &str) -> Started {
    started_with(cx, toml, Arc::new(dbento_core::MemoryStore::new()))
}
/// As `started`, with `store` as the keychain.
pub fn started_with(
    cx: &mut TestAppContext,
    toml: &str,
    store: Arc<dyn dbento_core::SecretStore>,
) -> Started {
    started_custom(cx, toml, |s| s.store = store)
}
/// As `started`, with `adjust` applied to the services first.
pub fn started_custom(
    cx: &mut TestAppContext,
    toml: &str,
    adjust: impl FnOnce(&mut Services),
) -> Started {
    init(cx);
    init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let mut services = services(files.path());
    adjust(&mut services);
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services, Some(dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Started {
        dir,
        files,
        win,
        shell,
    }
}
impl Started {
    pub fn toml(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("dbento.toml")).unwrap()
    }
    pub fn sidebar(
        &self,
        cx: &mut TestAppContext,
    ) -> gpui_kit::Entity<dbento_app::sidebar::Sidebar> {
        self.shell
            .read_with(cx, |s, _| s.sidebar().cloned().unwrap())
    }
}

use async_channel::Receiver;
use dbento_app::runner::{AppLimits, Command, Event};
/// The next runner event, failing after 20 s.
pub async fn next_event(rx: &Receiver<Event>) -> Event {
    tokio::time::timeout(std::time::Duration::from_secs(20), rx.recv())
        .await
        .expect("an event within 20 s")
        .expect("the runner is alive")
}
/// Whether no event arrives for 300 ms. A fast negative check that can pass falsely under
/// load: every caller must follow it with a positive check that a stray event would break.
pub async fn quiet(rx: &Receiver<Event>) -> bool {
    tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv())
        .await
        .is_err()
}
/// Events up to and including the first `stop` accepts, and the rows their batches held.
pub async fn until_event(
    rx: &Receiver<Event>,
    stop: impl Fn(&Event) -> bool,
) -> (Vec<Event>, usize) {
    let (mut seen, mut rows) = (Vec::new(), 0);
    loop {
        let e = next_event(rx).await;
        if let Event::Batch { batch, .. } = &e {
            rows += batch.len();
        }
        let done = stop(&e);
        seen.push(e);
        if done {
            return (seen, rows);
        }
    }
}
pub fn run_cmd(run_id: u64, sql: &str, max_rows: Option<u64>) -> Command {
    Command::Run {
        run_id,
        sql: sql.into(),
        limits: AppLimits {
            max_rows,
            ..AppLimits::default()
        },
    }
}
pub fn here() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}
/// A runner spec on a fresh Postgres database; `extra` joins the connection's table.
pub async fn pg_spec(extra: &str) -> (dbento_testkit::PgDb, tempfile::TempDir, RunnerSpec) {
    let db = dbento_testkit::pg_db().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", extra)),
    )
    .unwrap();
    let password = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "pg".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::SECRET_VAR).then(|| password.clone())),
    };
    (db, dir, spec)
}

/// A runner spec on `db` as connection `my` with `extra` TOML lines; the env closure answers
/// the fixture's secret variable, as the app's environment would.
pub fn my_spec(db: &dbento_testkit::MyDb, extra: &str) -> (tempfile::TempDir, RunnerSpec) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("my", extra)),
    )
    .unwrap();
    let password = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "my".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::MY_SECRET_VAR).then(|| password.clone())),
    };
    (dir, spec)
}

/// A runner spec on `db` as connection `ms` with `extra` TOML lines; the env closure answers
/// the fixture's secret variable, as the app's environment would.
pub fn ms_spec(db: &dbento_testkit::MsDb, extra: &str) -> (tempfile::TempDir, RunnerSpec) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("ms", extra)),
    )
    .unwrap();
    let password = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "ms".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::MS_SECRET_VAR).then(|| password.clone())),
    };
    (dir, spec)
}

/// Rewrites the fixture's dbento.toml: `lite` with `lite_extra`, then `nopg`, then `more`.
pub fn rewrite(fx: &Fx, lite_extra: &str, more: &str) {
    std::fs::write(
        fx.dir.path().join("dbento.toml"),
        format!("dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n{lite_extra}\n[connections.nopg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n{more}"),
    )
    .unwrap();
}
/// Reloads dbento.toml through the empty-space menu's `menu-reload`.
pub fn reload(cx: &mut TestAppContext, win: AnyWindowHandle) {
    right_click_empty_in(cx, win);
    click_id(cx, win, "menu-reload");
}
/// Moves the pointer onto `id` and lets the app settle.
pub fn hover(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.hover(id, cx)).unwrap();
    cx.run_until_parked();
}
/// Hovers connection `name`'s row, then clicks its gear.
pub fn gear(cx: &mut TestAppContext, win: AnyWindowHandle, name: &str) {
    hover(cx, win, connection_id(name));
    click_id(
        cx,
        win,
        dbento_app::explorer::part_id("edit-connection", name),
    );
}

/// The open connection dialog's form, once it shows.
pub fn form(
    cx: &mut TestAppContext,
    o: &Opened,
) -> gpui_kit::Entity<dbento_app::connection_dialog::ConnectionForm> {
    wait_real(cx, "the connection dialog", |cx| {
        present(cx, o.win, "conn-name")
    });
    o.shell
        .read_with(cx, |s, _| s.connection_form().cloned().unwrap())
}
/// Sets one text field of the open form.
pub fn fill(
    cx: &mut TestAppContext,
    o: &Opened,
    field: dbento_app::connection_dialog::Field,
    text: &str,
) {
    let state = form(cx, o).read_with(cx, |f, _| f.input(field).clone());
    cx.update_window(o.win, |_, w, cx| {
        state.update(cx, |s, cx| s.set_value(text.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
/// Clicks `id` in the window and lets the app settle.
pub fn click_id(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent, point, px};
/// Right-clicks the element `id` and lets the deferred menu build.
pub fn right_click(cx: &mut TestAppContext, s: &Started, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(s.win, |_, w, cx| w.right_click(id, cx))
        .unwrap();
    cx.run_until_parked();
}
/// Presses and releases the middle button over the element `id`; the kit has no middle click.
pub fn middle_click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let at = w.find(id).bounds().center();
        let down = MouseDownEvent {
            button: MouseButton::Middle,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        };
        w.dispatch_event(down.to_platform_input(), cx);
        let up = MouseUpEvent {
            button: MouseButton::Middle,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
        };
        w.dispatch_event(up.to_platform_input(), cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}
/// Right-clicks the empty list space below the last explorer row.
pub fn right_click_empty(cx: &mut TestAppContext, s: &Started) {
    right_click_empty_in(cx, s.win);
}
/// Right-clicks the empty list space below the last explorer row of `win`.
pub fn right_click_empty_in(cx: &mut TestAppContext, win: AnyWindowHandle) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let body = w.find("explorer-body").bounds();
        let at = point(body.origin.x + px(40.), body.bottom() - px(8.));
        let down = MouseDownEvent {
            button: MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        };
        w.dispatch_event(down.to_platform_input(), cx);
        let up = MouseUpEvent {
            button: MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
        };
        w.dispatch_event(up.to_platform_input(), cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}
/// Types `text` into the open name dialog and presses its OK button.
pub fn name(cx: &mut TestAppContext, s: &Started, text: &str) {
    wait_real(cx, "the name dialog", |cx| present(cx, s.win, "save-name"));
    cx.update_window(s.win, |_, w, cx| {
        w.press("ctrl-a", cx);
        w.input(text, cx);
    })
    .unwrap();
    click_id(cx, s.win, "save-ok");
}

/// The fixture's SQLite file.
pub fn db(fx: &Fx) -> std::path::PathBuf {
    fx.dir.path().join("data/app.db")
}
/// A tab model on `lite` browsing `main.<table>` that runs every browse SQL it asks for;
/// `before_run` is executed against the database just before each browse run.
pub fn browsing(
    cx: &mut TestAppContext,
    fx: &Fx,
    table: &str,
    tuning: dbento_app::runner::RunnerTuning,
    before_run: &'static str,
) -> gpui_kit::Entity<dbento_app::tab_model::TabModel> {
    init_tokio(cx);
    let m = cx.new(|_| dbento_app::tab_model::TabModel::with_tuning(spec(fx, "lite"), tuning));
    let path = db(fx);
    cx.update(|cx| {
        cx.subscribe(&m, move |m, e: &dbento_app::browse::BrowseEvent, cx| {
            if let dbento_app::browse::BrowseEvent::Run(sql) = e {
                if !before_run.is_empty() {
                    rusqlite::Connection::open(&path)
                        .unwrap()
                        .execute_batch(before_run)
                        .unwrap();
                }
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(dbento_core::RelationRef::new("main", table), cx)
    });
    m
}
/// `browsing` with the default tuning, once the result is editable and the run has ended.
pub fn ready(
    cx: &mut TestAppContext,
    fx: &Fx,
    table: &str,
) -> gpui_kit::Entity<dbento_app::tab_model::TabModel> {
    let m = browsing(cx, fx, table, Default::default(), "");
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    m
}
/// An editable browse of `public.<table>` on a fresh Postgres database built by `setup`.
/// The caller holds `rt.enter()` for the whole test, since a container's drop needs one.
pub fn pg_ready(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Handle,
    setup: &str,
    table: &str,
) -> (
    dbento_testkit::PgDb,
    tempfile::TempDir,
    gpui_kit::Entity<dbento_app::tab_model::TabModel>,
) {
    let (db, dir, spec) = rt.block_on(pg_spec(""));
    rt.block_on(async { db.admin().await.batch_execute(setup).await.unwrap() });
    let m = cx.new(|_| dbento_app::tab_model::TabModel::new(spec));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &dbento_app::browse::BrowseEvent, cx| {
            if let dbento_app::browse::BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(dbento_core::RelationRef::new("public", table), cx)
    });
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    (db, dir, m)
}

/// Runs `sql` on the fixture's SQLite file from a separate connection.
pub fn exec(fx: &Fx, sql: &str) {
    rusqlite::Connection::open(db(fx))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}
/// Column `col` of the row of `table` whose `id` is `id`, as text.
pub fn value_of(fx: &Fx, table: &str, col: &str, id: i64) -> String {
    rusqlite::Connection::open(db(fx))
        .unwrap()
        .query_row(
            &format!("SELECT {col} FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
pub fn wait_prompt(cx: &mut TestAppContext, m: &gpui_kit::Entity<dbento_app::tab_model::TabModel>) {
    wait_real(cx, "the apply prompt", |cx| {
        m.read_with(cx, |m, _| m.apply_prompt().is_some())
    });
}
pub fn wait_idle(cx: &mut TestAppContext, m: &gpui_kit::Entity<dbento_app::tab_model::TabModel>) {
    wait_real(cx, "the save to settle", |cx| {
        m.read_with(cx, |m, _| {
            m.save_state() == dbento_app::tab_model::SaveState::Idle
        })
    });
}
/// Polls until a write to `t` succeeds without waiting on a lock.
pub fn wait_unlocked(cx: &mut TestAppContext, fx: &Fx) {
    let path = db(fx);
    wait_real(cx, "the write lock to go", |_| {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.busy_timeout(std::time::Duration::ZERO).unwrap();
        c.execute_batch("UPDATE t SET name = name WHERE id = 2")
            .is_ok()
    });
}

impl Opened {
    /// Whether edit-strip button `id` is shown and the pane's `strip_actions` enables it.
    pub fn strip_enabled(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        let pane = self
            .shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
        present(cx, self.win, id)
            && pane.read_with(cx, |p, cx| p.strip_actions(cx).enabled(id)) == Some(true)
    }
    /// Asserts strip button `id` is shown, disabled by `strip_actions`, and that a click on
    /// it changes no staged edit, history step or save state. gpui-pre 0.3.8 writes no
    /// accesskit disabled flag, so the click stands in for `ElementSnapshot::disabled()`.
    pub fn assert_inert(&self, cx: &mut TestAppContext, id: &'static str) {
        assert!(present(cx, self.win, id), "{id} is shown");
        assert!(!self.strip_enabled(cx, id), "{id} is disabled");
        let m = self.model(cx);
        let state = |cx: &mut TestAppContext| {
            m.read_with(cx, |m, _| {
                (
                    m.edits().clone(),
                    m.can_undo(),
                    m.can_redo(),
                    m.save_state(),
                )
            })
        };
        let before = state(cx);
        click_id(cx, self.win, id);
        assert_eq!(state(cx), before, "a click on {id} changed nothing");
        assert!(
            !present(cx, self.win, "confirm-dialog"),
            "{id} asked nothing"
        );
    }
}
