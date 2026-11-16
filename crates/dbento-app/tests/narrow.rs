use crate::support;
use crate::support::{opened, present, reload, rewrite};
use dbento_app::query_tab::{TOOLBAR_LEVELS, toolbar_level};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Bounds, Pixels, TestAppContext, px, size};

const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";
// The query toolbar's items, left to right.
const TOOLBAR: [&str; 11] = [
    "query-connection",
    "read-only-badge",
    "production-band",
    "run-split",
    "format-tip",
    "explain-tip",
    "cancel-idle",
    "query-save-tip",
    "row-limit",
    "conn-chip",
    "run-summary",
];

// Resizes the window and lets the split settle, which takes a frame after the resize.
fn resize(cx: &mut TestAppContext, win: AnyWindowHandle, w: f32, h: f32) {
    cx.simulate_window_resize(win, size(px(w), px(h)));
    for _ in 0..3 {
        cx.update_window(win, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
}
fn bounds(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    id: &'static str,
) -> Option<Bounds<Pixels>> {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find(id).map(|e| e.bounds())
    })
    .unwrap()
}
fn width(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> f32 {
    f32::from(bounds(cx, win, id).unwrap().size.width)
}

#[gpui_kit::test]
fn a_shrinking_window_takes_width_from_the_tab_area_not_the_explorer(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    resize(cx, o.win, 1440., 900.);
    let explorer = width(cx, o.win, "sidebar");
    let (min_w, min_h) = dbento_app::app::MIN_WINDOW;
    for (w, h) in [(900., 600.), (700., 600.)] {
        resize(cx, o.win, w, h);
        let sidebar = width(cx, o.win, "sidebar");
        assert!(
            (sidebar - explorer).abs() < 1.,
            "at {w}x{h} the explorer went from {explorer} to {sidebar}"
        );
    }
    // At the smallest window both give a little, and neither drops below a usable width.
    resize(cx, o.win, min_w, min_h);
    let (sidebar, tabs) = (width(cx, o.win, "sidebar"), width(cx, o.win, "tab-area"));
    assert!(sidebar >= 180., "the explorer is {sidebar}");
    assert!(tabs >= 220., "the tab area is {tabs}");
}

#[gpui_kit::test]
fn the_query_toolbar_keeps_its_items_apart_in_a_narrow_window(cx: &mut TestAppContext) {
    // A production tab whose settings changed under it, after a run: every toolbar item shows.
    let o = opened(cx, T, "color = \"red\"", 0);
    o.run(cx, "SELECT 1; SELECT 2");
    o.idle(cx);
    rewrite(&o.fx, "color = \"red\"\nread_only = true", "");
    reload(cx, o.win);
    assert!(present(cx, o.win, "conn-chip"));
    let (min_w, min_h) = dbento_app::app::MIN_WINDOW;
    for (w, h) in [(900., 600.), (min_w, min_h)] {
        resize(cx, o.win, w, h);
        let bar = bounds(cx, o.win, "query-toolbar").unwrap();
        let tabs = bounds(cx, o.win, "tab-area").unwrap();
        assert!(bar.right() <= tabs.right() + px(0.5), "{bar:?} in {tabs:?}");
        let shown: Vec<_> = TOOLBAR
            .iter()
            .filter_map(|id| bounds(cx, o.win, id).map(|b| (*id, b)))
            .filter(|(_, b)| b.size.width > px(0.5))
            .collect();
        for pair in shown.windows(2) {
            let ((a, ab), (b, bb)) = (pair[0], pair[1]);
            assert!(
                bb.left() >= ab.right() - px(0.5),
                "at {w}x{h} {b} {bb:?} overlaps {a} {ab:?}"
            );
        }
        // The connection, its production chip and Run always show in full.
        for id in ["query-connection", "production-band", "run-split"] {
            let b = bounds(cx, o.win, id).unwrap();
            assert!(
                b.right() <= bar.right() + px(0.5),
                "at {w}x{h} {id} {b:?} past {bar:?}"
            );
        }
        assert!(width(cx, o.win, "query-connection") >= 60., "at {w}x{h}");
        // A wide enough window keeps every item, the row limit included, inside the toolbar.
        if w >= 900. {
            for (id, b) in &shown {
                assert!(
                    b.right() <= bar.right() + px(0.5),
                    "at {w}x{h} {id} {b:?} past {bar:?}"
                );
            }
            assert!(shown.iter().any(|(id, _)| *id == "row-limit"));
        }
    }
    // Widening the window again brings back what the narrow one dropped.
    resize(cx, o.win, 1920., 1080.);
    assert!(present(cx, o.win, "run-summary"));
    assert!(present(cx, o.win, "run-keys"));
}

#[test]
fn the_toolbar_collapses_on_overflow_and_comes_back_once_the_width_returns() {
    let mut needed = [None; TOOLBAR_LEVELS];
    // A 700 px row in a 600 px toolbar collapses a level and remembers what it needed.
    assert_eq!(toolbar_level(0, &mut needed, px(700.), px(600.)), 1);
    assert_eq!(needed[0], Some(px(700.)));
    // It stays collapsed until the toolbar is that wide again.
    assert_eq!(toolbar_level(1, &mut needed, px(650.), px(650.)), 1);
    assert_eq!(toolbar_level(1, &mut needed, px(700.), px(700.)), 0);
    // A wide toolbar returns past every level it has room for at once.
    needed = [Some(px(700.)), Some(px(650.)), None, None];
    assert_eq!(toolbar_level(2, &mut needed, px(800.), px(800.)), 0);
    // The last level never collapses further: its row fits the toolbar by truncating.
    let last = TOOLBAR_LEVELS;
    assert_eq!(toolbar_level(last, &mut needed, px(900.), px(100.)), last);
}

#[gpui_kit::test]
fn the_production_rule_overlays_the_toolbar_instead_of_pushing_it_down(cx: &mut TestAppContext) {
    let plain = opened(cx, T, "", 0);
    let top = |cx: &mut TestAppContext, win| {
        let (bar, tabs) = (
            bounds(cx, win, "query-toolbar").unwrap(),
            bounds(cx, win, "tab-bar").unwrap(),
        );
        bar.top() - tabs.bottom()
    };
    let expected = top(cx, plain.win);
    rewrite(&plain.fx, "color = \"red\"", "");
    reload(cx, plain.win);
    // The open tab keeps its old colour until it reconnects; a new one opens as production.
    support::open_connection_row(cx, plain.win, "lite");
    assert!(present(cx, plain.win, "production-band"));
    assert_eq!(top(cx, plain.win), expected);
    let rule = bounds(cx, plain.win, "production-rule").unwrap();
    let bar = bounds(cx, plain.win, "query-toolbar").unwrap();
    assert_eq!((rule.top(), rule.size.height), (bar.top(), px(2.)));
}

// A client-decorated window pads each untiled side with the border's shadow; layout gets the rest.
#[test]
fn the_usable_size_leaves_out_the_shadow_on_each_untiled_side() {
    use dbento_app::shell::usable_size;
    use gpui_kit::{Decorations, Tiling};
    let viewport = size(px(1000.), px(700.));
    let floating = Decorations::Client {
        tiling: Tiling::default(),
    };
    assert_eq!(
        usable_size(viewport, floating, Some(px(20.))),
        size(px(960.), px(660.))
    );
    let left_half = Decorations::Client {
        tiling: Tiling {
            top: true,
            left: true,
            right: false,
            bottom: true,
        },
    };
    assert_eq!(
        usable_size(viewport, left_half, Some(px(20.))),
        size(px(980.), px(700.))
    );
    assert_eq!(
        usable_size(viewport, Decorations::Server, Some(px(20.))),
        viewport
    );
    assert_eq!(usable_size(viewport, floating, None), viewport);
}
