//! The Databases choice under Database: which of a Postgres or SQL Server parent's databases
//! become child connections, as `databases` in `dbento.toml`.

use crate::support;

use crate::support::{click_id, fill, form, label_of, opened, present};
use dbento_app::connection_dialog::{ConnectionForm, DatabasesChoice, Field, TestState};
use dbento_core::{ConnectionSpec, Databases, Engine};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext};
use std::fs;

fn open(
    cx: &mut TestAppContext,
    existing: Option<(&str, ConnectionSpec)>,
) -> (AnyWindowHandle, Entity<ConnectionForm>) {
    support::init(cx);
    let existing = existing.map(|(n, s)| (n.to_owned(), s));
    cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| ConnectionForm::new(existing, Vec::new(), false, window, cx))
        })
    })
    .unwrap()
}
fn set(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    f: &Entity<ConnectionForm>,
    field: Field,
    text: &str,
) {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    cx.update_window(win, |_, w, cx| {
        state.update(cx, |s, cx| s.set_value(text.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn value(cx: &mut TestAppContext, f: &Entity<ConnectionForm>, field: Field) -> String {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    state.read_with(cx, |s, _| s.value().to_string())
}
fn click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn choose(cx: &mut TestAppContext, win: AnyWindowHandle, choice: DatabasesChoice) {
    cx.update_window(win, |_, w, cx| {
        w.within("conn-databases").click(choice as usize, cx)
    })
    .unwrap();
    cx.run_until_parked();
}
fn tick(cx: &mut TestAppContext, win: AnyWindowHandle, ix: usize) {
    click(cx, win, ("conn-databases-item", ix));
}
// What a Test that connected hands the form.
fn listed(cx: &mut TestAppContext, f: &Entity<ConnectionForm>, names: &[&str]) {
    let names = names.iter().map(|n| (*n).to_owned()).collect();
    f.update(cx, |f, cx| {
        f.set_test(TestState::Connected, cx);
        f.set_databases(names, cx);
    });
    cx.run_until_parked();
}
fn edit(
    cx: &mut TestAppContext,
    f: &Entity<ConnectionForm>,
) -> Result<(String, ConnectionSpec), String> {
    f.read_with(cx, |f, cx| f.edit(cx))
}
fn databases(cx: &mut TestAppContext, f: &Entity<ConnectionForm>) -> Option<Databases> {
    edit(cx, f).unwrap().1.databases
}
fn pg() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Postgres);
    (s.host, s.database, s.user) = (
        Some("127.0.0.1".into()),
        Some("app".into()),
        Some("me".into()),
    );
    s
}
fn ms() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Mssql);
    (s.host, s.user) = (Some("127.0.0.1".into()), Some("sa".into()));
    s
}
fn names(list: &[&str]) -> Databases {
    Databases::Listed(list.iter().map(|n| (*n).to_owned()).collect())
}

#[gpui_kit::test]
fn each_choice_writes_its_databases_value(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("srv", pg())));
    assert!(present(cx, win, "conn-databases"));
    assert_eq!(databases(cx, &f), None, "a plain connection stays Only");
    choose(cx, win, DatabasesChoice::All);
    assert_eq!(databases(cx, &f), Some(Databases::All));
    listed(cx, &f, &["app", "billing", "audit", "postgres"]);
    choose(cx, win, DatabasesChoice::Choose);
    assert!(present(cx, win, "conn-databases-items"));
    tick(cx, win, 2);
    tick(cx, win, 1);
    assert_eq!(
        databases(cx, &f),
        Some(names(&["billing", "audit"])),
        "the list's order, not the order ticked"
    );
    tick(cx, win, 2);
    assert_eq!(databases(cx, &f), Some(names(&["billing"])));
    choose(cx, win, DatabasesChoice::Only);
    assert_eq!(databases(cx, &f), None);
}

#[gpui_kit::test]
fn choose_with_nothing_ticked_is_refused(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("srv", pg())));
    choose(cx, win, DatabasesChoice::Choose);
    assert!(
        present(cx, win, "conn-databases-untested"),
        "before a Test, the list says how to fill it"
    );
    let refused = edit(cx, &f).unwrap_err();
    assert!(refused.contains("Tick at least one database"), "{refused}");
}

#[gpui_kit::test]
fn all_on_postgres_fills_an_empty_database_with_postgres(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    set(cx, win, &f, Field::Name, "srv");
    set(cx, win, &f, Field::Host, "127.0.0.1");
    set(cx, win, &f, Field::User, "me");
    choose(cx, win, DatabasesChoice::All);
    assert_eq!(
        value(cx, &f, Field::Database),
        "postgres",
        "filled in sight"
    );
    let (_, spec) = edit(cx, &f).unwrap();
    assert_eq!(
        (spec.database.as_deref(), spec.databases),
        (Some("postgres"), Some(Databases::All))
    );
    set(cx, win, &f, Field::Database, "");
    let refused = edit(cx, &f).unwrap_err();
    assert!(refused.contains("such as postgres"), "{refused}");
    set(cx, win, &f, Field::Database, "app");
    choose(cx, win, DatabasesChoice::Choose);
    assert_eq!(
        value(cx, &f, Field::Database),
        "app",
        "a named database stays"
    );
}

#[gpui_kit::test]
fn sql_server_keeps_database_optional(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    choose(cx, win, DatabasesChoice::All);
    assert_eq!(value(cx, &f, Field::Database), "");
    let (_, spec) = edit(cx, &f).unwrap();
    assert_eq!(
        (spec.database, spec.databases),
        (None, Some(Databases::All))
    );
}

#[gpui_kit::test]
fn sql_server_ticks_names_that_differ_only_in_case_as_one(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    listed(cx, &f, &["App", "app", "billing"]);
    choose(cx, win, DatabasesChoice::Choose);
    tick(cx, win, 0);
    assert_eq!(databases(cx, &f), Some(names(&["App"])));
    // `app` shows ticked as the same database, so clicking it unticks `App`.
    tick(cx, win, 1);
    assert!(f.read_with(cx, |f, _| f.chosen_databases().is_empty()));
}

#[gpui_kit::test]
fn sql_server_refuses_a_stored_pair_that_differs_only_in_case(cx: &mut TestAppContext) {
    let mut spec = ms();
    spec.databases = Some(names(&["App", "app"]));
    let (_, f) = open(cx, Some(("ms", spec)));
    let refused = edit(cx, &f).unwrap_err();
    assert!(
        refused.contains("App and app are one database on SQL Server"),
        "{refused}"
    );
}

#[gpui_kit::test]
fn a_name_that_cant_be_a_connection_is_disabled(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("srv", pg())));
    listed(cx, &f, &["app", "my.db", "with space"]);
    choose(cx, win, DatabasesChoice::Choose);
    tick(cx, win, 1);
    tick(cx, win, 2);
    tick(cx, win, 0);
    assert_eq!(
        f.read_with(cx, |f, _| f.chosen_databases().to_vec()),
        ["app"],
        "the disabled rows don't tick"
    );
    assert_eq!(databases(cx, &f), Some(names(&["app"])));
}

#[gpui_kit::test]
fn an_edited_parent_shows_and_keeps_its_stored_list(cx: &mut TestAppContext) {
    let mut spec = pg();
    spec.databases = Some(names(&["billing", "gone"]));
    let (win, f) = open(cx, Some(("srv", spec.clone())));
    assert_eq!(
        f.read_with(cx, |f, _| f.databases_choice()),
        DatabasesChoice::Choose
    );
    assert_eq!(
        f.read_with(cx, |f, _| f.chosen_databases().to_vec()),
        ["billing", "gone"],
        "before a Test the stored names show, ticked"
    );
    assert_eq!(
        label_of(cx, win, ("conn-databases-item", 1usize)).as_deref(),
        Some("gone")
    );
    assert_eq!(edit(cx, &f), Ok(("srv".into(), spec.clone())));
    listed(cx, &f, &["app", "billing", "postgres"]);
    assert_eq!(
        f.read_with(cx, |f, _| f.database_items()),
        ["app", "billing", "postgres", "gone"],
        "a stored name the server didn't return comes after the server's"
    );
    assert!(present(cx, win, ("conn-databases-gone", 3usize)));
    assert!(!present(cx, win, ("conn-databases-gone", 1usize)));
    assert_eq!(edit(cx, &f), Ok(("srv".into(), spec)), "still unchanged");
    tick(cx, win, 3);
    assert_eq!(databases(cx, &f), Some(names(&["billing"])));
    assert!(
        present(cx, win, ("conn-databases-item", 3usize)),
        "an unticked stored name stays in the list to tick again"
    );
}

#[gpui_kit::test]
fn an_all_parent_opens_on_all_and_returns_unchanged(cx: &mut TestAppContext) {
    let mut spec = pg();
    spec.databases = Some(Databases::All);
    let (win, f) = open(cx, Some(("srv", spec.clone())));
    assert_eq!(
        f.read_with(cx, |f, _| f.databases_choice()),
        DatabasesChoice::All
    );
    assert!(!present(cx, win, "conn-databases-items"));
    assert_eq!(edit(cx, &f), Ok(("srv".into(), spec)));
}

#[gpui_kit::test]
fn only_postgres_and_sql_server_show_databases(cx: &mut TestAppContext) {
    let mut spec = pg();
    spec.databases = Some(Databases::All);
    let (win, f) = open(cx, Some(("srv", spec)));
    // MySQL, the third engine, takes no `databases`.
    cx.update_window(win, |_, w, cx| w.within("conn-engine").click(2usize, cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, win, "conn-databases"));
    let (_, my) = edit(cx, &f).unwrap();
    assert_eq!((my.engine, my.databases), (Engine::Mysql, None));
    cx.update_window(win, |_, w, cx| w.within("conn-engine").click(0usize, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        f.read_with(cx, |f, _| f.databases_choice()),
        DatabasesChoice::All,
        "back on the loaded engine, the loaded choice returns"
    );
}

fn toml(o: &support::Opened) -> String {
    fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap()
}

#[gpui_kit::test]
fn save_writes_databases_and_a_plain_save_leaves_the_file_alone(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    fill(cx, &o, Field::Database, "");
    choose(cx, o.win, DatabasesChoice::All);
    assert_eq!(value(cx, &f, Field::Database), "postgres");
    click_id(cx, o.win, "conn-save");
    assert!(!present(cx, o.win, "conn-name"), "a saved dialog closes");
    let saved = toml(&o);
    assert!(
        saved.contains("[connections.nopg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"postgres\"\nuser = \"x\"\ndatabases = \"all\"\n"),
        "{saved}"
    );
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    click_id(cx, o.win, "conn-save");
    assert_eq!(toml(&o), saved, "an untouched parent saves byte for byte");
}

#[gpui_kit::test]
fn a_parent_offers_no_database_suggestions(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("srv", pg())));
    listed(cx, &f, &["app", "billing", "postgres"]);
    assert!(present(cx, win, "conn-database-suggestions"));
    choose(cx, win, DatabasesChoice::All);
    assert!(
        !present(cx, win, "conn-database-suggestions"),
        "the databases are picked under Databases, not twice"
    );
    choose(cx, win, DatabasesChoice::Choose);
    assert!(!present(cx, win, "conn-database-suggestions"));
    assert!(present(cx, win, "conn-databases-items"));
    choose(cx, win, DatabasesChoice::Only);
    assert!(
        present(cx, win, "conn-database-suggestions"),
        "back to one database, Database offers the list again"
    );
}

#[gpui_kit::test]
fn a_test_waits_for_save_to_check_databases(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("srv", pg())));
    choose(cx, win, DatabasesChoice::Choose);
    set(cx, win, &f, Field::Database, "");
    assert!(edit(cx, &f).is_err(), "Save still refuses both");
    let (_, tested) = f.read_with(cx, |f, cx| f.test_edit(cx)).unwrap();
    assert_eq!(
        (tested.database, tested.databases),
        (None, None),
        "Test connects the connection itself, with its own empty-Database fallback"
    );
}

#[gpui_kit::test]
fn sql_server_shows_a_stored_name_on_the_servers_row_ignoring_case(cx: &mut TestAppContext) {
    let mut spec = ms();
    spec.databases = Some(names(&["Sales"]));
    let (win, f) = open(cx, Some(("ms", spec)));
    listed(cx, &f, &["sales", "billing"]);
    assert_eq!(
        f.read_with(cx, |f, _| f.database_items()),
        ["sales", "billing"],
        "one row, the server's"
    );
    assert!(!present(cx, win, ("conn-databases-gone", 0usize)));
    tick(cx, win, 1);
    assert_eq!(
        databases(cx, &f),
        Some(names(&["Sales", "billing"])),
        "the stored spelling stays when another row is ticked"
    );
    tick(cx, win, 0);
    assert_eq!(
        databases(cx, &f),
        Some(names(&["billing"])),
        "the server's row is the stored name's"
    );
}

#[gpui_kit::test]
fn postgres_keeps_names_that_differ_in_case_apart(cx: &mut TestAppContext) {
    let mut spec = pg();
    spec.databases = Some(names(&["Sales"]));
    let (win, f) = open(cx, Some(("srv", spec)));
    listed(cx, &f, &["sales"]);
    assert_eq!(
        f.read_with(cx, |f, _| f.database_items()),
        ["sales", "Sales"]
    );
    assert!(present(cx, win, ("conn-databases-gone", 1usize)));
}
