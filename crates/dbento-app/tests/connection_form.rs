mod support;

use dbento_app::connection_dialog::{ConnectionForm, Field};
use dbento_core::{
    AuthLevel, Color, ConnectionSpec, Engine, SecretSource, TlsMode, connection_form,
};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext};
use support::{label_of, present};

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
fn click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn radio(cx: &mut TestAppContext, win: AnyWindowHandle, group: &'static str, ix: usize) {
    cx.update_window(win, |_, w, cx| w.within(group).click(ix, cx))
        .unwrap();
    cx.run_until_parked();
}
fn edit(
    cx: &mut TestAppContext,
    f: &Entity<ConnectionForm>,
) -> Result<(String, ConnectionSpec), String> {
    f.read_with(cx, |f, cx| f.edit(cx))
}
fn nopg() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Postgres);
    (s.host, s.port, s.database, s.user) = (
        Some("127.0.0.1".into()),
        Some(9),
        Some("x".into()),
        Some("x".into()),
    );
    s
}

fn engine_option(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    ix: usize,
) -> (Option<String>, Option<bool>) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let o = w.within("conn-engine").find(ix);
        (o.label().map(str::to_owned), o.selected())
    })
    .unwrap()
}
fn value_and_hint(
    cx: &mut TestAppContext,
    f: &Entity<ConnectionForm>,
    field: Field,
) -> (String, String) {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    state.read_with(cx, |s, _| {
        (
            s.value().to_string(),
            s.presentation().placeholder().to_string(),
        )
    })
}

#[gpui_kit::test]
fn a_new_form_starts_as_postgres_with_every_key_absent(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert!(present(cx, win, "conn-host") && !present(cx, win, "conn-path"));
    set(cx, win, &f, Field::Name, "pg");
    set(cx, win, &f, Field::Host, "127.0.0.1");
    set(cx, win, &f, Field::Port, "9");
    set(cx, win, &f, Field::Database, "x");
    set(cx, win, &f, Field::User, "x");
    assert_eq!(edit(cx, &f), Ok(("pg".into(), nopg())));
}

#[gpui_kit::test]
fn the_engine_choice_switches_the_fields_and_clears_the_others(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    set(cx, win, &f, Field::Name, "lite");
    set(cx, win, &f, Field::Host, "h");
    click(cx, win, "conn-read-only");
    radio(cx, win, "conn-engine", 1);
    assert!(present(cx, win, "conn-path") && !present(cx, win, "conn-host"));
    set(cx, win, &f, Field::Path, "a.db");
    let mut want = ConnectionSpec::new(Engine::Sqlite);
    (want.path, want.read_only) = (Some("a.db".into()), Some(true));
    assert_eq!(edit(cx, &f), Ok(("lite".into(), want)));
}

#[gpui_kit::test]
fn an_edit_form_shows_the_keys_and_returns_them_unchanged(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    assert_eq!(
        f.read_with(cx, |f, _| f.previous().map(str::to_owned)),
        Some("nopg".into())
    );
    let port = f.read_with(cx, |f, _| f.input(Field::Port).clone());
    assert_eq!(port.read_with(cx, |s, _| s.value().to_string()), "9");
    let pw = f.read_with(cx, |f, _| f.input(Field::Password).clone());
    assert_eq!(
        pw.read_with(cx, |s, _| s.value().to_string()),
        "",
        "a password is never shown back"
    );
    assert!(present(cx, win, "conn-tls"));
    assert_eq!(
        edit(cx, &f),
        Ok(("nopg".into(), nopg())),
        "defaults stay absent"
    );
}

#[gpui_kit::test]
fn only_changed_controls_change_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    click(cx, win, "conn-read-only");
    click(cx, win, ("conn-color", 0usize));
    radio(cx, win, "conn-tls", 2);
    let mut want = nopg();
    (want.read_only, want.color, want.tls) = (Some(true), Some(Color::Red), Some(TlsMode::Off));
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want.clone())));
    click(cx, win, "conn-color-none");
    radio(cx, win, "conn-tls", 0);
    (want.color, want.tls) = (None, None);
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want)));
}

#[gpui_kit::test]
fn form_errors_and_the_error_line(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    set(cx, win, &f, Field::Port, "70000");
    assert_eq!(
        edit(cx, &f),
        Err("Port must be a number from 1 to 65535".into())
    );
    set(cx, win, &f, Field::Port, "");
    set(cx, win, &f, Field::Name, "");
    assert_eq!(edit(cx, &f), Err("Enter a name".into()));
    f.update(cx, |f, cx| f.set_error(Some("boom".into()), false, cx));
    cx.run_until_parked();
    assert_eq!(label_of(cx, win, "conn-error").as_deref(), Some("boom"));
    assert!(!present(cx, win, "conn-reload"));
    f.update(cx, |f, cx| f.set_error(Some("changed".into()), true, cx));
    cx.run_until_parked();
    assert!(present(cx, win, "conn-reload"));
}

#[gpui_kit::test]
fn an_untouched_value_with_surrounding_spaces_is_unchanged(cx: &mut TestAppContext) {
    let mut spaced = nopg();
    (spaced.host, spaced.secret) = (
        Some(" 127.0.0.1 ".into()),
        Some(SecretSource::Env(" PGPW ".into())),
    );
    let (_, f) = open(cx, Some(("nopg", spaced.clone())));
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), spaced)));
}

#[gpui_kit::test]
fn a_bad_folder_names_the_folder_once(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    set(cx, win, &f, Field::Folder, "a//b");
    assert_eq!(
        edit(cx, &f),
        Err("folder path `a//b` has an empty segment".into())
    );
}

#[gpui_kit::test]
fn switching_back_to_the_loaded_engine_restores_its_values(cx: &mut TestAppContext) {
    let mut loaded = nopg();
    (loaded.secret, loaded.tls) = (Some(SecretSource::Env("PGPW".into())), Some(TlsMode::Off));
    let (win, f) = open(cx, Some(("nopg", loaded.clone())));
    radio(cx, win, "conn-engine", 1);
    radio(cx, win, "conn-engine", 0);
    let host = f.read_with(cx, |f, _| f.input(Field::Host).clone());
    assert_eq!(
        host.read_with(cx, |s, _| s.value().to_string()),
        "127.0.0.1"
    );
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), loaded)));
}

#[gpui_kit::test]
fn a_port_with_a_plus_sign_is_refused(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    set(cx, win, &f, Field::Port, "+5");
    assert_eq!(
        edit(cx, &f),
        Err("Port must be a number from 1 to 65535".into())
    );
}

#[gpui_kit::test]
fn client_cert_fields_edit_their_keys_and_hide_when_tls_is_off(cx: &mut TestAppContext) {
    let mut loaded = nopg();
    loaded.tls = Some(TlsMode::Verify);
    let (win, f) = open(cx, Some(("nopg", loaded.clone())));
    for id in ["conn-tls-cert", "conn-tls-key", "conn-tls-key-passphrase"] {
        assert!(present(cx, win, id), "{id}");
    }
    set(cx, win, &f, Field::TlsCert, "certs/me.crt");
    set(cx, win, &f, Field::TlsKey, "certs/me.key");
    let mut want = loaded.clone();
    (want.tls_cert, want.tls_key) = (Some("certs/me.crt".into()), Some("certs/me.key".into()));
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want.clone())));
    assert!(f.read_with(cx, |f, cx| f.key_passphrase(cx)).is_none());
    set(cx, win, &f, Field::KeyPassphrase, "typed");
    let typed = f.read_with(cx, |f, cx| f.key_passphrase(cx));
    assert_eq!(
        typed.map(|s| s.expose().to_owned()).as_deref(),
        Some("typed")
    );
}

#[gpui_kit::test]
fn loaded_client_cert_keys_show_back_and_clear(cx: &mut TestAppContext) {
    let mut loaded = nopg();
    loaded.tls = Some(TlsMode::Verify);
    let mut want = loaded.clone();
    (want.tls_cert, want.tls_key) = (Some("certs/me.crt".into()), Some("certs/me.key".into()));
    let (win, f) = open(cx, Some(("nopg", want)));
    let cert = f.read_with(cx, |f, _| f.input(Field::TlsCert).clone());
    assert_eq!(
        cert.read_with(cx, |s, _| s.value().to_string()),
        "certs/me.crt"
    );
    set(cx, win, &f, Field::TlsCert, "");
    set(cx, win, &f, Field::TlsKey, "");
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), loaded)));
    radio(cx, win, "conn-tls", 2);
    assert!(!present(cx, win, "conn-tls-cert") && !present(cx, win, "conn-tls-key"));
}

#[gpui_kit::test]
fn the_ca_field_edits_tls_ca_and_hides_when_tls_is_off(cx: &mut TestAppContext) {
    let mut loaded = nopg();
    loaded.tls = Some(TlsMode::Verify);
    loaded.tls_ca = Some("certs/ca.pem".into());
    let (win, f) = open(cx, Some(("nopg", loaded.clone())));
    assert!(present(cx, win, "conn-tls-ca"));
    let ca = f.read_with(cx, |f, _| f.input(Field::TlsCa).clone());
    assert_eq!(
        ca.read_with(cx, |s, _| s.value().to_string()),
        "certs/ca.pem"
    );
    set(cx, win, &f, Field::TlsCa, "other/ca.pem");
    let mut want = loaded.clone();
    want.tls_ca = Some("other/ca.pem".into());
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want.clone())));
    set(cx, win, &f, Field::TlsCa, "");
    want.tls_ca = None;
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want)));
    radio(cx, win, "conn-tls", 2);
    assert!(!present(cx, win, "conn-tls-ca"));
}

#[gpui_kit::test]
fn the_third_engine_is_mysql_and_writes_only_its_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 2).0.as_deref(), Some("MySQL"));
    radio(cx, win, "conn-engine", 2);
    for id in [
        "conn-host",
        "conn-port",
        "conn-database",
        "conn-database-help",
        "conn-user",
        "conn-password",
        "conn-secret",
        "conn-tls",
        "conn-tls-ca",
        "conn-read-only",
        "conn-read-only-help",
        "conn-timeout",
        "conn-folder",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    assert!(!present(cx, win, "conn-path"));
    assert_eq!(value_and_hint(cx, &f, Field::Port).1, "3306");
    assert_eq!(value_and_hint(cx, &f, Field::User).1, "e.g. root");
    set(cx, win, &f, Field::Name, "my");
    set(cx, win, &f, Field::Host, "127.0.0.1");
    set(cx, win, &f, Field::User, "root");
    let mut want = ConnectionSpec::new(Engine::Mysql);
    (want.host, want.user) = (Some("127.0.0.1".into()), Some("root".into()));
    assert_eq!(edit(cx, &f), Ok(("my".into(), want)));
}

#[gpui_kit::test]
fn postgres_to_mysql_keeps_the_shared_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("nopg", nopg())));
    radio(cx, win, "conn-engine", 2);
    let mut want = nopg();
    want.engine = Engine::Mysql;
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), want)));
    assert_eq!(value_and_hint(cx, &f, Field::User).1, "e.g. root");
    radio(cx, win, "conn-engine", 0);
    // Back on the loaded engine: its values and its placeholders return.
    assert_eq!(
        value_and_hint(cx, &f, Field::Port),
        ("9".into(), "5432".into())
    );
    assert_eq!(value_and_hint(cx, &f, Field::User).1, "e.g. postgres");
    assert_eq!(edit(cx, &f), Ok(("nopg".into(), nopg())));
}

#[gpui_kit::test]
fn a_typed_default_port_follows_the_engine_and_another_port_stays(cx: &mut TestAppContext) {
    // Loaded as SQLite, so neither server engine takes set_engine's restore-the-loaded path.
    let mut lite = ConnectionSpec::new(Engine::Sqlite);
    lite.path = Some("a.db".into());
    let (win, f) = open(cx, Some(("lite", lite)));
    radio(cx, win, "conn-engine", 0);
    set(cx, win, &f, Field::Port, "5432");
    radio(cx, win, "conn-engine", 2);
    assert_eq!(
        value_and_hint(cx, &f, Field::Port),
        ("".into(), "3306".into())
    );
    set(cx, win, &f, Field::Port, "3307");
    radio(cx, win, "conn-engine", 0);
    assert_eq!(
        value_and_hint(cx, &f, Field::Port),
        ("3307".into(), "5432".into())
    );
}

#[gpui_kit::test]
fn a_loaded_mysql_connection_selects_mysql_and_returns_unchanged(cx: &mut TestAppContext) {
    let mut my = ConnectionSpec::new(Engine::Mysql);
    (my.host, my.user) = (Some("h".into()), Some("u".into()));
    let (win, f) = open(cx, Some(("my", my.clone())));
    assert_eq!(engine_option(cx, win, 2).1, Some(true));
    assert_eq!(engine_option(cx, win, 0).1, Some(false));
    assert!(present(cx, win, "conn-read-only-help"));
    assert_eq!(edit(cx, &f), Ok(("my".into(), my)));
}

#[gpui_kit::test]
fn only_mysql_shows_the_read_only_help(cx: &mut TestAppContext) {
    let (win, _f) = open(cx, Some(("nopg", nopg())));
    assert!(!present(cx, win, "conn-read-only-help"));
    assert!(!present(cx, win, "conn-database-help"));
    radio(cx, win, "conn-engine", 1);
    assert!(!present(cx, win, "conn-read-only-help"));
    assert!(present(cx, win, "conn-read-only"));
}

#[gpui_kit::test]
fn a_round_trip_through_mysql_keeps_what_was_typed_in_shared_fields(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    set(cx, win, &f, Field::Host, "typed");
    set(cx, win, &f, Field::Password, "pw");
    radio(cx, win, "conn-engine", 2);
    radio(cx, win, "conn-engine", 0);
    assert_eq!(value_and_hint(cx, &f, Field::Host).0, "typed");
    assert_eq!(value_and_hint(cx, &f, Field::Password).0, "pw");
}

#[gpui_kit::test]
fn a_loaded_default_port_cleared_on_the_way_out_comes_back(cx: &mut TestAppContext) {
    let mut pg = nopg();
    pg.port = Some(5432);
    let (win, f) = open(cx, Some(("pg", pg.clone())));
    radio(cx, win, "conn-engine", 2);
    assert_eq!(value_and_hint(cx, &f, Field::Port).0, "");
    radio(cx, win, "conn-engine", 0);
    assert_eq!(value_and_hint(cx, &f, Field::Port).0, "5432");
    assert_eq!(edit(cx, &f), Ok(("pg".into(), pg)));
}

#[gpui_kit::test]
fn the_fourth_engine_is_duckdb_and_writes_only_its_path(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 3).0.as_deref(), Some("DuckDB"));
    radio(cx, win, "conn-engine", 3);
    for id in [
        "conn-path",
        "conn-read-only",
        "conn-read-only-help",
        "conn-timeout",
        "conn-folder",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    for id in [
        "conn-host",
        "conn-port",
        "conn-user",
        "conn-password",
        "conn-tls",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    assert_eq!(
        label_of(cx, win, "conn-read-only-help").as_deref(),
        connection_form(Engine::Duckdb).read_only_help
    );
    assert_eq!(
        value_and_hint(cx, &f, Field::Path).1,
        "path/to/database.duckdb"
    );
    set(cx, win, &f, Field::Name, "duck");
    set(cx, win, &f, Field::Path, "data/app.duckdb");
    let mut want = ConnectionSpec::new(Engine::Duckdb);
    want.path = Some("data/app.duckdb".into());
    assert_eq!(edit(cx, &f), Ok(("duck".into(), want)));
}

#[gpui_kit::test]
fn sqlite_to_duckdb_and_back_keeps_the_path(cx: &mut TestAppContext) {
    let mut lite = ConnectionSpec::new(Engine::Sqlite);
    (lite.path, lite.read_only) = (Some("a.db".into()), Some(true));
    let (win, f) = open(cx, Some(("lite", lite.clone())));
    radio(cx, win, "conn-engine", 3);
    let mut want = lite.clone();
    want.engine = Engine::Duckdb;
    assert_eq!(edit(cx, &f), Ok(("lite".into(), want)));
    assert_eq!(
        value_and_hint(cx, &f, Field::Path),
        ("a.db".into(), "path/to/database.duckdb".into())
    );
    radio(cx, win, "conn-engine", 1);
    assert_eq!(edit(cx, &f), Ok(("lite".into(), lite)));
}

#[gpui_kit::test]
fn a_loaded_duckdb_connection_selects_duckdb_and_returns_unchanged(cx: &mut TestAppContext) {
    let mut duck = ConnectionSpec::new(Engine::Duckdb);
    duck.path = Some("data/app.duckdb".into());
    let (win, f) = open(cx, Some(("duck", duck.clone())));
    assert_eq!(engine_option(cx, win, 3).1, Some(true));
    assert_eq!(engine_option(cx, win, 1).1, Some(false));
    assert!(present(cx, win, "conn-read-only-help"));
    assert_eq!(edit(cx, &f), Ok(("duck".into(), duck)));
}

fn ms() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Mssql);
    (
        s.host,
        s.database,
        s.user,
        s.tls,
        s.tls_ca,
        s.read_only_user,
    ) = (
        Some("db.example".into()),
        Some("shop".into()),
        Some("sa".into()),
        Some(TlsMode::Verify),
        Some("certs/ca.pem".into()),
        Some("dbento_reader".into()),
    );
    s
}

#[gpui_kit::test]
fn a_loaded_sql_server_connection_shows_its_sections_and_returns_unchanged(
    cx: &mut TestAppContext,
) {
    let (win, f) = open(cx, Some(("ms", ms())));
    for id in [
        "conn-host",
        "conn-port",
        "conn-database",
        "conn-database-help",
        "conn-user",
        "conn-password",
        "conn-secret",
        "conn-tls",
        "conn-tls-help",
        "conn-tls-ca",
        "conn-read-only-user",
        "conn-read-only-user-help",
        "conn-read-only",
        "conn-read-only-help",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    for id in [
        "conn-path",
        "conn-tls-cert",
        "conn-tls-key",
        "conn-tls-key-passphrase",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    let form = connection_form(Engine::Mssql);
    assert_eq!(
        label_of(cx, win, "conn-read-only-user-help").as_deref(),
        form.sections[3].fields[0].help
    );
    assert_eq!(
        label_of(cx, win, "conn-read-only-help").as_deref(),
        form.read_only_help
    );
    assert_eq!(
        value_and_hint(cx, &f, Field::ReadOnlyUser),
        ("dbento_reader".into(), "e.g. dbento_reader".into())
    );
    assert_eq!(value_and_hint(cx, &f, Field::Port).1, "1433");
    assert_eq!(value_and_hint(cx, &f, Field::User).1, "e.g. sa");
    assert_eq!(edit(cx, &f), Ok(("ms".into(), ms())));
}

#[gpui_kit::test]
fn the_reader_user_edits_read_only_user_and_clears(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    set(cx, win, &f, Field::ReadOnlyUser, "other_reader");
    let mut want = ms();
    want.read_only_user = Some("other_reader".into());
    assert_eq!(edit(cx, &f), Ok(("ms".into(), want.clone())));
    set(cx, win, &f, Field::ReadOnlyUser, "");
    want.read_only_user = None;
    assert_eq!(edit(cx, &f), Ok(("ms".into(), want)));
}

#[gpui_kit::test]
fn a_reload_moves_an_untouched_reader_user(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    let mut now = ms();
    now.read_only_user = Some("other_reader".into());
    cx.update_window(win, |_, w, cx| {
        f.update(cx, |f, cx| f.rebase(Some(now.clone()), Vec::new(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        value_and_hint(cx, &f, Field::ReadOnlyUser).0,
        "other_reader"
    );
    assert_eq!(edit(cx, &f), Ok(("ms".into(), now)));
}

#[gpui_kit::test]
fn leaving_sql_server_drops_its_reader_user(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    radio(cx, win, "conn-engine", 2);
    assert!(!present(cx, win, "conn-read-only-user"));
    assert!(
        present(cx, win, "conn-tls-cert"),
        "MySQL takes a client cert"
    );
    let mut want = ms();
    (want.engine, want.read_only_user) = (Engine::Mysql, None);
    assert_eq!(edit(cx, &f), Ok(("ms".into(), want)));
}

#[gpui_kit::test]
fn the_fifth_engine_is_sql_server_and_writes_only_its_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 4).0.as_deref(), Some("SQL Server"));
    radio(cx, win, "conn-engine", 4);
    assert!(present(cx, win, "conn-read-only-user"));
    assert_eq!(value_and_hint(cx, &f, Field::Port).1, "1433");
    assert_eq!(value_and_hint(cx, &f, Field::Database).1, "optional");
    set(cx, win, &f, Field::Name, "ms");
    set(cx, win, &f, Field::Host, "db.example");
    set(cx, win, &f, Field::User, "sa");
    set(cx, win, &f, Field::ReadOnlyUser, "dbento_reader");
    let mut want = ConnectionSpec::new(Engine::Mssql);
    (want.host, want.user, want.read_only_user) = (
        Some("db.example".into()),
        Some("sa".into()),
        Some("dbento_reader".into()),
    );
    assert_eq!(edit(cx, &f), Ok(("ms".into(), want)));
}

#[gpui_kit::test]
fn a_loaded_sql_server_connection_selects_sql_server(cx: &mut TestAppContext) {
    let (win, _f) = open(cx, Some(("ms", ms())));
    assert_eq!(engine_option(cx, win, 4).1, Some(true));
    assert_eq!(engine_option(cx, win, 0).1, Some(false));
}

#[gpui_kit::test]
fn sql_server_to_mysql_and_back_restores_the_reader_user(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("ms", ms())));
    radio(cx, win, "conn-engine", 2);
    radio(cx, win, "conn-engine", 4);
    assert_eq!(
        value_and_hint(cx, &f, Field::ReadOnlyUser).0,
        "dbento_reader"
    );
    assert_eq!(edit(cx, &f), Ok(("ms".into(), ms())));
}

#[gpui_kit::test]
fn a_client_certificate_stays_with_postgres_when_switching_to_sql_server(cx: &mut TestAppContext) {
    let mut pg = nopg();
    (pg.tls, pg.tls_cert, pg.tls_key) = (
        Some(TlsMode::Verify),
        Some("certs/me.crt".into()),
        Some("certs/me.key".into()),
    );
    let (win, f) = open(cx, Some(("pg", pg.clone())));
    set(cx, win, &f, Field::KeyPassphrase, "typed");
    radio(cx, win, "conn-engine", 4);
    for id in ["conn-tls-cert", "conn-tls-key", "conn-tls-key-passphrase"] {
        assert!(!present(cx, win, id), "{id}");
    }
    assert!(present(cx, win, "conn-tls-ca"));
    assert!(f.read_with(cx, |f, cx| f.key_passphrase(cx)).is_none());
    let mut want = pg.clone();
    (want.engine, want.tls_cert, want.tls_key) = (Engine::Mssql, None, None);
    assert_eq!(edit(cx, &f), Ok(("pg".into(), want)));
    radio(cx, win, "conn-engine", 0);
    assert_eq!(edit(cx, &f), Ok(("pg".into(), pg)));
    let typed = f.read_with(cx, |f, cx| f.key_passphrase(cx));
    assert_eq!(
        typed.map(|s| s.expose().to_owned()).as_deref(),
        Some("typed")
    );
}

fn sr() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Surrealdb);
    (s.host, s.namespace, s.database, s.user, s.auth_level) = (
        Some("db.example".into()),
        Some("app".into()),
        Some("main".into()),
        Some("reader".into()),
        Some(AuthLevel::Database),
    );
    (s.tls, s.tls_ca) = (Some(TlsMode::Verify), Some("certs/ca.pem".into()));
    s
}

#[gpui_kit::test]
fn a_loaded_surrealdb_connection_shows_its_fields_and_returns_unchanged(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("sr", sr())));
    for id in [
        "conn-host",
        "conn-port",
        "conn-namespace",
        "conn-database",
        "conn-user",
        "conn-user-help",
        "conn-auth-level",
        "conn-auth-level-help",
        "conn-password",
        "conn-secret",
        "conn-tls",
        "conn-tls-ca",
        "conn-read-only",
        "conn-read-only-help",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    for id in [
        "conn-path",
        "conn-read-only-user",
        "conn-tls-cert",
        "conn-tls-key",
        "conn-tls-key-passphrase",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    let form = connection_form(Engine::Surrealdb);
    assert_eq!(
        label_of(cx, win, "conn-auth-level-help").as_deref(),
        form.sections[0].fields[5].help
    );
    assert_eq!(
        label_of(cx, win, "conn-user-help").as_deref(),
        form.sections[0].fields[4].help
    );
    assert_eq!(
        value_and_hint(cx, &f, Field::Namespace),
        ("app".into(), "e.g. app".into())
    );
    assert_eq!(
        value_and_hint(cx, &f, Field::AuthLevel),
        ("database".into(), "root, namespace or database".into())
    );
    assert_eq!(value_and_hint(cx, &f, Field::Port).1, "8000");
    assert_eq!(value_and_hint(cx, &f, Field::User).1, "optional");
    assert_eq!(edit(cx, &f), Ok(("sr".into(), sr())));
}

#[gpui_kit::test]
fn namespace_and_auth_level_edit_and_clear(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("sr", sr())));
    set(cx, win, &f, Field::Namespace, "other");
    set(cx, win, &f, Field::AuthLevel, " Root ");
    let mut want = sr();
    (want.namespace, want.auth_level) = (Some("other".into()), Some(AuthLevel::Root));
    assert_eq!(edit(cx, &f), Ok(("sr".into(), want.clone())));
    set(cx, win, &f, Field::AuthLevel, "");
    want.auth_level = None;
    assert_eq!(edit(cx, &f), Ok(("sr".into(), want)));
    set(cx, win, &f, Field::AuthLevel, "admin");
    assert_eq!(
        edit(cx, &f),
        Err("Auth level must be root, namespace or database".into())
    );
}

#[gpui_kit::test]
fn a_reload_moves_an_untouched_namespace_and_auth_level(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("sr", sr())));
    let mut now = sr();
    (now.namespace, now.auth_level) = (Some("moved".into()), Some(AuthLevel::Namespace));
    cx.update_window(win, |_, w, cx| {
        f.update(cx, |f, cx| f.rebase(Some(now.clone()), Vec::new(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(value_and_hint(cx, &f, Field::Namespace).0, "moved");
    assert_eq!(value_and_hint(cx, &f, Field::AuthLevel).0, "namespace");
    assert_eq!(edit(cx, &f), Ok(("sr".into(), now)));
}

#[gpui_kit::test]
fn leaving_surrealdb_drops_its_namespace_and_auth_level(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("sr", sr())));
    radio(cx, win, "conn-engine", 0);
    assert!(!present(cx, win, "conn-namespace"));
    assert!(!present(cx, win, "conn-auth-level"));
    let mut want = sr();
    (want.engine, want.namespace, want.auth_level) = (Engine::Postgres, None, None);
    assert_eq!(edit(cx, &f), Ok(("sr".into(), want)));
}

#[gpui_kit::test]
fn the_sixth_engine_is_surrealdb_and_writes_only_its_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 5).0.as_deref(), Some("SurrealDB"));
    radio(cx, win, "conn-engine", 5);
    assert!(present(cx, win, "conn-namespace"));
    assert_eq!(value_and_hint(cx, &f, Field::Port).1, "8000");
    set(cx, win, &f, Field::Name, "sr");
    set(cx, win, &f, Field::Host, "db.example");
    set(cx, win, &f, Field::Namespace, "app");
    set(cx, win, &f, Field::Database, "main");
    let mut want = ConnectionSpec::new(Engine::Surrealdb);
    (want.host, want.namespace, want.database) = (
        Some("db.example".into()),
        Some("app".into()),
        Some("main".into()),
    );
    assert_eq!(edit(cx, &f), Ok(("sr".into(), want)));
}

#[gpui_kit::test]
fn a_loaded_surrealdb_connection_selects_surrealdb(cx: &mut TestAppContext) {
    let (win, _f) = open(cx, Some(("sr", sr())));
    assert_eq!(engine_option(cx, win, 5).1, Some(true));
    assert_eq!(engine_option(cx, win, 0).1, Some(false));
}

#[gpui_kit::test]
fn surrealdb_to_postgres_and_back_restores_namespace_and_auth_level(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("sr", sr())));
    radio(cx, win, "conn-engine", 0);
    radio(cx, win, "conn-engine", 5);
    assert_eq!(value_and_hint(cx, &f, Field::Namespace).0, "app");
    assert_eq!(value_and_hint(cx, &f, Field::AuthLevel).0, "database");
    assert_eq!(edit(cx, &f), Ok(("sr".into(), sr())));
}

#[gpui_kit::test]
fn a_client_certificate_stays_with_postgres_when_switching_to_surrealdb(cx: &mut TestAppContext) {
    let mut pg = nopg();
    (pg.tls, pg.tls_cert, pg.tls_key) = (
        Some(TlsMode::Verify),
        Some("certs/me.crt".into()),
        Some("certs/me.key".into()),
    );
    let (win, f) = open(cx, Some(("pg", pg.clone())));
    radio(cx, win, "conn-engine", 5);
    for id in ["conn-tls-cert", "conn-tls-key", "conn-tls-key-passphrase"] {
        assert!(!present(cx, win, id), "{id}");
    }
    let mut want = pg.clone();
    (want.engine, want.tls_cert, want.tls_key) = (Engine::Surrealdb, None, None);
    assert_eq!(edit(cx, &f), Ok(("pg".into(), want)));
    radio(cx, win, "conn-engine", 0);
    assert_eq!(edit(cx, &f), Ok(("pg".into(), pg)));
}

const MG_URI: &str = "mongodb://app@db.example:27017/shop?authSource=admin";
fn mg() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Mongodb);
    s.uri = Some(MG_URI.into());
    s
}

#[gpui_kit::test]
fn the_seventh_engine_is_mongodb_and_writes_only_its_uri(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 6).0.as_deref(), Some("MongoDB"));
    radio(cx, win, "conn-engine", 6);
    assert!(present(cx, win, "conn-uri"));
    for id in [
        "conn-host",
        "conn-port",
        "conn-database",
        "conn-user",
        "conn-path",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    set(cx, win, &f, Field::Name, "mg");
    set(cx, win, &f, Field::Uri, MG_URI);
    assert_eq!(edit(cx, &f), Ok(("mg".into(), mg())));
}

#[gpui_kit::test]
fn a_loaded_mongodb_connection_selects_mongodb_and_shows_its_uri(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("mg", mg())));
    assert_eq!(engine_option(cx, win, 6).1, Some(true));
    for ix in 0..6 {
        assert_eq!(engine_option(cx, win, ix).1, Some(false), "{ix}");
    }
    assert_eq!(value_and_hint(cx, &f, Field::Uri).0, MG_URI);
    assert_eq!(edit(cx, &f), Ok(("mg".into(), mg())));
}

#[gpui_kit::test]
fn leaving_mongodb_drops_its_uri_and_coming_back_restores_it(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("mg", mg())));
    radio(cx, win, "conn-engine", 0);
    let (_, pg) = edit(cx, &f).unwrap();
    assert_eq!((pg.engine, pg.uri), (Engine::Postgres, None));
    radio(cx, win, "conn-engine", 6);
    assert_eq!(edit(cx, &f), Ok(("mg".into(), mg())));
}

fn rd() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Redis);
    (s.host, s.port, s.database, s.user) = (
        Some("cache.internal".into()),
        Some(6380),
        Some("2".into()),
        Some("dbento_ro".into()),
    );
    s
}

#[gpui_kit::test]
fn the_eighth_engine_is_redis_with_host_port_db_index_and_user(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 7).0.as_deref(), Some("Redis"));
    radio(cx, win, "conn-engine", 7);
    for id in [
        "conn-host",
        "conn-port",
        "conn-database",
        "conn-user",
        "conn-tls",
        "conn-read-only-help",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    for id in [
        "conn-path",
        "conn-uri",
        "conn-namespace",
        "conn-read-only-user",
        "conn-auth-level",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    set(cx, win, &f, Field::Name, "cache");
    set(cx, win, &f, Field::Host, "cache.internal");
    set(cx, win, &f, Field::Port, "6380");
    set(cx, win, &f, Field::Database, "2");
    set(cx, win, &f, Field::User, "dbento_ro");
    assert_eq!(edit(cx, &f), Ok(("cache".into(), rd())));
}

#[gpui_kit::test]
fn a_loaded_redis_connection_selects_redis_and_returns_unchanged(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("cache", rd())));
    assert_eq!(engine_option(cx, win, 7).1, Some(true));
    for ix in 0..7 {
        assert_eq!(engine_option(cx, win, ix).1, Some(false), "{ix}");
    }
    assert_eq!(edit(cx, &f), Ok(("cache".into(), rd())));
}

#[gpui_kit::test]
fn leaving_redis_for_mongodb_drops_its_keys_and_coming_back_restores_them(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("cache", rd())));
    radio(cx, win, "conn-engine", 6);
    set(cx, win, &f, Field::Uri, MG_URI);
    let (_, mg) = edit(cx, &f).unwrap();
    assert_eq!(
        (mg.engine, mg.host, mg.port, mg.user),
        (Engine::Mongodb, None, None, None)
    );
    radio(cx, win, "conn-engine", 7);
    assert_eq!(edit(cx, &f), Ok(("cache".into(), rd())));
}
