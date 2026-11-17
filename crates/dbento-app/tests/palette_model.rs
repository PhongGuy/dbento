//! The palette's groups from a sources snapshot and a query.
use dbento_app::keymap::{self, NEW_TAB_KEYS, RELOAD_WORKSPACE_KEYS};
use dbento_app::palette::{
    CatalogStatus, ConnectionSource, Entry, Mode, PaletteCommand, Section, Sources, TableRef,
    parse_query, prefix_hints, push_recent, sections,
};
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::{SharedString, TestAppContext};

fn t(conn: &str, name: &str) -> TableRef {
    TableRef {
        connection: conn.into(),
        rel: RelationRef::new("main", name),
        kind: RelationKind::Table,
    }
}
fn conn(name: &str, status: CatalogStatus) -> ConnectionSource {
    ConnectionSource {
        name: name.into(),
        status,
    }
}
fn src() -> Sources {
    Sources {
        tables: vec![t("lite", "orders"), t("lite", "customers")],
        queries: vec!["queries/a.sql".into()],
        recent: vec![t("lite", "customers")],
        connections: vec![
            conn("lite", CatalogStatus::Ready),
            conn("nopg", CatalogStatus::Idle),
        ],
        shortcuts: Vec::new(),
        searched: Vec::new(),
        lone_schemas: Default::default(),
    }
}
/// [`src`] with the table's shortcut list, which needs an app to read.
fn src_with_shortcuts(cx: &mut TestAppContext) -> Sources {
    Sources {
        shortcuts: cx.update(|cx| keymap::shortcuts(cx)),
        ..src()
    }
}
fn titles(s: &[Section]) -> Vec<&'static str> {
    s.iter().map(|s| s.title).collect()
}
fn commands() -> Vec<Entry> {
    PaletteCommand::ALL
        .iter()
        .map(|c| Entry::Command(*c))
        .collect()
}

#[test]
fn an_empty_query_shows_every_group_in_order() {
    let s = sections(&src(), "");
    assert_eq!(
        titles(&s),
        [
            "Tables and views",
            "Saved queries",
            "Recent tables",
            "Connections",
            "Commands",
            "Tables not loaded"
        ]
    );
    assert_eq!(
        s[0].entries,
        [
            Entry::Table(t("lite", "orders")),
            Entry::Table(t("lite", "customers"))
        ]
    );
    assert_eq!(s[1].entries, [Entry::Query(0)]);
    assert_eq!(s[2].entries, [Entry::Recent(t("lite", "customers"))]);
    assert_eq!(
        s[3].entries,
        [
            Entry::Connection("lite".into()),
            Entry::Connection("nopg".into())
        ]
    );
    assert_eq!(s[4].entries, commands());
    assert_eq!(s[5].entries, [Entry::SearchIn("nopg".into())]);
}

#[test]
fn a_query_ranks_each_group_and_drops_empty_ones() {
    let s = sections(&src(), "ord");
    assert_eq!(
        titles(&s),
        ["Tables and views", "Commands", "Tables not loaded"]
    );
    assert_eq!(s[0].entries, [Entry::Table(t("lite", "orders"))]);
    assert_eq!(
        s[1].entries,
        [
            Entry::Command(PaletteCommand::CheckForUpdates),
            Entry::Command(PaletteCommand::KeyboardShortcuts)
        ]
    );
    assert_eq!(s[2].entries, [Entry::SearchIn("nopg".into())]);
}

#[gpui_kit::test]
fn each_prefix_shows_one_group(cx: &mut TestAppContext) {
    let s = sections(&src(), "> ");
    assert_eq!(titles(&s), ["Commands"]);
    assert_eq!(s[0].entries, commands());
    assert_eq!(
        sections(&src(), "> reload")[0].entries,
        [Entry::Command(PaletteCommand::ReloadWorkspace)]
    );
    assert_eq!(sections(&src(), "#")[0].entries, [Entry::Query(0)]);
    assert_eq!(titles(&sections(&src(), "#")), ["Saved queries"]);
    // One row per label, its chords in table order.
    let mut shortcuts: Vec<Entry> = Vec::new();
    for s in cx.update(|cx| keymap::shortcuts(cx)) {
        match shortcuts
            .iter_mut()
            .find(|e| matches!(e, Entry::Shortcut { label, .. } if *label == s.label))
        {
            Some(Entry::Shortcut { keys, .. }) => keys.push(s.keys),
            _ => shortcuts.push(Entry::Shortcut {
                label: s.label,
                keys: vec![s.keys],
            }),
        }
    }
    let q = sections(&src_with_shortcuts(cx), "?");
    assert_eq!(titles(&q), ["Keyboard shortcuts"]);
    assert_eq!(q[0].entries, shortcuts);
}

#[test]
fn a_connection_not_ready_gets_its_status_row() {
    let mut s = src();
    s.connections = vec![
        conn("a", CatalogStatus::Loading),
        conn("b", CatalogStatus::Failed("refused".into())),
        conn("c", CatalogStatus::Idle),
        conn("d", CatalogStatus::Ready),
    ];
    s.tables.clear();
    let all = sections(&s, "zzz");
    assert_eq!(titles(&all), ["Tables not loaded"]);
    let g = &all[0];
    assert_eq!(
        g.entries,
        [
            Entry::Loading("a".into()),
            Entry::Failed {
                connection: "b".into(),
                message: "refused".into()
            },
            Entry::SearchIn("c".into()),
        ]
    );
    let labels: Vec<_> = g.entries.iter().map(|e| e.label(&s)).collect();
    assert_eq!(
        labels,
        [
            "Loading tables in a\u{2026}",
            "Retry b \u{b7} refused",
            "Search tables in c"
        ]
    );
}

#[test]
fn all_mode_caps_tables_and_queries_but_a_single_mode_does_not() {
    let mut s = src();
    s.tables = (0..60).map(|i| t("lite", &format!("t{i}"))).collect();
    s.queries = (0..30).map(|i| format!("queries/q{i}.sql")).collect();
    let all = sections(&s, "");
    assert_eq!(all[0].entries.len(), 50);
    assert_eq!(all[1].entries.len(), 20);
    assert_eq!(sections(&s, "#")[0].entries.len(), 30);
}

#[test]
fn a_lone_schema_drops_from_table_rows_but_ranking_keeps_it() {
    let mut s = src();
    s.lone_schemas = [("lite".to_owned(), "main".to_owned())].into();
    assert_eq!(
        Entry::Table(t("lite", "orders")).label(&s),
        "orders \u{b7} lite"
    );
    assert_eq!(
        Entry::Recent(t("lite", "orders")).label(&s),
        "orders \u{b7} lite"
    );
    let found = sections(&s, "main.ord");
    assert_eq!(found[0].entries[0], Entry::Table(t("lite", "orders")));
    // A table outside the lone schema, or of a connection with several, keeps its prefix.
    let other = TableRef {
        rel: RelationRef::new("temp", "scratch"),
        ..t("lite", "x")
    };
    assert_eq!(Entry::Table(other).label(&s), "temp.scratch \u{b7} lite");
    assert_eq!(
        Entry::Table(t("pg", "orders")).label(&s),
        "main.orders \u{b7} pg"
    );
}

#[gpui_kit::test]
fn labels_and_keys(cx: &mut TestAppContext) {
    let s = src();
    assert_eq!(
        Entry::Table(t("lite", "orders")).label(&s),
        "main.orders \u{b7} lite"
    );
    assert_eq!(
        Entry::Recent(t("lite", "orders")).label(&s),
        "main.orders \u{b7} lite"
    );
    assert_eq!(Entry::Query(0).label(&s), "a.sql");
    assert_eq!(Entry::Connection("lite".into()).label(&s), "lite");
    assert_eq!(
        Entry::Command(PaletteCommand::ReloadWorkspace).label(&s),
        "Reload dbento.toml"
    );
    let keys = |cx: &mut TestAppContext, e: Entry| cx.update(|cx| e.keys(cx));
    assert_eq!(
        keys(cx, Entry::Command(PaletteCommand::NewTab)),
        [NEW_TAB_KEYS]
    );
    assert_eq!(
        keys(cx, Entry::Command(PaletteCommand::ReloadWorkspace)),
        [RELOAD_WORKSPACE_KEYS]
    );
    assert_eq!(
        keys(cx, Entry::Command(PaletteCommand::KeyboardShortcuts)),
        Vec::<SharedString>::new()
    );
    assert!(keys(cx, Entry::Table(t("lite", "orders"))).is_empty());
}

#[gpui_kit::test]
fn a_label_with_several_chords_is_one_shortcut_row(cx: &mut TestAppContext) {
    let chords: Vec<_> = cx
        .update(|cx| keymap::shortcuts(cx))
        .into_iter()
        .filter(|s| s.label == "Format SQL")
        .map(|s| s.keys)
        .collect();
    assert!(chords.len() > 1, "precondition: several chords");
    let rows: Vec<_> = sections(&src_with_shortcuts(cx), "?")[0]
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e,
                Entry::Shortcut {
                    label: "Format SQL",
                    ..
                }
            )
        })
        .cloned()
        .collect();
    assert_eq!(
        rows,
        [Entry::Shortcut {
            label: "Format SQL",
            keys: chords
        }]
    );
    assert_eq!(rows[0].label(&src()), "Format SQL");
}

#[test]
fn recent_tables_move_to_the_front_and_stop_at_twenty() {
    let mut list = Vec::new();
    for i in 0..25 {
        push_recent(&mut list, t("lite", &format!("t{i}")));
    }
    assert_eq!(list.len(), 20);
    assert_eq!(list[0], t("lite", "t24"));
    push_recent(&mut list, t("lite", "t10"));
    assert_eq!(list[0], t("lite", "t10"));
    assert_eq!(list.iter().filter(|x| **x == t("lite", "t10")).count(), 1);
    assert_eq!(list.len(), 20);
}

#[test]
fn a_saved_query_row_drops_the_queries_folder_but_keeps_deeper_ones() {
    let mut s = src();
    s.queries = vec![
        "queries/reports/monthly.sql".into(),
        "queries/a.sql".into(),
        "loose.sql".into(),
    ];
    let labels: Vec<_> = (0..3).map(|i| Entry::Query(i).label(&s)).collect();
    assert_eq!(labels, ["reports/monthly.sql", "a.sql", "loose.sql"]);
}

#[test]
fn saved_query_search_still_matches_the_full_path() {
    let mut s = src();
    s.queries = vec!["queries/reports/monthly.sql".into(), "loose.sql".into()];
    // "que" only matches inside the hidden "queries/" part.
    assert_eq!(sections(&s, "# que")[0].entries, [Entry::Query(0)]);
}

#[test]
fn a_searched_connection_without_a_match_says_so() {
    let mut s = src();
    s.searched = vec!["lite".into()];
    let all = sections(&s, "zzz");
    assert_eq!(all[0].title, "Tables and views");
    let row = Entry::NoMatch {
        connection: "lite".into(),
        query: "zzz".into(),
    };
    assert_eq!(all[0].entries, std::slice::from_ref(&row));
    assert_eq!(row.label(&s), "No tables in lite match 'zzz'");
}

#[test]
fn a_searched_connection_with_a_match_shows_no_message() {
    let mut s = src();
    s.searched = vec!["lite".into()];
    assert_eq!(
        sections(&s, "ord")[0].entries,
        [Entry::Table(t("lite", "orders"))]
    );
}

#[test]
fn a_connection_never_searched_from_the_palette_shows_no_message() {
    let s = src();
    let all = sections(&s, "zzz");
    assert!(
        all.iter()
            .flat_map(|g| &g.entries)
            .all(|e| !matches!(e, Entry::NoMatch { .. }))
    );
}

#[test]
fn only_the_no_match_row_is_not_selectable() {
    let row = Entry::NoMatch {
        connection: "lite".into(),
        query: "x".into(),
    };
    assert!(!row.selectable());
    assert!(Entry::SearchIn("lite".into()).selectable());
    assert!(Entry::Query(0).selectable());
}

#[test]
fn the_footer_lists_each_prefix_only_without_one() {
    let hints = prefix_hints("");
    assert_eq!(
        hints,
        [
            (">", "commands"),
            ("#", "saved queries"),
            ("?", "shortcuts")
        ]
    );
    for (prefix, _) in hints {
        assert_ne!(
            parse_query(prefix).0,
            Mode::All,
            "{prefix} is a real prefix"
        );
    }
    assert_eq!(prefix_hints("orders").len(), 3);
    assert!(prefix_hints("> ").is_empty());
    assert!(prefix_hints("#").is_empty());
    assert!(prefix_hints("? x").is_empty());
}
