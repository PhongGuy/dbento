//! The release notes document: schema, sanitising, caps and which versions an install is shown.
#![cfg(feature = "update")]

use dbento_core::update::{NotesError, NotesKind, ReleaseNotes, plain_text};

fn doc(releases: &str) -> Vec<u8> {
    format!(r#"{{"schema":1,"releases":[{releases}]}}"#).into_bytes()
}

fn entry(version: &str, previous: &str) -> String {
    format!(
        r#"{{"version":"{version}","previous":{previous},"date":"2026-12-01","groups":[{{"kind":"fixes","title":"Fixes","items":[{{"text":"fix {version}","pr":7}}]}}]}}"#
    )
}

fn chain(oldest_previous: &str) -> ReleaseNotes {
    let body = [
        entry("2.0.0", r#""1.0.0""#),
        entry("3.0.0", r#""2.0.0""#),
        entry("1.0.0", oldest_previous),
    ]
    .join(",");
    ReleaseNotes::parse(&doc(&body)).unwrap()
}

fn shown(n: &ReleaseNotes, installed: &str) -> (Vec<String>, bool) {
    let b = n.between(installed);
    let versions = b
        .releases()
        .iter()
        .map(|e| e.version().to_owned())
        .collect();
    (versions, b.older_on_release_page())
}

#[test]
fn a_full_entry_parses_with_its_groups_and_items() {
    let n = ReleaseNotes::parse(&doc(&entry("3.0.0", r#""2.0.0""#))).unwrap();
    let e = &n.releases()[0];
    assert_eq!(
        (e.version(), e.previous(), e.date(), e.intro()),
        ("3.0.0", Some("2.0.0"), Some("2026-12-01"), None)
    );
    let g = &e.groups()[0];
    assert_eq!((g.kind(), g.title()), (NotesKind::Fixes, "Fixes"));
    assert_eq!(
        (g.items()[0].text(), g.items()[0].pr()),
        ("fix 3.0.0", Some(7))
    );
}

#[test]
fn escape_controls_and_bidi_are_stripped_from_every_text() {
    let n = ReleaseNotes::parse(&doc(
        r#"{"version":"1.0.0","intro":"in\u001b[2Jtro\u202e","groups":[{"kind":"fixes","title":"Fi\u2066xes\u0085",
           "items":[{"text":"\u001b[31mred\u202a\u2069x\u0085y\u007f"},{"text":"a\tb\nc\r"}]}]}"#,
    ))
    .unwrap();
    let e = &n.releases()[0];
    assert_eq!(e.intro(), Some("in[2Jtro"));
    let g = &e.groups()[0];
    assert_eq!(g.title(), "Fixes");
    let texts: Vec<&str> = g.items().iter().map(|i| i.text()).collect();
    assert_eq!(texts, ["[31mredxy", "a b c"]);
    assert_eq!(plain_text("a\u{1b}b\u{202b}c\u{2067}\td "), "abc d");
}

#[test]
fn an_item_over_500_chars_is_cut_to_500() {
    let long = "é".repeat(501);
    let n = ReleaseNotes::parse(&doc(&format!(
        r#"{{"version":"1.0.0","groups":[{{"items":[{{"text":"{long}"}},{{"text":"{}"}}]}}]}}"#,
        "x".repeat(500)
    )))
    .unwrap();
    let items = n.releases()[0].groups()[0].items();
    assert_eq!(items[0].text().chars().count(), 500);
    assert_eq!(items[1].text().chars().count(), 500);
}

#[test]
fn only_schema_1_is_accepted() {
    let two = br#"{"schema":2,"releases":[]}"#;
    assert_eq!(
        ReleaseNotes::parse(two).unwrap_err(),
        NotesError::UnsupportedSchema(2)
    );
    for bad in [
        &br#"{"releases":[]}"#[..],
        &b"not json"[..],
        &br#"{"schema":1,"releases":[{"version":"one"}]}"#[..],
    ] {
        assert!(
            matches!(ReleaseNotes::parse(bad), Err(NotesError::Invalid(_))),
            "{bad:?}"
        );
    }
    assert!(ReleaseNotes::parse(br#"{"schema":1,"releases":[]}"#).is_ok());
}

#[test]
fn previous_may_be_null_or_missing() {
    let n = ReleaseNotes::parse(&doc(
        r#"{"version":"1.0.0","previous":null},{"version":"0.9.0"}"#,
    ))
    .unwrap();
    assert_eq!(n.releases()[0].previous(), None);
    assert_eq!(n.releases()[1].previous(), None);
    assert!(n.releases()[1].groups().is_empty());
}

#[test]
fn kinds_and_titles_fall_back() {
    let n = ReleaseNotes::parse(&doc(
        r#"{"version":"1.0.0","groups":[{"kind":"shiny","title":"Shiny","items":[{"text":"a"}]},
           {"kind":"fixes","items":[{"text":"b"}]},{"items":[{"text":"c"}]},
           {"kind":"performance","title":"","items":[{"text":"d"}]},{"kind":"fixes","items":[{"text":"\u001b"}]}]}"#,
    ))
    .unwrap();
    let got: Vec<(NotesKind, &str)> = n.releases()[0]
        .groups()
        .iter()
        .map(|g| (g.kind(), g.title()))
        .collect();
    assert_eq!(
        got,
        [
            (NotesKind::Other, "Shiny"),
            (NotesKind::Fixes, "Fixes"),
            (NotesKind::Other, "Other changes"),
            (NotesKind::Performance, "Performance"),
        ]
    );
}

#[test]
fn between_shows_newer_versions_newest_first() {
    let n = chain("null");
    assert_eq!(
        shown(&n, "1.0.0"),
        (vec!["3.0.0".into(), "2.0.0".into()], false)
    );
    assert_eq!(shown(&n, "2.0.0"), (vec!["3.0.0".into()], false));
    assert_eq!(shown(&n, "2.0.0+rebuild"), (vec!["3.0.0".into()], false));
}

#[test]
fn an_install_older_than_the_list_points_at_the_release_page() {
    let first_release = chain("null");
    assert_eq!(
        shown(&first_release, "0.5.0"),
        (vec!["3.0.0".into(), "2.0.0".into(), "1.0.0".into()], false)
    );
    let cut_history = chain(r#""0.9.0""#);
    assert_eq!(
        shown(&cut_history, "0.5.0"),
        (vec!["3.0.0".into(), "2.0.0".into(), "1.0.0".into()], true)
    );
    assert!(!shown(&cut_history, "0.9.0").1);
    assert_eq!(shown(&cut_history, "3.0.0"), (vec![], true));
    assert_eq!(shown(&cut_history, "not a version").0.len(), 3);
}
