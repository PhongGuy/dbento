//! Density and the grid font size set the grid's row and header pitch and the tree's row height live.
use crate::support::{TREE, opened, started};
use dbento_app::explorer::{connection_id, folder_id};
use dbento_app::settings::{Density, Settings};
use dbento_app::settings_store;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px};

fn set(cx: &mut TestAppContext, f: impl FnOnce(&mut Settings)) {
    let store = cx.update(|cx| settings_store::store(cx)).unwrap();
    store.update(cx, |s, cx| s.change(cx, f));
    cx.run_until_parked();
}
fn pitches(cx: &mut TestAppContext, win: AnyWindowHandle) -> (gpui_kit::Pixels, gpui_kit::Pixels) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let (a, b) = (w.find("cell-0-0").bounds(), w.find("cell-1-0").bounds());
        let head = w.find(("result-col", 0usize)).bounds();
        (b.top() - a.top(), a.top() - head.top())
    })
    .unwrap()
}
const ROWS: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

#[gpui_kit::test]
fn compact_grid_rows_and_header_are_24_px(cx: &mut TestAppContext) {
    let o = opened(cx, ROWS, "", 0);
    set(cx, |s| s.density = Density::Compact);
    o.run(cx, "SELECT x FROM t");
    o.idle(cx);
    assert_eq!(pitches(cx, o.win), (px(24.), px(24.)));
    set(cx, |s| s.density = Density::Default);
    assert_eq!(
        pitches(cx, o.win),
        (px(28.), px(28.)),
        "Default restores 28 live"
    );
}

#[gpui_kit::test]
fn a_large_grid_font_raises_the_row(cx: &mut TestAppContext) {
    let o = opened(cx, ROWS, "", 0);
    set(cx, |s| {
        s.density = Density::Compact;
        s.grid_font.size = 14;
    });
    o.run(cx, "SELECT x FROM t");
    o.idle(cx);
    assert_eq!(
        pitches(cx, o.win),
        (px(26.), px(26.)),
        "ceil(17.5) + 8 beats the 24 base"
    );
}

#[gpui_kit::test]
fn compact_tree_rows_are_22_px(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    set(cx, |v| v.density = Density::Compact);
    let (team, sub, lite) = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find(folder_id(&"team".parse().unwrap())).bounds(),
                w.find(folder_id(&"team/sub".parse().unwrap())).bounds(),
                w.find(connection_id("lite")).bounds(),
            )
        })
        .unwrap();
    assert_eq!(
        (team.size.height, sub.top() - team.top(), lite.size.height),
        (px(22.), px(22.), px(22.))
    );
}
