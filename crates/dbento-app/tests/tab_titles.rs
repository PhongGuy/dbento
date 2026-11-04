//! Tab titles: the connection is added when titles clash, production lines stay strong, labels persist.
mod support;
use std::path::Path;

use dbento_app::app::AppPaths;
use dbento_app::drafts::{DRAFT_DEBOUNCE, Draft};
use dbento_app::restore::plan_restore;
use dbento_app::shell::{tab_labels, tab_line_strength};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use support::{label_of, launched, opened, present, wait_real};

#[test]
fn clashing_titles_name_their_connection() {
    let tabs = [
        ("main.customers", "shop"),
        ("main.customers", "shop-prod"),
        ("Query 1", "shop"),
        ("main.orders", "shop"),
    ];
    assert_eq!(
        tab_labels(&tabs),
        [
            "main.customers \u{B7} shop",
            "main.customers \u{B7} shop-prod",
            "Query 1",
            "main.orders"
        ]
    );
}

#[test]
fn the_same_title_on_one_connection_is_no_clash() {
    assert_eq!(
        tab_labels(&[("a.sql", "shop"), ("a.sql", "shop")]),
        ["a.sql", "a.sql"]
    );
    assert_eq!(
        tab_labels(&[("a.sql", "shop"), ("a.sql", "shop"), ("a.sql", "prod")]),
        [
            "a.sql \u{B7} shop",
            "a.sql \u{B7} shop",
            "a.sql \u{B7} prod"
        ]
    );
}

#[test]
fn production_lines_stay_full_strength() {
    assert_eq!(tab_line_strength(true, false), 1.);
    assert_eq!(tab_line_strength(false, true), 1.);
    assert_eq!(tab_line_strength(false, false), 0.5);
}

#[test]
fn a_draft_keeps_its_label_and_restore_carries_it() {
    let d = Draft::new("d1", Path::new("/w"), "lite", None, "SELECT 1").with_label(Some("daily"));
    let back: Draft = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back.label.as_deref(), Some("daily"));
    let unlabelled =
        serde_json::to_string(&Draft::new("d2", Path::new("/w"), "lite", None, "x")).unwrap();
    assert!(!unlabelled.contains("label"), "{unlabelled}");
    let old = r#"{"v":1,"id":"d3","workspace":"/w","connection":"lite","file":null,"sql":"x","saved_at_ms":1}"#;
    assert_eq!(serde_json::from_str::<Draft>(old).unwrap().label, None);
    let (tabs, _) = plan_restore(None, vec![back], &["lite"]);
    assert_eq!(tabs[0].label.as_deref(), Some("daily"));
}

#[gpui_kit::test]
fn two_tabs_with_one_title_on_two_connections_are_told_apart(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    support::open_connection_row(cx, o.win, "lite");
    support::open_connection_row(cx, o.win, "nopg");
    for ix in 0..2 {
        let tab = o.shell.read_with(cx, |s, _| s.tabs()[ix].clone());
        tab.update(cx, |t, cx| t.set_label(Some("orders".into()), cx));
    }
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("orders \u{B7} lite")
    );
    assert_eq!(
        label_of(cx, o.win, ("tab", 1usize)).as_deref(),
        Some("orders \u{B7} nopg")
    );
}

#[gpui_kit::test]
fn an_inactive_production_tab_keeps_its_line(cx: &mut TestAppContext) {
    let o = opened(cx, "", "color = \"red\"", 0);
    support::open_connection_row(cx, o.win, "nopg");
    assert!(
        !present(cx, o.win, "tab-accent"),
        "nopg is untagged and active"
    );
    assert_eq!(
        label_of(cx, o.win, ("tab-line", 0usize)).as_deref(),
        Some("red")
    );
}

#[gpui_kit::test]
fn a_label_is_the_title_and_is_kept_as_a_draft(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[0].clone());
    tab.update(cx, |t, cx| t.set_label(Some("daily orders".into()), cx));
    cx.executor().advance_clock(DRAFT_DEBOUNCE);
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("daily orders")
    );
    let dir = AppPaths::in_dir(o.files.path()).drafts;
    wait_real(cx, "the labelled draft", |_| {
        std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("daily orders")))
    });
}

#[gpui_kit::test]
fn a_long_clashing_title_never_cuts_its_connection(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    support::open_connection_row(cx, o.win, "lite");
    support::open_connection_row(cx, o.win, "nopg");
    for ix in 0..2 {
        let tab = o.shell.read_with(cx, |s, _| s.tabs()[ix].clone());
        tab.update(cx, |t, cx| {
            t.set_label(Some("main.a_rather_long_relation_name".into()), cx)
        });
    }
    assert_eq!(
        label_of(cx, o.win, ("tab-suffix", 1usize)).as_deref(),
        Some("nopg")
    );
    // The suffix keeps its full width inside the handle; the title is what truncates.
    let (tab, suffix) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find(("tab", 1usize)).bounds(),
                w.find(("tab-suffix", 1usize)).bounds(),
            )
        })
        .unwrap();
    assert!(suffix.size.width > px(0.), "{suffix:?}");
    assert!(
        suffix.left() >= tab.left() && suffix.right() <= tab.right(),
        "{suffix:?} in {tab:?}"
    );
}
