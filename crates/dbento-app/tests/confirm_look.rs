//! The dangerous-write dialog's look: button styles and keys, its frame, and the initial focus.
use crate::support;
use crate::support::{label_of, opened, present, wait_real};
use dbento_app::confirm::{
    Ask, Choice, ChoiceStyle, DIALOG_TOP, chip_colours, choice_keys, choice_style,
};
use dbento_app::query_tab::{confirm_explanation, unguarded_ask};
use dbento_core::Unguarded;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

fn ask(choices: [(&'static str, bool); 2], default: usize) -> Ask {
    Ask {
        title: "t".into(),
        detail: None,
        sql: None,
        language: "sql",
        choices: choices
            .iter()
            .map(|&(id, danger)| Choice {
                id,
                label: id.into(),
                danger,
            })
            .collect(),
        default,
        cancel: 0,
    }
}
#[test]
fn beside_run_skip_is_neutral_and_run_is_danger() {
    let a = ask([("confirm-skip", false), ("confirm-run", true)], 0);
    assert_eq!(
        [choice_style(&a, 0), choice_style(&a, 1)],
        [ChoiceStyle::Neutral, ChoiceStyle::Danger]
    );
}
#[test]
fn without_a_danger_choice_the_default_is_primary() {
    let a = ask([("no", false), ("yes", false)], 1);
    assert_eq!(
        [choice_style(&a, 0), choice_style(&a, 1)],
        [ChoiceStyle::Neutral, ChoiceStyle::Primary]
    );
}
#[test]
fn key_chips_name_only_the_keys_that_answer() {
    // Enter answers Cancel in the dangerous-write dialog, so Run anyway has no chip.
    let a = ask([("confirm-skip", false), ("confirm-run", true)], 0);
    assert_eq!(
        [choice_keys(&a, 0), choice_keys(&a, 1)],
        [Some("Esc"), None]
    );
    let b = ask([("no", false), ("yes", false)], 1);
    assert_eq!(
        [choice_keys(&b, 0), choice_keys(&b, 1)],
        [Some("Esc"), Some("\u{23CE}")]
    );
}
// A chip on a filled button takes that button's text, its boxes at 40 %, like Run's chip.
#[gpui_kit::test]
fn a_filled_buttons_key_chip_takes_its_text_colour(cx: &mut TestAppContext) {
    support::init(cx);
    cx.update(|cx| {
        use gpui_kit::component::ActiveTheme as _;
        let t = cx.theme().clone();
        assert_eq!(
            chip_colours(ChoiceStyle::Primary, cx),
            (t.primary_foreground, t.primary_foreground.opacity(0.4))
        );
        assert_eq!(
            chip_colours(ChoiceStyle::Danger, cx),
            (t.danger_foreground, t.danger_foreground.opacity(0.4))
        );
        assert_eq!(
            chip_colours(ChoiceStyle::Neutral, cx),
            (dbento_app::theme::dim(cx), t.border)
        );
    });
}
#[test]
fn the_explanation_names_what_happens_to_every_row() {
    assert_eq!(
        confirm_explanation(Some(Unguarded::UpdateWithoutWhere)),
        "It has no WHERE clause, so it changes every row."
    );
    assert_eq!(
        confirm_explanation(Some(Unguarded::DeleteWithoutWhere)),
        "It has no WHERE clause, so it deletes every row."
    );
    assert_eq!(confirm_explanation(None), "It may change rows.");
    assert_eq!(
        confirm_explanation(Some(Unguarded::Drop)),
        "It removes the object and everything in it."
    );
    assert_eq!(
        confirm_explanation(Some(Unguarded::Truncate)),
        "It removes every row of the table."
    );
}
#[test]
fn the_unguarded_ask_reads_cancel_and_run_anyway_and_carries_the_sql() {
    let a = unguarded_ask(
        Some(Unguarded::DeleteWithoutWhere),
        "prod",
        "DELETE FROM t",
        "sql",
    );
    let buttons: Vec<_> = a
        .choices
        .iter()
        .map(|c| (c.id, c.label.to_string(), c.danger))
        .collect();
    assert_eq!(
        buttons,
        [
            ("confirm-skip", "Cancel".to_owned(), false),
            ("confirm-run", "Run anyway".to_owned(), true)
        ]
    );
    assert_eq!((a.default, a.cancel), (0, 0));
    assert_eq!(a.title.to_string(), "DELETE without WHERE on 'prod'");
    assert_eq!(
        a.detail.as_deref(),
        Some(confirm_explanation(Some(Unguarded::DeleteWithoutWhere)))
    );
    assert_eq!(a.sql.as_deref(), Some("DELETE FROM t"));
}
#[gpui_kit::test]
fn skip_has_the_initial_focus_so_space_skips(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);",
        "",
        0,
    );
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    cx.update_window(o.win, |_, w, cx| w.press("space", cx))
        .unwrap();
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("skipped")
    );
}
#[gpui_kit::test]
fn the_dialog_is_480_px_under_the_toolbar_with_a_48_px_footer(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x);", "color = \"red\"", 0);
    // At 800 px the kit's default offset (a tenth of the height) lands on the toolbar.
    cx.simulate_window_resize(o.win, gpui_kit::size(px(1280.), px(800.)));
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(
        label_of(cx, o.win, "confirm-sql").as_deref(),
        Some("UPDATE t SET x = 0")
    );
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let band = w.find("production-band").bounds();
        let surface = w.find("confirm-dialog").bounds();
        let run = w.find("confirm-run").bounds();
        assert!(surface.top() >= band.bottom(), "{surface:?} vs {band:?}");
        assert!(
            surface.top() >= DIALOG_TOP && surface.top() < DIALOG_TOP + px(4.),
            "{surface:?}"
        );
        assert!(
            (f32::from(surface.size.width) - 480.).abs() <= 2.,
            "{surface:?}"
        );
        assert_eq!(run.size.height, px(32.));
        // The 32 px buttons sit centred in the 48 px footer at the surface's bottom.
        let below = f32::from(surface.bottom() - run.bottom());
        assert!((below - 8.).abs() <= 1., "{run:?} in {surface:?}");
    })
    .unwrap();
}

#[test]
fn a_mongodb_prompt_previews_in_javascript() {
    let a = unguarded_ask(
        Some(Unguarded::DeleteAllDocuments),
        "shop",
        "db.orders.deleteMany({})",
        "javascript",
    );
    assert_eq!(a.language, "javascript");
    assert_eq!(
        a.title.as_ref(),
        "deleteMany with an empty filter on 'shop'"
    );
}

// Whether `id` holds the keyboard focus, read after a fresh frame.
fn focused(cx: &mut TestAppContext, o: &support::Opened, id: &'static str) -> bool {
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).focused() == Some(true)
    })
    .unwrap()
}

fn press(cx: &mut TestAppContext, o: &support::Opened, key: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
}

fn asked(cx: &mut TestAppContext, o: &support::Opened) {
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
}

#[gpui_kit::test]
fn enter_with_the_initial_focus_cancels(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    asked(cx, &o);
    assert!(focused(cx, &o, "confirm-skip"));
    press(cx, &o, "enter");
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
}

#[gpui_kit::test]
fn tab_then_enter_runs_the_focused_run_anyway(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    asked(cx, &o);
    press(cx, &o, "tab");
    assert!(focused(cx, &o, "confirm-run"));
    assert!(!focused(cx, &o, "confirm-skip"));
    press(cx, &o, "enter");
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 1);
}

#[gpui_kit::test]
fn escape_cancels_even_with_run_anyway_focused(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    asked(cx, &o);
    press(cx, &o, "tab");
    assert!(focused(cx, &o, "confirm-run"));
    press(cx, &o, "escape");
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    assert!(!present(cx, o.win, "confirm-dialog"));
}

// A dialog whose Enter answers a later choice keeps the first button focused for Space and
// Enter on its default, even with that first button focused.
#[gpui_kit::test]
fn a_later_default_keeps_enter_while_the_first_choice_holds_focus(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let rx = cx
        .update_window(o.win, |_, w, cx| {
            dbento_app::confirm::ask(ask([("no", false), ("yes", false)], 1), w, cx)
        })
        .unwrap();
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert!(focused(cx, &o, "no"));
    press(cx, &o, "enter");
    assert_eq!(rx.try_recv().ok(), Some(1));
}
