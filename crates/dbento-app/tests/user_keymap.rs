//! keymap.json: overrides, unbinds, per-entry problems, conflicts and the broken-file paths.
use dbento_app::keymap::{self, NEW_TAB_KEYS};
use dbento_app::user_keymap::{KeymapProblem, Overrides, conflicts, effective, load, parse};

fn ov(entries: &[(&str, &[&str])]) -> Overrides {
    Overrides(
        entries
            .iter()
            .map(|(id, k)| (id.to_string(), k.iter().map(|s| s.to_string()).collect()))
            .collect(),
    )
}
fn rows_of<'a>(
    rows: &'a [dbento_app::user_keymap::Row],
    id: &str,
) -> Vec<(&'a str, Option<&'static str>)> {
    rows.iter()
        .filter(|r| r.id == id)
        .map(|r| (r.keys.as_ref(), r.context))
        .collect()
}

#[test]
fn an_empty_object_overrides_nothing() {
    let p = parse("{}").unwrap();
    assert_eq!((p.overrides, p.problems), (Overrides::default(), vec![]));
    assert_eq!(
        effective(&Overrides::default()).len(),
        keymap::bindings().len()
    );
}

#[test]
fn an_override_replaces_the_keys_in_each_default_context() {
    let p = parse(r#"{"query.run_statement": ["ctrl-r", "f5"]}"#).unwrap();
    assert!(p.problems.is_empty());
    let rows = effective(&p.overrides);
    assert_eq!(
        rows_of(&rows, "query.run_statement"),
        vec![
            ("ctrl-r", Some("QueryTab")),
            ("f5", Some("QueryTab")),
            ("ctrl-r", Some("QueryTab > Input")),
            ("f5", Some("QueryTab > Input")),
        ]
    );
    // Placed where the id's first default row was, so later rows still win over it.
    let first = rows
        .iter()
        .position(|r| r.id == "query.run_statement")
        .unwrap();
    let table = keymap::bindings();
    assert_eq!(
        first,
        table
            .iter()
            .position(|b| b.id == "query.run_statement")
            .unwrap()
    );
    assert_eq!(rows.len(), table.len() + 2);
}

#[test]
fn an_empty_list_unbinds_and_a_missing_id_keeps_its_defaults() {
    let p = parse(r#"{"view.toggle_history": []}"#).unwrap();
    let rows = effective(&p.overrides);
    assert!(rows_of(&rows, "view.toggle_history").is_empty());
    assert_eq!(rows_of(&rows, "tabs.new"), vec![(NEW_TAB_KEYS, None)]);
}

#[test]
fn chords_are_allowed_and_duplicates_collapse() {
    let p = parse(r#"{"tabs.new": ["ctrl-k n", "ctrl-k  n", "ctrl-k n"]}"#).unwrap();
    assert!(p.problems.is_empty(), "{:?}", p.problems);
    assert_eq!(p.overrides.get("tabs.new").unwrap().len(), 1);
}

#[test]
fn each_bad_entry_is_skipped_and_listed_and_the_rest_apply() {
    let p = parse(
        r#"{
        "nope.nothing": ["ctrl-1"],
        "tabs.close": "ctrl-w",
        "tabs.recent": ["ctrl-tab", 3],
        "query.cancel": [""],
        "query.run_all": ["ctrl-shift-"],
        "query.open_saved": ["ctrl-a-b"],
        "tabs.new": ["ctrl-alt-n"]
    }"#,
    )
    .unwrap();
    assert_eq!(p.overrides, ov(&[("tabs.new", &["ctrl-alt-n"])]));
    let has = |want: &dyn Fn(&KeymapProblem) -> bool| p.problems.iter().any(want);
    assert!(has(
        &|x| *x == KeymapProblem::UnknownId("nope.nothing".into())
    ));
    assert!(has(&|x| *x == KeymapProblem::NotAList("tabs.close".into())));
    assert!(has(&|x| *x == KeymapProblem::NotAList("tabs.recent".into())));
    assert!(has(
        &|x| matches!(x, KeymapProblem::BadKeys { id, .. } if id == "query.cancel")
    ));
    assert!(has(
        &|x| matches!(x, KeymapProblem::BadKeys { id, keys, .. } if id == "query.run_all" && keys == "ctrl-shift-")
    ));
    assert!(has(
        &|x| matches!(x, KeymapProblem::BadKeys { id, keys, .. } if id == "query.open_saved" && keys == "ctrl-a-b")
    ));
    assert_eq!(p.problems.len(), 6);
    for x in &p.problems {
        assert!(!x.message().is_empty());
    }
}

#[test]
fn not_json_or_not_an_object_is_unparseable() {
    assert!(parse("{ nope").unwrap_err().contains("line"));
    assert!(parse("[]").is_err());
}

#[test]
fn a_conflict_is_listed_only_when_an_overridden_id_takes_part() {
    // tabs.new is global and query.run_all lives in QueryTab: both can be active.
    let o = ov(&[("tabs.new", &[keymap::RUN_ALL_KEYS])]);
    let found = conflicts(&effective(&o), &o);
    assert_eq!(
        found,
        vec![KeymapProblem::Conflict {
            keys: keymap::RUN_ALL_KEYS.into(),
            ids: vec!["query.run_all".into(), "tabs.new".into()],
        }]
    );
    // The defaults share keys on purpose (SaveEdits and SaveConnection); none is reported.
    assert!(conflicts(&effective(&Overrides::default()), &Overrides::default()).is_empty());
    // Disjoint contexts never conflict: DataTable's edit key bound in the Explorer too.
    let o = ov(&[("explorer.open_selected", &["f2"])]);
    assert!(conflicts(&effective(&o), &o).is_empty());
}

#[test]
fn a_missing_file_is_empty_and_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let l = load(&dir.path().join("keymap.json"), true);
    assert_eq!(
        (l.overrides, l.problems, l.warning),
        (Some(Overrides::default()), vec![], None)
    );
}

#[test]
fn at_startup_a_broken_file_moves_aside_with_one_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.json");
    std::fs::write(&path, "{ secret-looking text").unwrap();
    let l = load(&path, true);
    assert!(l.overrides.is_none());
    assert!(matches!(
        l.problems.as_slice(),
        [KeymapProblem::Unparseable { .. }]
    ));
    let warning = l.warning.unwrap();
    assert!(warning.contains("keymap.json") && !warning.contains("secret"));
    assert!(!path.exists() && dir.path().join("keymap.json.bad").exists());
}

#[test]
fn at_startup_an_unreadable_file_warns_once_and_stays() {
    // A directory in the file's place cannot be read but is not missing.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.json");
    std::fs::create_dir(&path).unwrap();
    let l = load(&path, true);
    assert!(l.overrides.is_none());
    assert!(matches!(
        l.problems.as_slice(),
        [KeymapProblem::Unparseable { .. }]
    ));
    assert!(l.warning.unwrap().contains("keymap.json"));
    assert!(path.is_dir() && !dir.path().join("keymap.json.bad").exists());
    assert!(load(&path, false).warning.is_none(), "reload stays quiet");
}

#[test]
fn on_reload_a_broken_file_stays_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.json");
    std::fs::write(&path, "{ \"tabs.new\": [").unwrap();
    let l = load(&path, false);
    assert!(l.overrides.is_none() && l.warning.is_none());
    assert!(matches!(
        l.problems.as_slice(),
        [KeymapProblem::Unparseable { .. }]
    ));
    assert!(path.exists() && !dir.path().join("keymap.json.bad").exists());
}

#[test]
fn load_lists_entry_problems_then_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.json");
    let json = format!(
        r#"{{"nope.x": ["f1"], "tabs.new": ["{}"]}}"#,
        keymap::RUN_ALL_KEYS
    );
    std::fs::write(&path, json).unwrap();
    let l = load(&path, true);
    assert_eq!(l.overrides.unwrap().0.len(), 1);
    assert!(matches!(
        l.problems.as_slice(),
        [KeymapProblem::UnknownId(_), KeymapProblem::Conflict { .. }]
    ));
}
