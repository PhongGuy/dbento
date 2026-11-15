use crate::support::{self, click_id, fill, form, label_of, opened, wait_real};
use dbento_app::connection_dialog::{Field, TestState};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Bounds, Pixels, TestAppContext, px};

fn bounds(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> Bounds<Pixels> {
    cx.run_until_parked();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).bounds()
    })
    .unwrap()
}

fn pick_engine(cx: &mut TestAppContext, win: AnyWindowHandle, ix: usize) {
    cx.update_window(win, |_, w, cx| w.within("conn-engine").click(ix, cx))
        .unwrap();
    cx.run_until_parked();
}

const LONG_FAILURE: &str = "error connecting to server: Connection refused (os error 111) \
    while resolving the host and opening a TCP connection to port 5432 on every address";

// The top edge holds through every engine (to the kit's snap of a fractional height to a
// whole pixel), and a Test result coming and going never moves
// the Test button, whether the form fits the window or scrolls in a short one.
fn holds_still(cx: &mut TestAppContext, height: f32) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1440.), px(height)));
    click_id(cx, o.win, "connection-new");
    let f = form(cx, &o);
    let top = bounds(cx, o.win, "conn-dialog").origin.y;
    for ix in [1usize, 2, 3, 5, 6, 7, 8, 0] {
        pick_engine(cx, o.win, ix);
        let moved = (bounds(cx, o.win, "conn-dialog").origin.y - top).abs();
        assert!(
            moved < px(1.),
            "engine {ix} at height {height} moved {moved:?}"
        );
        let test = bounds(cx, o.win, "conn-test");
        for state in [
            TestState::Testing,
            TestState::Failed(LONG_FAILURE.into()),
            TestState::Connected,
            TestState::Idle,
        ] {
            f.update(cx, |f, cx| f.set_test(state.clone(), cx));
            assert_eq!(
                bounds(cx, o.win, "conn-test"),
                test,
                "engine {ix}, {state:?}, height {height}"
            );
            let moved = (bounds(cx, o.win, "conn-dialog").origin.y - top).abs();
            assert!(
                moved < px(1.),
                "engine {ix}, {state:?}: the top moved {moved:?}"
            );
        }
    }
}

#[gpui_kit::test]
fn the_dialog_and_its_test_button_hold_still_in_a_tall_window(cx: &mut TestAppContext) {
    holds_still(cx, 1200.);
}

#[gpui_kit::test]
fn the_dialog_and_its_test_button_hold_still_in_a_short_window(cx: &mut TestAppContext) {
    holds_still(cx, 600.);
}

#[gpui_kit::test]
fn test_connects_without_a_name(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    pick_engine(cx, o.win, 1);
    fill(cx, &o, Field::Path, "data/app.db");
    click_id(cx, o.win, "conn-test");
    wait_real(cx, "the test result", |cx| {
        label_of(cx, o.win, "conn-test-result").is_some_and(|l| l != "Testing\u{2026}")
    });
    assert_eq!(
        label_of(cx, o.win, "conn-test-result").as_deref(),
        Some("Connected")
    );
}

#[gpui_kit::test]
fn a_bad_port_shows_under_the_port_and_marks_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1440.), px(1200.)));
    click_id(cx, o.win, "connection-new");
    let f = form(cx, &o);
    fill(cx, &o, Field::Name, "pg");
    fill(cx, &o, Field::Host, "h");
    fill(cx, &o, Field::Port, "70000");
    fill(cx, &o, Field::Database, "d");
    click_id(cx, o.win, "conn-save");
    assert!(
        label_of(cx, o.win, "conn-error").is_some(),
        "Save shows an error"
    );
    assert_eq!(f.read_with(cx, |f, _| f.error_field()), Some(Field::Port));
    let (error, port, next) = (
        bounds(cx, o.win, "conn-error"),
        bounds(cx, o.win, "conn-port"),
        bounds(cx, o.win, "conn-database"),
    );
    assert!(error.top() >= port.bottom(), "{error:?} under {port:?}");
    assert!(error.bottom() <= next.top(), "{error:?} above {next:?}");
}

#[gpui_kit::test]
fn a_missing_name_shows_under_the_name(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1440.), px(1200.)));
    click_id(cx, o.win, "connection-new");
    let f = form(cx, &o);
    fill(cx, &o, Field::Host, "h");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("Enter a name")
    );
    assert_eq!(f.read_with(cx, |f, _| f.error_field()), Some(Field::Name));
    let (error, name, engine) = (
        bounds(cx, o.win, "conn-error"),
        bounds(cx, o.win, "conn-name"),
        bounds(cx, o.win, "conn-engine"),
    );
    assert!(
        error.top() >= name.bottom() && error.bottom() <= engine.top(),
        "{error:?} between {name:?} and {engine:?}"
    );
    // Typing the name clears both the line and the mark.
    let name = f.read_with(cx, |f, _| f.input(Field::Name).clone());
    cx.update_window(o.win, |_, w, cx| {
        name.update(cx, |s, cx| s.focus(w, cx));
        w.input("pg", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(f.read_with(cx, |f, _| f.error_field()), None);
    assert!(
        !support::present(cx, o.win, "conn-error"),
        "the error line goes"
    );
}

#[gpui_kit::test]
fn a_taken_name_shows_under_the_name(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    let f = form(cx, &o);
    fill(cx, &o, Field::Name, "lite");
    fill(cx, &o, Field::Host, "h");
    fill(cx, &o, Field::Database, "d");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("a connection named `lite` already exists")
    );
    assert_eq!(f.read_with(cx, |f, _| f.error_field()), Some(Field::Name));
}

#[gpui_kit::test]
fn delete_sits_alone_at_the_far_left_of_the_edit_footer(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1440.), px(900.)));
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    let (dialog, delete, test, cancel, save) = (
        bounds(cx, o.win, "conn-dialog"),
        bounds(cx, o.win, "conn-delete"),
        bounds(cx, o.win, "conn-test"),
        bounds(cx, o.win, "conn-cancel"),
        bounds(cx, o.win, "conn-save"),
    );
    assert!(
        delete.left() - dialog.left() <= px(20.),
        "{delete:?} in {dialog:?}"
    );
    assert!(
        test.left() - delete.right() >= px(120.),
        "{delete:?} then {test:?}"
    );
    assert!(
        test.right() <= cancel.left() && cancel.right() <= save.left(),
        "Test {test:?}, Cancel {cancel:?}, Save {save:?} in order"
    );
    assert!(
        dialog.right() - save.right() <= px(20.),
        "Save {save:?} ends the row"
    );
}

#[gpui_kit::test]
fn an_unnamed_test_never_shows_the_stand_in_name(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    pick_engine(cx, o.win, 2);
    fill(cx, &o, Field::Host, "127.0.0.1");
    click_id(cx, o.win, "conn-test");
    wait_real(cx, "the test result", |cx| {
        label_of(cx, o.win, "conn-test-result").is_some_and(|l| l.starts_with("Failed: "))
    });
    let failed = label_of(cx, o.win, "conn-test-result").unwrap();
    assert!(!failed.contains("unnamed"), "{failed}");
    assert!(failed.contains("user"), "{failed}");
}
