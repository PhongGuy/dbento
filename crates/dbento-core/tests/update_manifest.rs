//! The manifest rules applied once the signature has verified.
#![cfg(feature = "update")]

use dbento_core::update::{
    CheckError, Format, Product, UpdateConfig, build_config_with, evaluate, target_id,
};

const PAGE: &str = "https://example.com/download";
const SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn cfg(current: &str) -> UpdateConfig {
    build_config_with(
        Product::Cli,
        Some("https://example.com/latest.json"),
        Some(PAGE),
        "a-key\n",
        current,
    )
    .unwrap()
}

fn cli() -> String {
    target_id(Product::Cli).unwrap()
}

// `notes`, `extra` and `more` are unknown fields, which the rules ignore.
fn manifest(version: &str, target: &str, format: &str, url: &str, sha: &str) -> String {
    format!(
        r#"{{"version":"{version}","pub_date":"2026-11-01T12:00:00Z","notes":"Fixes.","extra":1,
          "targets":{{"{target}":{{"url":"{url}","format":"{format}","size":3,"sha256":"{sha}","more":true}}}}}}"#
    )
}

fn own(version: &str) -> String {
    manifest(version, &cli(), "binary", "https://example.com/dbento", SHA)
}

#[test]
fn a_newer_stable_release_is_offered_with_its_entry() {
    let a = evaluate(own("1.2.0").as_bytes(), &cfg("1.1.9"))
        .unwrap()
        .expect("an update");
    assert_eq!(a.version(), "1.2.0");
    assert_eq!(a.pub_date(), Some("2026-11-01T12:00:00Z"));
    assert_eq!(a.format(), Format::Binary);
    assert_eq!(
        (a.url(), a.sha256(), a.size()),
        ("https://example.com/dbento", SHA, 3)
    );
}

#[test]
fn only_a_strictly_greater_version_is_offered() {
    let table = [
        ("1.0.0", "1.0.0", false),
        ("0.9.9", "1.0.0", false),
        ("1.0.0+rebuild", "1.0.0", false),
        ("1.0.1", "1.0.0", true),
        ("2.0.0", "1.9.9", true),
        ("1.0.0", "1.0.0-dev", true),
    ];
    for (remote, current, offered) in table {
        let got = evaluate(own(remote).as_bytes(), &cfg(current)).unwrap();
        assert_eq!(got.is_some(), offered, "{remote} against {current}");
    }
}

#[test]
fn a_pre_release_manifest_is_invalid() {
    for v in ["1.2.0-rc.1", "9.0.0-beta"] {
        let err = evaluate(own(v).as_bytes(), &cfg("0.1.0")).unwrap_err();
        assert!(
            matches!(err, CheckError::ManifestInvalid(_)),
            "{v}: {err:?}"
        );
    }
}

#[test]
fn a_missing_target_is_no_entry() {
    let m = manifest(
        "2.0.0",
        "cli-plan9-mips",
        "binary",
        "https://example.com/a",
        SHA,
    );
    assert_eq!(
        evaluate(m.as_bytes(), &cfg("0.1.0")).unwrap_err(),
        CheckError::NoEntryForTarget(cli())
    );
}

#[test]
fn malformed_documents_and_entries_are_invalid() {
    let id = cli();
    let bad = [
        "not json".to_string(),
        r#"{"targets":{}}"#.to_string(),
        format!(
            r#"{{"version":"2.0.0","targets":{{"{id}":{{"url":"https://e.com/a","format":"binary","sha256":"{SHA}"}}}}}}"#
        ),
        manifest("two", &id, "binary", "https://example.com/a", SHA),
        manifest("2.0.0", &id, "dmg", "https://example.com/a", SHA),
        manifest("2.0.0", &id, "binary", "http://example.com/a", SHA),
        manifest("2.0.0", &id, "binary", "https://example.com/a", "abc"),
        manifest(
            "2.0.0",
            &id,
            "binary",
            "https://example.com/a",
            &SHA.to_uppercase(),
        ),
    ];
    for m in bad {
        let got = evaluate(m.as_bytes(), &cfg("0.1.0"));
        assert!(
            matches!(got, Err(CheckError::ManifestInvalid(_))),
            "{m}: {got:?}"
        );
    }
}

#[test]
fn every_format_name_maps() {
    for (name, format) in [
        ("appimage", Format::AppImage),
        ("app", Format::App),
        ("nsis", Format::Nsis),
        ("binary", Format::Binary),
    ] {
        let m = manifest("2.0.0", &cli(), name, "https://example.com/a", SHA);
        assert_eq!(
            evaluate(m.as_bytes(), &cfg("0.1.0"))
                .unwrap()
                .unwrap()
                .format(),
            format
        );
    }
}

#[test]
fn loopback_http_artifacts_are_allowed_for_local_testing() {
    let m = manifest("2.0.0", &cli(), "binary", "http://127.0.0.1:9/a", SHA);
    assert!(evaluate(m.as_bytes(), &cfg("0.1.0")).unwrap().is_some());
}

#[test]
fn a_manifest_with_notes_asset_is_still_offered() {
    let m = own("1.2.0").replacen(
        '{',
        &format!(r#"{{"notes_asset":{{"url":"https://example.com/notes.json","size":10,"sha256":"{SHA}"}},"#),
        1,
    );
    assert!(evaluate(m.as_bytes(), &cfg("1.1.9")).unwrap().is_some());
    for bad in [
        r#""notes_asset":7,"#,
        r#""notes_asset":{"url":"ftp://x","size":1,"sha256":"AB"},"#,
        r#""notes_asset":null,"#,
    ] {
        let m = own("1.2.0").replacen('{', &format!("{{{bad}"), 1);
        assert!(
            evaluate(m.as_bytes(), &cfg("1.1.9")).unwrap().is_some(),
            "{bad}"
        );
    }
}
