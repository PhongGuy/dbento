//! The palette loads catalogs only for connections already open, and on request.
use crate::support::{Opened, launched, opened, palette_pick, palette_rows, present, wait_real};
use dbento_app::catalog::CatalogQuery;
use dbento_app::palette::Mode;
use gpui_kit::component::IndexPath;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use gpui_kit::{Bounds, Pixels, Point, point, px};

const SETUP: &str = "CREATE TABLE orders(id INTEGER PRIMARY KEY);";

/// Opens the palette and types "ord": the status rows come last, so a narrowing query keeps
/// them inside the list's visible height (only "Keyboard shortcuts" among commands matches).
fn open(cx: &mut TestAppContext, o: &Opened) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_palette(Mode::All, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.input("ord", cx))
        .unwrap();
    cx.run_until_parked();
}
fn started(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell
        .read_with(cx, |s, cx| s.sidebar().unwrap().read(cx).catalogs_started())
}
fn has(cx: &mut TestAppContext, o: &Opened, label: &str) -> bool {
    palette_rows(cx, o.win).iter().any(|r| r == label)
}

#[gpui_kit::test]
fn opening_the_palette_starts_no_catalog_session(cx: &mut TestAppContext) {
    let o = launched(cx, SETUP, "");
    let before = started(cx, &o);
    open(cx, &o);
    wait_real(cx, "the idle rows", |cx| {
        has(cx, &o, "Search tables in lite") && has(cx, &o, "Search tables in nopg")
    });
    assert_eq!(started(cx, &o), before, "the palette connected on its own");
}

#[gpui_kit::test]
fn search_in_loads_that_connection_and_keeps_the_palette_open(cx: &mut TestAppContext) {
    let o = launched(cx, SETUP, "");
    let before = started(cx, &o);
    open(cx, &o);
    palette_pick(cx, o.win, "Search tables in lite");
    assert!(present(cx, o.win, "palette"));
    wait_real(cx, "lite's tables", |cx| {
        has(cx, &o, "main.orders \u{b7} lite")
    });
    assert!(!has(cx, &o, "Search tables in lite"));
    assert!(
        has(cx, &o, "Search tables in nopg"),
        "only the asked connection loads"
    );
    assert_eq!(started(cx, &o), before + 1);
}

#[gpui_kit::test]
fn a_connection_with_a_session_loads_its_tables_on_open(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    open(cx, &o);
    wait_real(cx, "lite's tables", |cx| {
        has(cx, &o, "main.orders \u{b7} lite")
    });
    assert!(!has(cx, &o, "Search tables in lite"));
    assert!(has(cx, &o, "Search tables in nopg"));
}

#[gpui_kit::test]
fn a_failed_catalog_shows_its_error_and_retry(cx: &mut TestAppContext) {
    let o = launched(cx, SETUP, "");
    open(cx, &o);
    palette_pick(cx, o.win, "Search tables in nopg");
    let failed = |cx: &mut TestAppContext| {
        palette_rows(cx, o.win)
            .into_iter()
            .find(|r| r.starts_with("Retry nopg \u{b7} "))
    };
    wait_real(cx, "the error row", |cx| failed(cx).is_some());
    let row = failed(cx).unwrap();
    palette_pick(cx, o.win, &row);
    assert!(
        present(cx, o.win, "palette"),
        "Retry keeps the palette open"
    );
    wait_real(cx, "the error again", |cx| failed(cx).is_some());
    assert!(
        !palette_rows(cx, o.win)
            .iter()
            .any(|r| r == "Search tables in nopg")
    );
}

/// The kit's rows as (bounds, selected, label), the label read from the row drawn inside.
fn kit_rows(cx: &mut TestAppContext, o: &Opened) -> Vec<(Bounds<Pixels>, bool, String)> {
    let rows = palette_rows(cx, o.win);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let paths = (0..6).flat_map(|s| (0..40).map(move |r| IndexPath::new(r).section(s)));
        paths
            .filter_map(|p| w.try_find(p))
            .filter_map(|e| {
                let b = e.bounds();
                let n = (0..rows.len())
                    .find(|&n| b.contains(&w.find(("palette-row", n)).bounds().center()))?;
                Some((b, e.selected() == Some(true), rows[n].clone()))
            })
            .collect()
    })
    .unwrap()
}
/// The label of the palette row the kit highlights.
fn highlighted(cx: &mut TestAppContext, o: &Opened) -> Option<String> {
    kit_rows(cx, o).into_iter().find(|r| r.1).map(|r| r.2)
}
/// The label of the palette row under `at`.
fn row_at(cx: &mut TestAppContext, o: &Opened, at: Point<Pixels>) -> Option<String> {
    kit_rows(cx, o)
        .into_iter()
        .find(|r| r.0.contains(&at))
        .map(|r| r.2)
}
const MANY: &str = "CREATE TABLE orders(id INTEGER PRIMARY KEY); CREATE TABLE orders2(id INTEGER); \
                    CREATE TABLE orders3(id INTEGER); CREATE TABLE orders4(id INTEGER);";

// Rows that load under a resting pointer must not steal the highlight; moving it still does.
#[gpui_kit::test]
fn loaded_rows_under_a_resting_pointer_keep_the_highlight(cx: &mut TestAppContext) {
    let o = launched(cx, MANY, "");
    open(cx, &o);
    wait_real(cx, "the idle rows", |cx| {
        has(cx, &o, "Search tables in nopg")
    });
    let at = kit_rows(cx, &o)
        .into_iter()
        .find(|r| r.2 == "Search tables in nopg")
        .unwrap()
        .0
        .center();
    cx.update_window(o.win, |_, w, cx| w.simulate_mouse_move(at, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
    // lite's tables load while the pointer rests, as a catalog that lands on its own does.
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().unwrap().clone());
    let main = CatalogQuery::Relations {
        schema: "main".into(),
    };
    wait_real(cx, "lite's tables", |cx| {
        sidebar.update(cx, |s, cx| {
            s.ensure_loaded("lite", &[CatalogQuery::Schemas, main.clone()], cx)
        });
        has(cx, &o, "main.orders4 \u{b7} lite")
    });
    let under = row_at(cx, &o, at).expect("a row under the pointer");
    assert_ne!(
        under, "Search tables in nopg",
        "the rows did not move under the pointer"
    );
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
    cx.update_window(o.win, |_, w, cx| {
        w.simulate_mouse_move(at + point(px(1.), px(0.)), cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        highlighted(cx, &o),
        Some(under),
        "a moved pointer highlights its row"
    );
}

/// Loads lite's catalog in two steps, schemas in one frame and relations in a later one,
/// as a catalog that lands on its own can; each step may leave no row under the old path.
fn load_lite_in_two_steps(cx: &mut TestAppContext, o: &Opened) {
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().unwrap().clone());
    sidebar.update(cx, |s, cx| {
        s.ensure_loaded("lite", &[CatalogQuery::Schemas], cx)
    });
    wait_real(cx, "lite's schemas", |cx| {
        !has(cx, o, "Search tables in lite")
    });
    let main = CatalogQuery::Relations {
        schema: "main".into(),
    };
    wait_real(cx, "lite's tables", |cx| {
        sidebar.update(cx, |s, cx| {
            s.ensure_loaded("lite", std::slice::from_ref(&main), cx)
        });
        has(cx, o, "main.orders4 \u{b7} lite")
    });
}

#[gpui_kit::test]
fn a_catalog_landing_in_two_steps_keeps_the_hovered_highlight(cx: &mut TestAppContext) {
    let o = launched(cx, MANY, "");
    open(cx, &o);
    wait_real(cx, "the idle rows", |cx| {
        has(cx, &o, "Search tables in nopg")
    });
    let at = kit_rows(cx, &o)
        .into_iter()
        .find(|r| r.2 == "Search tables in nopg")
        .unwrap()
        .0
        .center();
    cx.update_window(o.win, |_, w, cx| w.simulate_mouse_move(at, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
    load_lite_in_two_steps(cx, &o);
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
}

#[gpui_kit::test]
fn a_catalog_landing_in_two_steps_keeps_the_keyboard_highlight(cx: &mut TestAppContext) {
    let o = launched(cx, MANY, "");
    open(cx, &o);
    wait_real(cx, "the idle rows", |cx| {
        has(cx, &o, "Search tables in nopg")
    });
    for _ in 0..palette_rows(cx, o.win).len() {
        if highlighted(cx, &o).as_deref() == Some("Search tables in nopg") {
            break;
        }
        cx.simulate_keystrokes(o.win, "down");
        cx.run_until_parked();
    }
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
    load_lite_in_two_steps(cx, &o);
    assert_eq!(
        highlighted(cx, &o).as_deref(),
        Some("Search tables in nopg")
    );
}
