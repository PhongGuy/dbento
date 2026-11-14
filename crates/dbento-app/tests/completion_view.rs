use crate::support::{Opened, click_id, opened, present, wait_real};
use dbento_app::completion::CandidateKind;
use dbento_app::completion_popup::{CompletionPopup, kind_icon};
use dbento_app::schema_tree::CatalogTree;
use dbento_app::ui::Icons;
use dbento_core::RelationRef;
use gpui_kit::component::input::EditorState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Pixels, TestAppContext, px};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT); \
                 CREATE TABLE Artist(ArtistId INTEGER, Name TEXT);";

fn editor(cx: &mut TestAppContext, o: &Opened) -> Entity<EditorState> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone())
}
/// `sql` in the focused editor with the caret at `caret`.
fn place(cx: &mut TestAppContext, o: &Opened, sql: &str, caret: usize) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = editor(cx, o);
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(caret..caret, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}
fn typed(cx: &mut TestAppContext, o: &Opened, text: &str) {
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
}
fn popup(cx: &mut TestAppContext, o: &Opened) -> Entity<CompletionPopup> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).completion().clone())
}
fn menu(cx: &mut TestAppContext, o: &Opened) -> Vec<String> {
    if !present(cx, o.win, "completion-popup") {
        return Vec::new();
    }
    popup(cx, o).read_with(cx, |p, _| {
        p.items().iter().map(|i| i.label.clone()).collect()
    })
}
/// The popup's rendered width.
fn width(cx: &mut TestAppContext, o: &Opened) -> Pixels {
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        w.find("completion-popup").bounds().size.width
    })
    .unwrap()
}
fn press(cx: &mut TestAppContext, o: &Opened, key: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}
fn cached(cx: &mut TestAppContext, o: &Opened, f: impl Fn(&CatalogTree) -> bool) -> bool {
    o.shell
        .read_with(cx, |s, cx| f(s.sidebar().unwrap().read(cx).tree()))
}

#[gpui_kit::test]
fn a_relation_completes_once_the_catalog_has_loaded(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "A");
    // The first keystroke finds nothing cached: it starts the loads and never waits on them;
    // its answer fills in once they land.
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| {
            t.schemas("lite").is_some() && t.relations("lite", "main").is_some()
        })
    });
    typed(cx, &o, "l");
    assert_eq!(menu(cx, &o), ["Album"]);
    cx.update_window(o.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    let text = editor(cx, &o).read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text, "SELECT * FROM Album");
    assert!(menu(cx, &o).is_empty());
}

#[gpui_kit::test]
fn a_column_completes_with_its_type_and_a_space_closes_the_menu(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT  FROM Album", 7);
    typed(cx, &o, "T");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "i");
    let album = RelationRef::new("main", "Album");
    wait_real(cx, "Album's columns", |cx| {
        cached(cx, &o, |t| t.columns("lite", &album).is_some())
    });
    typed(cx, &o, "t");
    assert_eq!(menu(cx, &o), ["Title"]);
    let detail = popup(cx, &o).read_with(cx, |p, _| p.items()[0].detail.clone());
    assert_eq!(detail, "TEXT");
    typed(cx, &o, " ");
    assert!(menu(cx, &o).is_empty());
    let text = editor(cx, &o).read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text, "SELECT Tit  FROM Album");
}

#[gpui_kit::test]
fn completion_loads_leave_the_tree_collapsed(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "A");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    assert!(cached(cx, &o, |t| t.rows("lite", 0, "").is_empty()));
}

// Each label highlights the typed prefix, measured on the label, never the whole line typed.
#[gpui_kit::test]
fn each_item_highlights_exactly_the_typed_prefix(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "A");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "l");
    let lit = popup(cx, &o).read_with(cx, |p, _| {
        (0..p.items().len())
            .map(|ix| p.items()[ix].label[..p.highlight(ix)].to_owned())
            .collect::<Vec<_>>()
    });
    assert_eq!(lit, ["Al"]);
}

// Case folding can change a character's byte length (the Kelvin sign folds to `k`), so the
// highlight must be measured on the label, never on the typed text.
#[gpui_kit::test]
fn the_highlight_ends_on_a_char_boundary_of_the_label(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE Kabé(x INTEGER);", "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "\u{212A}");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "a");
    let items = popup(cx, &o).read_with(cx, |p, _| {
        (0..p.items().len())
            .map(|ix| (p.items()[ix].label.clone(), p.highlight(ix)))
            .collect::<Vec<_>>()
    });
    assert_eq!(items.len(), 1, "{items:?}");
    let (label, lit) = &items[0];
    assert!(label.is_char_boundary(*lit), "{label:?} cut at {lit}");
    assert_eq!(&label[..*lit], "Ka");
}

#[gpui_kit::test]
fn ctrl_enter_runs_and_closes_an_open_menu(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT  FROM Album", 7);
    typed(cx, &o, "T");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "i");
    let album = RelationRef::new("main", "Album");
    wait_real(cx, "Album's columns", |cx| {
        cached(cx, &o, |t| t.columns("lite", &album).is_some())
    });
    typed(cx, &o, "tle");
    assert_eq!(menu(cx, &o), ["Title"]);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-enter", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(menu(cx, &o).is_empty(), "the menu stayed open over the run");
    o.idle(cx);
    let ran: Vec<String> = o.model(cx).read_with(cx, |m, _| {
        m.results().iter().map(|r| r.sql.clone()).collect()
    });
    assert_eq!(ran, ["SELECT Title FROM Album"]);
}

/// Opens Album and Artist in the popup at "SELECT * FROM A", the catalog already cached.
fn both_relations(cx: &mut TestAppContext, o: &Opened) {
    place(cx, o, "SELECT * FROM ", 14);
    typed(cx, o, "A");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, o, |t| t.relations("lite", "main").is_some())
    });
    place(cx, o, "SELECT * FROM ", 14);
    typed(cx, o, "A");
    assert_eq!(
        menu(cx, o),
        ["Album", "Artist"],
        "completion offers the relations starting with A"
    );
}

#[gpui_kit::test]
fn arrows_move_the_selection_and_enter_inserts_it(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    both_relations(cx, &o);
    press(cx, &o, "down");
    assert_eq!(popup(cx, &o).read_with(cx, |p, _| p.selected()), 1);
    press(cx, &o, "enter");
    let text = editor(cx, &o).read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text, "SELECT * FROM Artist");
    assert!(menu(cx, &o).is_empty());
}

#[gpui_kit::test]
fn escape_closes_the_popup_and_leaves_the_text(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    both_relations(cx, &o);
    press(cx, &o, "escape");
    assert!(menu(cx, &o).is_empty());
    let text = editor(cx, &o).read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text, "SELECT * FROM A");
}

#[gpui_kit::test]
fn a_click_on_a_row_inserts_it(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    both_relations(cx, &o);
    click_id(cx, o.win, ("completion-row", 1usize));
    let text = editor(cx, &o).read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text, "SELECT * FROM Artist");
}

#[gpui_kit::test]
fn moving_the_caret_away_closes_the_popup(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    both_relations(cx, &o);
    press(cx, &o, "left");
    assert!(menu(cx, &o).is_empty());
}

const WIDE: &str = "CREATE TABLE x1(a INTEGER); \
    CREATE TABLE zz_a_rather_long_table_name_for_the_width(a INTEGER);";

// The kit latched the width at first open; this popup sizes to each answer, up to a cap.
#[gpui_kit::test]
fn the_popup_width_follows_its_content_up_to_a_cap(cx: &mut TestAppContext) {
    let huge = format!("CREATE TABLE w{}(a INTEGER);", "w".repeat(200));
    let o = opened(cx, &format!("{WIDE} {huge}"), "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "x");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "1");
    assert_eq!(menu(cx, &o), ["x1"]);
    let short = width(cx, &o);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "z");
    assert_eq!(menu(cx, &o).len(), 1);
    let long = width(cx, &o);
    assert!(long > short + px(100.), "{short:?} then {long:?}");
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "w");
    assert_eq!(menu(cx, &o).len(), 1);
    assert_eq!(width(cx, &o), dbento_app::completion_popup::MAX_WIDTH);
}

#[gpui_kit::test]
fn each_kind_draws_its_explorer_glyph(_cx: &mut TestAppContext) {
    assert_eq!(kind_icon(CandidateKind::Column), Icons::Columns3);
    assert_eq!(kind_icon(CandidateKind::Relation), Icons::Table2);
    assert_eq!(kind_icon(CandidateKind::Schema), Icons::Layers);
    assert_eq!(kind_icon(CandidateKind::Keyword), Icons::FileCode);
}

// A command's usage line or a long type yields to the label, which keeps its full width.
#[gpui_kit::test]
fn a_long_detail_truncates_inside_the_popup_and_the_label_keeps_its_width(cx: &mut TestAppContext) {
    let ty = format!("T{}", "y".repeat(200));
    let o = opened(cx, &format!("CREATE TABLE Album(Title {ty});"), "", 0);
    place(cx, &o, "SELECT  FROM Album", 7);
    typed(cx, &o, "T");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    typed(cx, &o, "i");
    let album = RelationRef::new("main", "Album");
    wait_real(cx, "Album's columns", |cx| {
        cached(cx, &o, |t| t.columns("lite", &album).is_some())
    });
    typed(cx, &o, "t");
    assert_eq!(menu(cx, &o), ["Title"]);
    let (pop, label, detail) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("completion-popup").bounds(),
                w.find(("completion-label", 0usize)).bounds(),
                w.find(("completion-detail", 0usize)).bounds(),
            )
        })
        .unwrap();
    assert!(label.size.width > px(20.), "label squeezed to {label:?}");
    assert!(detail.size.width > px(0.), "{detail:?}");
    assert!(
        detail.right() <= pop.right(),
        "detail {detail:?} overflows popup {pop:?}"
    );
}

// The first keystroke after a dot or FROM finds the catalog unloaded; the answer fills in
// once the load lands, without waiting for another keystroke.
#[gpui_kit::test]
fn an_open_answer_fills_in_when_the_catalog_it_wanted_lands(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    place(cx, &o, "SELECT * FROM ", 14);
    typed(cx, &o, "A");
    wait_real(cx, "main's relations", |cx| {
        cached(cx, &o, |t| t.relations("lite", "main").is_some())
    });
    cx.update_window(o.win, |_, w, cx| w.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(menu(cx, &o), ["Album", "Artist"]);
}
