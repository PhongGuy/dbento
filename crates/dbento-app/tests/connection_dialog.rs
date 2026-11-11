use crate::support;
use dbento_app::explorer::{connection_id, part_id};

use crate::support::{click_id, fill, form, label_of, opened, present};
use dbento_app::connection_dialog::{Field, TestState};
use gpui_kit::px;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::fs;

fn toml(o: &support::Opened) -> String {
    fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap()
}

#[gpui_kit::test]
fn new_sqlite_connection_is_written_and_shown(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = toml(&o);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-shift-n", cx))
        .unwrap();
    cx.run_until_parked();
    form(cx, &o);
    cx.update_window(o.win, |_, w, cx| w.within("conn-engine").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    fill(cx, &o, Field::Name, "notes");
    fill(cx, &o, Field::Path, "data/app.db");
    click_id(cx, o.win, ("conn-color", 3usize));
    click_id(cx, o.win, "conn-save");
    assert!(!present(cx, o.win, "conn-name"), "a saved dialog closes");
    assert_eq!(
        toml(&o),
        format!(
            "{before}\n[connections.notes]\nengine = \"sqlite\"\npath = \"data/app.db\"\ncolor = \"green\"\n"
        )
    );
    assert_eq!(
        label_of(cx, o.win, connection_id("notes")).as_deref(),
        Some("notes"),
        "lite, nopg, notes"
    );
}

#[gpui_kit::test]
fn the_edit_button_opens_the_form_without_a_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    // A double click pins the button's stop_propagation: without it the row would open nopg.
    support::hover(cx, o.win, connection_id("nopg"));
    cx.update_window(o.win, |_, w, cx| {
        w.double_click(part_id("edit-connection", "nopg"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    let f = form(cx, &o);
    assert_eq!(
        f.read_with(cx, |f, _| f.previous().map(str::to_owned)),
        Some("nopg".into())
    );
    assert_eq!(
        o.shell.read_with(cx, |s, _| (s.tabs().len(), s.active())),
        (1, Some(0)),
        "no tab opens"
    );
    let before = toml(&o);
    click_id(cx, o.win, "conn-save");
    assert_eq!(toml(&o), before, "an unchanged form writes nothing new");
    support::gear(cx, o.win, "nopg");
    fill(cx, &o, Field::Database, "y");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        toml(&o),
        before.replace("database = \"x\"", "database = \"y\""),
        "only the changed key moves"
    );
}

#[gpui_kit::test]
fn plan_errors_show_inline_and_keep_the_dialog_open(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = toml(&o);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    fill(cx, &o, Field::Name, "lite");
    fill(cx, &o, Field::Host, "h");
    fill(cx, &o, Field::Database, "d");
    fill(cx, &o, Field::User, "u");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("a connection named `lite` already exists")
    );
    fill(cx, &o, Field::Name, "pg");
    fill(cx, &o, Field::Timeout, "soon");
    click_id(cx, o.win, "conn-save");
    let msg = label_of(cx, o.win, "conn-error").unwrap();
    assert!(msg.contains("statement_timeout"), "{msg}");
    assert_eq!(toml(&o), before, "nothing is written");
    click_id(cx, o.win, "conn-cancel");
    assert!(!present(cx, o.win, "conn-name"));
}

#[gpui_kit::test]
fn a_changed_file_offers_reload(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "lite");
    form(cx, &o);
    let file = o.fx.dir.path().join("dbento.toml");
    let edited = format!("{}# by hand\n", toml(&o));
    fs::write(&file, &edited).unwrap();
    click_id(cx, o.win, ("conn-color", 0usize));
    click_id(cx, o.win, "conn-save");
    let msg = label_of(cx, o.win, "conn-error").unwrap();
    assert!(msg.contains("changed since it was loaded"), "{msg}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        edited,
        "no silent overwrite"
    );
    click_id(cx, o.win, "conn-reload");
    assert!(o.shell.read_with(cx, |s, _| {
        s.workspace()
            .unwrap()
            .source_text()
            .ends_with("# by hand\n")
    }));
    click_id(cx, o.win, "conn-save");
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .contains("color = \"red\""),
        "the redo lands"
    );
}

#[gpui_kit::test]
fn the_folder_field_suggests_existing_folders_and_takes_new_ones(cx: &mut TestAppContext) {
    let o = opened(cx, "", "folder = \"team/core\"", 0);
    support::gear(cx, o.win, "nopg");
    let folder = form(cx, &o).read_with(cx, |f, _| f.input(Field::Folder).clone());
    cx.update_window(o.win, |_, w, cx| {
        folder.update(cx, |s, cx| s.focus(w, cx));
        w.input("te", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, ("conn-folder-suggestion", 0usize)).as_deref(),
        Some("team")
    );
    assert_eq!(
        label_of(cx, o.win, ("conn-folder-suggestion", 1usize)).as_deref(),
        Some("team/core")
    );
    click_id(cx, o.win, ("conn-folder-suggestion", 1usize));
    assert_eq!(
        folder.read_with(cx, |s, _| s.value().to_string()),
        "team/core"
    );
    fill(cx, &o, Field::Folder, "ops/new");
    click_id(cx, o.win, "conn-save");
    assert!(toml(&o).contains("folder = \"ops/new\""));
}

#[gpui_kit::test]
fn a_tests_databases_are_offered_under_database_until_the_server_changes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    fill(cx, &o, Field::Database, "");
    let listed = ["app", "app_test", "billing", "postgres"];
    f.update(cx, |f, cx| {
        f.set_test(TestState::Connected, cx);
        f.set_databases(listed.map(str::to_owned).to_vec(), cx);
    });
    cx.run_until_parked();
    for (ix, name) in listed.iter().enumerate() {
        assert_eq!(
            label_of(cx, o.win, ("conn-database-suggestion", ix)).as_deref(),
            Some(*name),
            "an empty Database offers every listed database"
        );
    }
    let database = f.read_with(cx, |f, _| f.input(Field::Database).clone());
    cx.update_window(o.win, |_, w, cx| {
        database.update(cx, |s, cx| s.focus(w, cx));
        w.input("ap", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        f.read_with(cx, |f, _| f.database_suggestions().to_vec()),
        ["app", "app_test"],
        "typing narrows the list"
    );
    f.update(cx, |f, cx| f.set_test(TestState::Connected, cx));
    cx.run_until_parked();
    click_id(cx, o.win, ("conn-database-suggestion", 1usize));
    assert_eq!(
        database.read_with(cx, |s, _| s.value().to_string()),
        "app_test"
    );
    assert!(
        !present(cx, o.win, ("conn-database-suggestion", 0usize)),
        "a pick closes the list"
    );
    assert!(
        !present(cx, o.win, "conn-test-result"),
        "the Test answer was for the database named before the pick"
    );
    // The list belongs to the server, so editing Database offers it again.
    cx.update_window(o.win, |_, w, cx| {
        database.update(cx, |s, cx| s.focus(w, cx));
        for _ in 0.."_test".len() {
            w.press("backspace", cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        f.read_with(cx, |f, _| f.database_suggestions().to_vec()),
        ["app_test"]
    );
    // Another host may hold other databases, so the list goes until the next Test.
    let host = f.read_with(cx, |f, _| f.input(Field::Host).clone());
    cx.update_window(o.win, |_, w, cx| {
        host.update(cx, |s, cx| s.focus(w, cx));
        w.input("x", cx);
        database.update(cx, |s, cx| s.focus(w, cx));
        w.press("backspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(f.read_with(cx, |f, _| f.database_suggestions().is_empty()));
    assert!(!present(cx, o.win, ("conn-database-suggestion", 0usize)));
}

#[gpui_kit::test]
fn the_file_picker_fills_the_path(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "lite");
    form(cx, &o);
    click_id(cx, o.win, "conn-path-pick");
    assert!(cx.did_prompt_for_paths());
    let picked = o.fx.ws.dir().join("data/other.db");
    cx.simulate_path_prompt_response(move |_| Some(vec![picked.clone()]));
    cx.run_until_parked();
    let path = form(cx, &o).read_with(cx, |f, _| f.input(Field::Path).clone());
    assert_eq!(
        path.read_with(cx, |s, _| s.value().to_string()),
        "data/other.db",
        "relative to the workspace"
    );
}

#[gpui_kit::test]
fn escape_closes_the_dialog_and_a_new_one_opens(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "conn-name"), "Escape closes the dialog");
    assert!(o.shell.read_with(cx, |s, _| s.connection_form().is_none()));
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    assert_eq!(
        f.read_with(cx, |f, _| f.previous().map(str::to_owned)),
        Some("nopg".into())
    );
}

// Saves after a hand edit of nopg's database and a Reload in the dialog; `form_db` also
// changes the database in the form first.
fn save_after_reload(cx: &mut TestAppContext, form_db: Option<&str>) -> String {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    if let Some(db) = form_db {
        fill(cx, &o, Field::Database, db);
    }
    let file = o.fx.dir.path().join("dbento.toml");
    fs::write(
        &file,
        toml(&o).replace("database = \"x\"", "database = \"hand\""),
    )
    .unwrap();
    fill(cx, &o, Field::User, "me");
    click_id(cx, o.win, "conn-save");
    click_id(cx, o.win, "conn-reload");
    click_id(cx, o.win, "conn-save");
    assert!(
        !present(cx, o.win, "conn-name"),
        "the redo saves and closes"
    );
    toml(&o)
}

#[gpui_kit::test]
fn reload_keeps_a_hand_edit_the_form_did_not_touch(cx: &mut TestAppContext) {
    let text = save_after_reload(cx, None);
    assert!(text.contains("database = \"hand\""), "{text}");
    assert!(text.contains("user = \"me\""), "{text}");
}

#[gpui_kit::test]
fn reload_keeps_the_users_own_change(cx: &mut TestAppContext) {
    let text = save_after_reload(cx, Some("mine"));
    assert!(text.contains("database = \"mine\""), "{text}");
    assert!(text.contains("user = \"me\""), "{text}");
}

#[gpui_kit::test]
fn reload_after_the_connection_is_gone_keeps_the_dialog(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    let file = o.fx.dir.path().join("dbento.toml");
    let cut = toml(&o);
    let cut = &cut[..cut.find("[connections.nopg]").unwrap()];
    fs::write(&file, cut).unwrap();
    click_id(cx, o.win, ("conn-color", 0usize));
    click_id(cx, o.win, "conn-save");
    click_id(cx, o.win, "conn-reload");
    let msg = label_of(cx, o.win, "conn-error").unwrap();
    assert!(msg.contains("nopg"), "{msg}");
    assert!(present(cx, o.win, "conn-name"));
}

#[gpui_kit::test]
fn a_finished_test_and_an_inline_error_clear_when_a_field_changes(cx: &mut TestAppContext) {
    use dbento_app::connection_dialog::TestState;
    let o = opened(cx, "", "", 0);
    // A double click pins the button's stop_propagation: without it the row would open nopg.
    support::hover(cx, o.win, connection_id("nopg"));
    cx.update_window(o.win, |_, w, cx| {
        w.double_click(part_id("edit-connection", "nopg"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    let f = form(cx, &o);
    f.update(cx, |f, cx| {
        f.set_test(TestState::Connected, cx);
        f.set_error(Some("Port must be a number".into()), false, cx);
    });
    assert!(present(cx, o.win, "conn-test-result"));
    let host = f.read_with(cx, |f, _| f.input(Field::Host).clone());
    cx.update_window(o.win, |_, w, cx| {
        host.update(cx, |s, cx| s.focus(w, cx));
        w.input("x", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        !present(cx, o.win, "conn-test-result"),
        "a changed host drops Connected"
    );
    assert!(!present(cx, o.win, "conn-error"), "and the inline error");
    // A stale-file error stays until Reload; only the test result goes.
    f.update(cx, |f, cx| {
        f.set_test(TestState::Connected, cx);
        f.set_error(Some("stale".into()), true, cx);
    });
    cx.update_window(o.win, |_, w, cx| w.within("conn-engine").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "conn-test-result"));
    assert_eq!(label_of(cx, o.win, "conn-error").as_deref(), Some("stale"));
}

#[gpui_kit::test]
fn a_reload_drops_a_running_test(cx: &mut TestAppContext) {
    use dbento_app::connection_dialog::TestState;
    // A listener that never answers keeps the test connecting until it is dropped.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let o = opened(cx, "", "", 0);
    let path = o.fx.dir.path().join("dbento.toml");
    fs::write(
        &path,
        toml(&o).replace("port = 9", &format!("port = {port}")),
    )
    .unwrap();
    support::reload(cx, o.win);
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    click_id(cx, o.win, "conn-test");
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Testing
    );
    fs::write(&path, format!("{}# by hand\n", toml(&o))).unwrap();
    click_id(cx, o.win, "conn-save");
    click_id(cx, o.win, "conn-reload");
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Idle
    );
    drop(listener);
    let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Idle
    );
    assert!(!present(cx, o.win, "conn-test-result"));
}

#[gpui_kit::test]
fn typing_during_a_test_drops_its_result(cx: &mut TestAppContext) {
    use dbento_app::connection_dialog::TestState;
    // A listener that never answers keeps the test connecting until it is dropped.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let o = opened(cx, "", "", 0);
    let path = o.fx.dir.path().join("dbento.toml");
    fs::write(
        &path,
        toml(&o).replace("port = 9", &format!("port = {port}")),
    )
    .unwrap();
    support::reload(cx, o.win);
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    click_id(cx, o.win, "conn-test");
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Testing
    );
    let host = f.read_with(cx, |f, _| f.input(Field::Host).clone());
    cx.update_window(o.win, |_, w, cx| {
        host.update(cx, |s, cx| s.focus(w, cx));
        w.input("x", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Idle
    );
    // Closing the listener ends the old attempt; its answer must not land.
    drop(listener);
    let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        f.read_with(cx, |f, _| f.test_state().clone()),
        TestState::Idle
    );
    assert!(!present(cx, o.win, "conn-test-result"));
}

#[gpui_kit::test]
fn the_dialog_is_560_px_with_32_px_inputs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1600.), px(900.)));
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    let (dialog, name, host) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("conn-dialog").bounds(),
                w.find("conn-name").bounds(),
                w.find("conn-host").bounds(),
            )
        })
        .unwrap();
    assert!(
        (f32::from(dialog.size.width) - 560.).abs() <= 2.,
        "{dialog:?}"
    );
    assert_eq!(name.size.height, px(32.));
    assert_eq!(host.size.height, px(32.));
}

#[gpui_kit::test]
fn ctrl_s_in_the_form_saves_the_connection_not_a_query(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = toml(&o);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    cx.update_window(o.win, |_, w, cx| w.within("conn-engine").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    fill(cx, &o, Field::Name, "notes");
    fill(cx, &o, Field::Path, "data/app.db");
    let name = form(cx, &o).read_with(cx, |f, _| f.input(Field::Name).clone());
    cx.update_window(o.win, |_, w, cx| {
        name.update(cx, |s, cx| s.focus(w, cx));
        w.press("ctrl-s", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "save-name"), "no query is saved");
    assert!(!present(cx, o.win, "conn-name"), "a saved dialog closes");
    assert_eq!(
        toml(&o),
        format!("{before}\n[connections.notes]\nengine = \"sqlite\"\npath = \"data/app.db\"\n")
    );
}

#[gpui_kit::test]
fn ctrl_s_with_focus_on_the_dialog_frame_saves_the_connection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = toml(&o);
    support::gear(cx, o.win, "nopg");
    fill(cx, &o, Field::Database, "y");
    // Nothing in the form was clicked: focus is where the dialog put it on open.
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-s", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "save-name"), "no query is saved");
    assert!(!present(cx, o.win, "conn-name"), "a saved dialog closes");
    assert_eq!(
        toml(&o),
        before.replace("database = \"x\"", "database = \"y\"")
    );
}

#[gpui_kit::test]
fn a_long_test_failure_wraps_instead_of_truncating(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1600.), px(900.)));
    support::gear(cx, o.win, "nopg");
    let f = form(cx, &o);
    let why = "error connecting to server: Connection refused (os error 111) while \
               resolving the host and opening a TCP connection to port 5432";
    f.update(cx, |f, cx| f.set_test(TestState::Failed(why.into()), cx));
    cx.run_until_parked();
    let (result, name) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("conn-test-result").bounds(),
                w.find("conn-name").bounds(),
            )
        })
        .unwrap();
    assert!(
        result.size.height > px(30.),
        "the failure wraps onto several lines: {result:?}"
    );
    assert!(
        result.size.width > px(400.),
        "the failure spans the form: {result:?}"
    );
    assert!(result.origin.y > name.origin.y);
}

#[gpui_kit::test]
fn the_tls_file_pickers_fill_their_fields(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    let outside = tempfile::tempdir().unwrap();
    for (pick, field, picked, shown) in [
        (
            "conn-tls-cert-pick",
            Field::TlsCert,
            o.fx.ws.dir().join("certs/me.crt"),
            "certs/me.crt".to_owned(),
        ),
        (
            "conn-tls-key-pick",
            Field::TlsKey,
            o.fx.ws.dir().join("certs/me.key"),
            "certs/me.key".to_owned(),
        ),
        (
            "conn-tls-ca-pick",
            Field::TlsCa,
            outside.path().join("ca.pem"),
            outside.path().join("ca.pem").to_string_lossy().into_owned(),
        ),
    ] {
        click_id(cx, o.win, pick);
        assert!(cx.did_prompt_for_paths(), "{pick}");
        cx.simulate_path_prompt_response(move |_| Some(vec![picked.clone()]));
        cx.run_until_parked();
        let input = form(cx, &o).read_with(cx, |f, _| f.input(field).clone());
        assert_eq!(
            input.read_with(cx, |s, _| s.value().to_string()),
            shown,
            "{pick}"
        );
    }
}

#[gpui_kit::test]
fn new_duckdb_connection_is_written_and_the_engine_choice_fits(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = toml(&o);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-shift-n", cx))
        .unwrap();
    cx.run_until_parked();
    form(cx, &o);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let name = w.find("conn-name").bounds();
        for ix in 0..9usize {
            let option = w.within("conn-engine").find(ix).bounds();
            assert!(
                option.right() <= name.right(),
                "{ix}: {option:?} overflows {name:?}"
            );
            assert!(option.size.width > px(0.), "{ix}: {option:?} is hidden");
        }
    })
    .unwrap();
    cx.update_window(o.win, |_, w, cx| w.within("conn-engine").click(3usize, cx))
        .unwrap();
    cx.run_until_parked();
    fill(cx, &o, Field::Name, "ducks");
    fill(cx, &o, Field::Path, "data/app.duckdb");
    click_id(cx, o.win, "conn-save");
    assert!(!present(cx, o.win, "conn-name"), "a saved dialog closes");
    assert_eq!(
        toml(&o),
        format!("{before}\n[connections.ducks]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n")
    );
}
