//! The inspector's References: outgoing and incoming links open filtered browse tabs.
use crate::support::{Opened, click_id, label_of, launched, present, wait_real};
use dbento_app::results_pane::ResultsPane;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm'), (3, 1, 'o');";

fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn select(cx: &mut TestAppContext, o: &Opened, row: usize) {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(row, 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn rows(cx: &mut TestAppContext, o: &Opened, n: u64) {
    let m = o.model(cx);
    wait_real(cx, "the filtered rows", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.rows() == n)
        })
    });
}

#[gpui_kit::test]
fn links_follow_both_directions(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert!(present(cx, o.win, "inspector-references"));
    assert_eq!(
        label_of(cx, o.win, ("ref-out", 0usize)).as_deref(),
        Some("a id = 1")
    );
    click(cx, &o, ("ref-out", 0usize));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.a\nWHERE (\nid = '1'\n)\nORDER BY id ASC",
    );
    rows(cx, &o, 1);
    select(cx, &o, 0);
    assert_eq!(
        label_of(cx, o.win, ("ref-in", 0usize)).as_deref(),
        Some("b a_id = 1")
    );
    click(cx, &o, ("ref-in", 0usize));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.b\nWHERE (\na_id = '1'\n)\nORDER BY id ASC",
    );
    rows(cx, &o, 2);
}

#[gpui_kit::test]
fn a_null_key_is_listed_and_inert(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 1);
    assert_eq!(
        label_of(cx, o.win, ("ref-out", 0usize)).as_deref(),
        Some("a id = NULL")
    );
    click(cx, &o, ("ref-out", 0usize));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
}

// Only a followable link carries the chevron that says it opens something.
#[gpui_kit::test]
fn a_null_key_row_has_no_chevron(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert!(present(cx, o.win, ("ref-chevron", 0usize)));
    select(cx, &o, 1);
    assert!(present(cx, o.win, ("ref-out", 0usize)));
    assert!(!present(cx, o.win, ("ref-chevron", 0usize)));
}

#[gpui_kit::test]
fn an_inserted_row_has_no_references(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    click(cx, &o, "inspector-duplicate");
    assert!(!present(cx, o.win, "inspector-references"));
}

// A link keys the far table by the row as loaded; a staged key edit neither relabels nor
// refilters it.
#[gpui_kit::test]
fn links_use_loaded_not_staged_values(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "2", cx)));
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, ("ref-out", 0usize)).as_deref(),
        Some("a id = 1")
    );
    click(cx, &o, ("ref-out", 0usize));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.a\nWHERE (\nid = '1'\n)\nORDER BY id ASC",
    );
}

// A link clicked in an unfocused window gives the inspector's field no blur; it stages before
// the form swaps.
#[gpui_kit::test]
fn following_a_link_stages_the_inspectors_open_field(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    let m = o.model(cx);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("2", cx))
        .unwrap();
    click(cx, &o, ("ref-out", 0usize));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("2".into())));
    });
}

// A self-reference follows into a preview, so the origin's staged edits stay and nothing asks.
#[gpui_kit::test]
fn following_a_self_reference_keeps_the_origins_edits(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE e(id INTEGER PRIMARY KEY, boss INTEGER REFERENCES e(id), name TEXT); \
         INSERT INTO e VALUES (1, NULL, 'ceo'), (2, 1, 'dev');",
        "",
    );
    o.open_relation(cx, "e", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.e\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 1);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 2, "lead", cx)));
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, ("ref-out", 0usize)).as_deref(),
        Some("e id = 1")
    );
    click(cx, &o, ("ref-out", 0usize));
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
    m.read_with(cx, |m, _| {
        assert!(m.discard_question().is_none());
        assert_eq!(m.edits().len(), 1);
    });
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.e\nWHERE (\nid = '1'\n)\nORDER BY id ASC",
    );
}

// A table with many foreign keys keeps its form readable: References scrolls inside a capped
// share of the panel instead of squeezing the form.
#[gpui_kit::test]
fn many_references_do_not_squeeze_the_form(cx: &mut TestAppContext) {
    let cols: Vec<String> = (0..24)
        .map(|i| format!("k{i} INTEGER REFERENCES a(id)"))
        .collect();
    let ones = vec!["1"; 24].join(", ");
    let setup = format!(
        "CREATE TABLE a(id INTEGER PRIMARY KEY); INSERT INTO a VALUES (1); \
         CREATE TABLE m(id INTEGER PRIMARY KEY, {}); INSERT INTO m VALUES (1, {ones});",
        cols.join(", ")
    );
    let o = launched(cx, &setup, "");
    o.open_relation(cx, "m", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.m\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert!(present(cx, o.win, ("ref-out", 23usize)));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let panel = w.find("inspector-panel").bounds().size.height;
        let refs = w.find("inspector-references").bounds().size.height;
        let form = w.find("inspector-view").bounds().size.height;
        assert!(
            refs <= panel * 0.5,
            "references {refs:?} of panel {panel:?}"
        );
        assert!(form >= panel * 0.25, "form {form:?} of panel {panel:?}");
    })
    .unwrap();
}

// A short form leaves no gap: References follows the last field, as in DBFlux.
#[gpui_kit::test]
fn references_sit_right_under_the_fields(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert!(present(cx, o.win, "inspector-references"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let last = w.find(("inspector-value", 2usize)).bounds();
        let refs = w.find("inspector-references").bounds();
        let gap = refs.origin.y - (last.origin.y + last.size.height);
        assert!(gap <= gpui_kit::px(16.), "gap {gap:?}");
    })
    .unwrap();
}

// The identity reads on the title's line, beside "Row i".
#[gpui_kit::test]
fn the_identity_sits_beside_the_title(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert!(present(cx, o.win, "inspector-identity"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let title = w.find("inspector-title").bounds();
        let identity = w.find("inspector-identity").bounds();
        assert!(identity.origin.y < title.origin.y + title.size.height);
        assert!(identity.origin.x > title.origin.x + title.size.width);
    })
    .unwrap();
}

// GPUI has no letter spacing, so the heading is tracked by spacing its letters: it reads
// "REFERENCES" and is wider than the same text set solid.
#[gpui_kit::test]
fn the_references_heading_is_letter_spaced(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    assert_eq!(
        label_of(cx, o.win, "inspector-references-heading").as_deref(),
        Some("REFERENCES")
    );
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let heading = w.find("inspector-references-heading").bounds().size.width;
        let mut font = w.text_style().font();
        font.weight = gpui_kit::FontWeight::SEMIBOLD;
        let run = gpui_kit::TextRun {
            len: "REFERENCES".len(),
            font,
            color: gpui_kit::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let solid = w
            .text_system()
            .shape_line("REFERENCES".into(), gpui_kit::px(11.), &[run], None)
            .width;
        assert!(
            heading >= solid + gpui_kit::px(9. * 1.2),
            "heading {heading:?}, solid {solid:?}"
        );
    })
    .unwrap();
}
