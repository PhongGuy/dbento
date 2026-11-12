use crate::support::{open_connection_row, opened, started};
use dbento_app::comment::toggle_line_comments;
use dbento_app::shell::Shell;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::ops::Range;

/// The text and selection after toggling `marker` over `sel` in `text`; unchanged when the
/// toggle makes no edit.
fn toggled(text: &str, sel: Range<usize>, marker: &str) -> (String, Range<usize>) {
    match toggle_line_comments(text, sel.clone(), marker) {
        Some(edit) => {
            let mut out = text.to_owned();
            out.replace_range(edit.range, &edit.text);
            (out, edit.selection)
        }
        None => (text.to_owned(), sel),
    }
}

#[test]
fn the_caret_line_gets_the_marker_and_the_caret_moves_with_its_text() {
    assert_eq!(
        toggled("select 1;\nselect 2;", 12..12, "--"),
        ("select 1;\n-- select 2;".to_owned(), 15..15)
    );
}

#[test]
fn a_commented_caret_line_loses_the_marker_and_one_space() {
    assert_eq!(
        toggled("select 1;\n-- select 2;", 15..15, "--"),
        ("select 1;\nselect 2;".to_owned(), 12..12)
    );
}

#[test]
fn a_marker_without_a_space_is_removed_alone() {
    assert_eq!(
        toggled("--select 1;", 0..0, "--").0,
        "select 1;",
        "no space follows, so only the marker goes"
    );
    assert_eq!(toggled("--  select 1;", 0..0, "--").0, " select 1;");
}

#[test]
fn a_mix_of_commented_and_plain_lines_comments_them_all() {
    let text = "-- a\nb\n-- c";
    assert_eq!(
        toggled(text, 0..text.len(), "--").0,
        "-- -- a\n-- b\n-- -- c"
    );
}

#[test]
fn all_commented_lines_are_uncommented() {
    let text = "-- a\n--b\n  -- c";
    assert_eq!(toggled(text, 0..text.len(), "--").0, "a\nb\n  c");
}

#[test]
fn the_marker_goes_at_the_least_indented_column_and_keeps_indentation() {
    let text = "    a\n  b\n      c";
    assert_eq!(
        toggled(text, 0..text.len(), "//").0,
        "  //   a\n  // b\n  //     c"
    );
    assert_eq!(
        toggled("  //   a\n  // b\n  //     c", 0..26, "//").0,
        text,
        "toggling back restores the text"
    );
}

#[test]
fn blank_lines_are_left_alone_and_do_not_lower_the_column() {
    let text = "  a\n\n \n  b";
    let (out, _) = toggled(text, 0..text.len(), "#");
    assert_eq!(out, "  # a\n\n \n  # b");
    assert_eq!(toggled(&out, 0..out.len(), "#").0, text);
}

#[test]
fn only_blank_lines_make_no_edit() {
    assert_eq!(toggle_line_comments("\n  \n", 0..4, "--"), None);
    assert_eq!(toggle_line_comments("", 0..0, "--"), None);
}

#[test]
fn a_selection_ending_at_column_zero_leaves_that_line_out() {
    let text = "a\nb\nc";
    let (out, sel) = toggled(text, 0..4, "--");
    assert_eq!(out, "-- a\n-- b\nc");
    assert_eq!(sel, 0..10, "the selection still ends at the start of `c`");
}

#[test]
fn the_selection_keeps_covering_the_same_lines() {
    let text = "select 1;\nselect 2;\nselect 3;";
    // From inside the first line to inside the second.
    let (out, sel) = toggled(text, 3..14, "--");
    assert_eq!(out, "-- select 1;\n-- select 2;\nselect 3;");
    assert_eq!(&out[sel.clone()], "ect 1;\n-- sele");
    let (back, sel) = toggled(&out, sel, "--");
    assert_eq!(back, text);
    assert_eq!(sel, 3..14);
}

#[test]
fn a_selection_from_a_line_start_includes_the_new_marker() {
    let (out, sel) = toggled("a\nb", 0..3, "--");
    assert_eq!(out, "-- a\n-- b");
    assert_eq!(sel, 0..9);
}

#[test]
fn crlf_lines_keep_their_endings() {
    let text = "a\r\nb\r\n";
    assert_eq!(toggled(text, 0..4, "--").0, "-- a\r\n-- b\r\n");
    assert_eq!(toggled("-- a\r\n-- b\r\n", 0..10, "--").0, text);
}

#[test]
fn non_ascii_text_is_cut_on_line_boundaries_only() {
    let text = "æø\n  å";
    let (out, _) = toggled(text, 0..text.len(), "--");
    assert_eq!(out, "-- æø\n--   å");
    assert_eq!(toggled(&out, 0..out.len(), "--").0, text);
}

/// The window and shell a test drives, from either harness entry point.
struct At {
    win: AnyWindowHandle,
    shell: Entity<Shell>,
}

fn key(cx: &mut TestAppContext, o: &At, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}

fn place(cx: &mut TestAppContext, o: &At, text: &str, sel: Range<usize>) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(text, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(sel, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}

fn text(cx: &mut TestAppContext, o: &At) -> String {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx))
}

fn selected(cx: &mut TestAppContext, o: &At) -> Range<usize> {
    o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).editor().read(cx).selected_range()
    })
}

const RAW: &str = "select 1;\n  select 2;\nselect 3;";

#[gpui_kit::test]
fn ctrl_slash_comments_the_selected_lines_and_one_undo_restores_them(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let o = At {
        win: o.win,
        shell: o.shell,
    };
    place(cx, &o, RAW, 3..14);
    key(cx, &o, "ctrl-/");
    assert_eq!(text(cx, &o), "-- select 1;\n--   select 2;\nselect 3;");
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 3..14, "undo puts the selection back");
}

#[gpui_kit::test]
fn ctrl_slash_twice_uncomments_the_caret_line(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let o = At {
        win: o.win,
        shell: o.shell,
    };
    place(cx, &o, RAW, 25..25);
    key(cx, &o, "ctrl-/");
    assert_eq!(text(cx, &o), "select 1;\n  select 2;\n-- select 3;");
    key(cx, &o, "ctrl-/");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 25..25);
}

/// A SurrealDB connection that never connects; the tab opens and its preconnect fails quietly.
const SR_CLOSED: &str = "dbento = 1\n[connections.sr]\nengine = \"surrealdb\"\nhost = \"127.0.0.1\"\nport = 9\nnamespace = \"app\"\ndatabase = \"main\"\n";

#[gpui_kit::test]
fn a_surrealdb_tab_comments_with_its_own_marker(cx: &mut TestAppContext) {
    let s = started(cx, SR_CLOSED);
    open_connection_row(cx, s.win, "sr");
    let s = At {
        win: s.win,
        shell: s.shell,
    };
    place(cx, &s, "select * from person", 0..0);
    key(cx, &s, "ctrl-/");
    assert_eq!(text(cx, &s), "// select * from person");
}
