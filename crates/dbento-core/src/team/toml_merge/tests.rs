use super::{Place, TomlMergeConflict, Unit, identity, merge_workspace_text, units};

/// Each unit of `text` as its name, place and slice, in source order.
fn sliced(text: &str) -> Vec<(String, Place, &str)> {
    units(text)
        .expect("mergeable")
        .into_iter()
        .map(|Unit { name, place, range }| (name, place, &text[range]))
        .collect()
}

fn table(name: &str) -> Place {
    Place::Table(name.to_owned())
}

#[test]
fn subtables_and_comments_above_a_header_belong_to_its_entry() {
    let text = "# workspace\ndbento = 1\n\n# above a\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n\n[connections.a.settings]\nx = 1\n\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\n# the end\n";
    assert_eq!(
        sliced(text),
        [
            (
                "dbento".to_owned(),
                Place::Root,
                "# workspace\ndbento = 1\n"
            ),
            (
                "connections.a".to_owned(),
                table("connections"),
                "\n# above a\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n\n[connections.a.settings]\nx = 1\n",
            ),
            (
                "connections.b".to_owned(),
                table("connections"),
                "\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\n",
            ),
            ("(end of file)".to_owned(), Place::End, "# the end\n"),
        ]
    );
}

#[test]
fn crlf_text_slices_at_the_same_boundaries() {
    let text = "dbento = 1\r\n# c\r\n[connections]\r\n# top\r\n[connections.b]\r\nengine = \"sqlite\"\r\n[folders.\"team/x\"]\r\ncolor = \"red\"\r\n";
    assert_eq!(
        sliced(text),
        [
            ("dbento".to_owned(), Place::Root, "dbento = 1\r\n"),
            (
                "connections".to_owned(),
                table("connections"),
                "# c\r\n[connections]\r\n"
            ),
            (
                "connections.b".to_owned(),
                table("connections"),
                "# top\r\n[connections.b]\r\nengine = \"sqlite\"\r\n",
            ),
            (
                "folders.team/x".to_owned(),
                table("folders"),
                "[folders.\"team/x\"]\r\ncolor = \"red\"\r\n",
            ),
        ]
    );
}

#[test]
fn other_top_level_tables_are_whole_units_and_blank_trailing_text_is_not_a_unit() {
    let text = "dbento = 1\nname = \"x\"\n[future]\na = 1\n[future.deep]\nb = 2\n\n\n";
    assert_eq!(
        sliced(text),
        [
            ("dbento".to_owned(), Place::Root, "dbento = 1\n"),
            ("name".to_owned(), Place::Root, "name = \"x\"\n"),
            (
                "future".to_owned(),
                table("future"),
                "[future]\na = 1\n[future.deep]\nb = 2\n\n\n"
            ),
        ]
    );
}

#[test]
fn a_last_entry_without_a_final_newline_ends_at_the_end_of_the_text() {
    let text = "dbento = 1\n[connections.a]\nengine = \"sqlite\"";
    assert_eq!(
        sliced(text)[1],
        (
            "connections.a".to_owned(),
            table("connections"),
            "[connections.a]\nengine = \"sqlite\""
        )
    );
}

#[test]
fn empty_text_has_no_units() {
    assert_eq!(units("").expect("mergeable"), []);
}

#[test]
fn shapes_the_merge_does_not_handle_are_unmergeable() {
    for text in [
        "dbento = 1\nconnections = { a = { engine = \"sqlite\" } }\n",
        "dbento = 1\nconnections.a.engine = \"sqlite\"\n",
        "dbento = 1\n[connections]\na.engine = \"sqlite\"\n",
        "dbento = 1\n[connections]\na = { engine = \"sqlite\" }\n",
        "dbento = 1\nfolders = 1\n",
        "dbento = 1\n[[connections]]\nengine = \"sqlite\"\n",
        "dbento = 1\n[connections.a]\nengine = \"x\"\n[folders.f]\n[connections.a.settings]\nk = 1\n",
        "dbento = 1\n[connections.a\n",
    ] {
        assert_eq!(units(text), Err(TomlMergeConflict::Unmergeable), "{text:?}");
    }
}

#[test]
fn a_quoted_root_key_named_like_an_entry_does_not_fold_into_it() {
    let text = "dbento = 1\n\"connections.a\" = 1\n[connections.a]\npath = \"x\"\n";
    assert_eq!(units(text), Err(TomlMergeConflict::Unmergeable));
}

#[test]
fn identity_ignores_line_endings_and_trailing_blank_lines() {
    assert_eq!(identity("[a]\r\nx = 1\r\n\r\n"), "[a]\nx = 1");
    assert_eq!(identity("[a]\nx = 1"), "[a]\nx = 1");
    assert_ne!(identity("# one\n[a]\n"), identity("# two\n[a]\n"));
}

const BASE: &str = "dbento = 1\n\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n";
const B: &str = "\n# added by a teammate\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\n";
const C: &str = "\n# added by me\n[connections.c]\nengine = \"sqlite\"\npath = \"c.db\"\n";
const D: &str = "\n[connections.d]\nengine = \"sqlite\"\npath = \"d.db\"\n";

fn merged(base: &str, ours: &str, theirs: &str) -> String {
    merge_workspace_text(base, ours, theirs).expect("merges")
}

fn conflict(base: &str, ours: &str, theirs: &str) -> Vec<String> {
    match merge_workspace_text(base, ours, theirs) {
        Err(TomlMergeConflict::Units(names)) => names,
        other => panic!("not a unit conflict: {other:?}"),
    }
}

#[test]
fn disjoint_additions_keep_both_theirs_first_with_comments() {
    let ours = format!("{BASE}{C}{D}");
    let theirs = format!("{BASE}{B}");
    assert_eq!(merged(BASE, &ours, &theirs), format!("{BASE}{B}{C}{D}"));
}

#[test]
fn my_edit_lands_in_place_beside_their_addition() {
    let ours = BASE.replace("a.db", "mine.db");
    let theirs = format!("{BASE}{B}");
    assert_eq!(merged(BASE, &ours, &theirs), format!("{ours}{B}"));
}

#[test]
fn their_edit_is_kept_beside_my_addition() {
    let ours = format!("{BASE}{C}");
    let theirs = BASE.replace("a.db", "theirs.db");
    assert_eq!(merged(BASE, &ours, &theirs), format!("{theirs}{C}"));
}

#[test]
fn the_same_entry_edited_differently_conflicts_by_name() {
    let ours = BASE.replace("a.db", "mine.db");
    let theirs = BASE.replace("a.db", "theirs.db");
    assert_eq!(conflict(BASE, &ours, &theirs), ["connections.a"]);
}

#[test]
fn two_fields_of_one_entry_edited_on_each_side_conflict() {
    let ours = BASE.replace("engine = \"sqlite\"", "engine = \"duckdb\"");
    let theirs = BASE.replace("a.db", "theirs.db");
    assert_eq!(conflict(BASE, &ours, &theirs), ["connections.a"]);
}

#[test]
fn identical_edits_on_both_sides_merge() {
    let ours = BASE.replace("a.db", "same.db");
    let theirs = format!("{ours}{B}");
    assert_eq!(merged(BASE, &ours, &theirs), theirs);
}

#[test]
fn delete_against_edit_conflicts_either_way() {
    let deleted = "dbento = 1\n";
    let edited = BASE.replace("a.db", "edited.db");
    assert_eq!(conflict(BASE, deleted, &edited), ["connections.a"]);
    assert_eq!(conflict(BASE, &edited, deleted), ["connections.a"]);
}

#[test]
fn my_delete_of_an_entry_they_left_alone_removes_it() {
    let base = format!("{BASE}{D}");
    let theirs = format!("{base}{B}");
    assert_eq!(merged(&base, BASE, &theirs), format!("{BASE}{B}"));
}

#[test]
fn their_delete_of_an_entry_i_left_alone_stays_deleted() {
    let base = format!("{BASE}{D}");
    let ours = format!("{base}{C}");
    assert_eq!(merged(&base, &ours, BASE), format!("{BASE}{C}"));
}

#[test]
fn a_rename_is_a_delete_and_an_add() {
    let ours = BASE.replace("[connections.a]", "[connections.z]");
    let theirs = format!("{BASE}{B}");
    assert_eq!(
        merged(BASE, &ours, &theirs),
        format!("dbento = 1\n{B}\n[connections.z]\nengine = \"sqlite\"\npath = \"a.db\"\n")
    );
}

#[test]
fn both_adding_one_name_conflicts_unless_the_text_matches() {
    let ours = format!("{BASE}{C}");
    let theirs = format!("{BASE}{}", C.replace("c.db", "other.db"));
    assert_eq!(conflict(BASE, &ours, &theirs), ["connections.c"]);
    let same = format!("{BASE}{B}{C}");
    assert_eq!(merged(BASE, &ours, &same), same);
}

#[test]
fn folder_entries_merge_like_connections() {
    let base = format!("{BASE}\n[folders.prod]\ncolor = \"red\"\n");
    let ours = base.replace("\"red\"", "\"blue\"");
    let theirs = format!("{base}\n[folders.dev]\ncolor = \"green\"\n");
    assert_eq!(
        merged(&base, &ours, &theirs),
        format!("{BASE}\n[folders.prod]\ncolor = \"blue\"\n\n[folders.dev]\ncolor = \"green\"\n")
    );
}

#[test]
fn a_top_level_key_is_one_unit() {
    let base = "dbento = 1\nname = \"x\"\n";
    let ours = "dbento = 1\nname = \"mine\"\n";
    let theirs = format!("{base}{B}");
    assert_eq!(merged(base, ours, &theirs), format!("{ours}{B}"));
    assert_eq!(
        conflict(base, ours, "dbento = 1\nname = \"theirs\"\n"),
        ["name"]
    );
}

#[test]
fn the_old_single_key_seed_still_conflicts_on_name() {
    assert_eq!(
        conflict(
            "name = \"seed\"\n",
            "name = \"mine\"\n",
            "name = \"theirs\"\n"
        ),
        ["name"]
    );
}

#[test]
fn every_conflicting_unit_is_named_in_order() {
    let base = "dbento = 1\nname = \"x\"\n\n[connections.a]\npath = \"a\"\n";
    let ours = "dbento = 1\nname = \"m\"\n\n[connections.a]\npath = \"m\"\n";
    let theirs = "dbento = 1\nname = \"t\"\n\n[connections.a]\npath = \"t\"\n";
    assert_eq!(conflict(base, ours, theirs), ["connections.a", "name"]);
}

#[test]
fn output_follows_their_line_endings() {
    let crlf = |s: &str| s.replace('\n', "\r\n");
    let ours = format!("{BASE}{C}");
    let theirs = crlf(&format!("{BASE}{B}"));
    assert_eq!(merged(BASE, &ours, &theirs), crlf(&format!("{BASE}{B}{C}")));
    let ours = crlf(&format!("{BASE}{C}"));
    let theirs = format!("{BASE}{B}");
    assert_eq!(merged(BASE, &ours, &theirs), format!("{BASE}{B}{C}"));
}

#[test]
fn header_and_trailing_comments_survive() {
    let base =
        "dbento = 1\n\n# prod db\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n# end\n";
    let ours = format!("{}{C}# end\n", base.strip_suffix("# end\n").unwrap());
    let theirs = base.replace("# prod db", "# production db");
    assert_eq!(
        merged(base, &ours, &theirs),
        format!("{}{C}# end\n", theirs.strip_suffix("# end\n").unwrap())
    );
}

#[test]
fn appending_after_a_last_entry_without_newline_does_not_edit_it() {
    let base = "dbento = 1\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"";
    let ours = format!("{base}\n{C}");
    let theirs = base.replace("a.db", "t.db");
    assert_eq!(merged(base, &ours, &theirs), format!("{theirs}\n{C}"));
}

#[test]
fn a_first_entry_goes_at_the_end_when_theirs_has_none() {
    let base = "dbento = 1\nname = \"x\"\n";
    let ours = format!("{base}{C}");
    let theirs = "dbento = 1\nname = \"t\"\n";
    assert_eq!(merged(base, &ours, theirs), format!("{theirs}{C}"));
}

#[test]
fn an_unparsable_or_inline_side_is_unmergeable() {
    let inline = "dbento = 1\nconnections = { a = { engine = \"sqlite\" } }\n";
    for (ours, theirs) in [("dbento = [\n", BASE), (BASE, inline)] {
        assert_eq!(
            merge_workspace_text(BASE, ours, theirs),
            Err(TomlMergeConflict::Unmergeable)
        );
    }
}
