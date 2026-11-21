use super::{Place, TomlMergeConflict, Unit, units};

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
