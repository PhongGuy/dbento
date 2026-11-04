use dbento_app::settings::{Density, FontChoice, Settings, load};
use dbento_app::theme::Appearance;
use serde_json::json;
use std::path::{Path, PathBuf};

fn font(family: &str, size: u8) -> FontChoice {
    FontChoice {
        family: family.into(),
        size,
    }
}
fn file(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("settings.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn the_defaults_are_the_bundled_fonts_and_system() {
    let s = Settings::default();
    assert_eq!(s.appearance, Appearance::System);
    assert_eq!(s.ui_font, font("Inter", 13));
    assert_eq!(s.editor_font, font("JetBrains Mono", 12));
    assert_eq!(s.grid_font, font("JetBrains Mono", 12));
    assert_eq!(s.density, Density::Default);
    assert!(s.extra().is_empty());
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        json!({
            "appearance": "system",
            "ui_font": { "family": "Inter", "size": 13 },
            "editor_font": { "family": "JetBrains Mono", "size": 12 },
            "grid_font": { "family": "JetBrains Mono", "size": 12 },
            "density": "default"
        })
    );
}

#[test]
fn a_missing_file_takes_the_seed_appearance_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let l = load(&path, Appearance::Dark);
    assert_eq!(l.settings.appearance, Appearance::Dark);
    assert_eq!(l.settings.ui_font, font("Inter", 13));
    assert_eq!((l.recovered, l.writable), (None, true));
    assert!(!path.exists());
}

#[test]
fn an_existing_partial_file_ignores_the_seed() {
    let dir = tempfile::tempdir().unwrap();
    let l = load(
        &file(dir.path(), r#"{"density":"compact"}"#),
        Appearance::Dark,
    );
    assert_eq!(
        l.settings.appearance,
        Appearance::System,
        "the seed is for a missing file only"
    );
    assert_eq!(l.settings.density, Density::Compact);
    assert_eq!(l.settings.grid_font, font("JetBrains Mono", 12));
    assert_eq!((l.recovered, l.writable), (None, true));
}

#[test]
fn a_partial_font_keeps_that_fonts_own_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"{"grid_font":{"size":14},"ui_font":{"family":"No Such Font"}}"#;
    let s = load(&file(dir.path(), text), Appearance::System).settings;
    assert_eq!(s.grid_font, font("JetBrains Mono", 14));
    assert_eq!(
        s.ui_font,
        font("No Such Font", 13),
        "any family string is kept"
    );
    assert_eq!(s.editor_font, font("JetBrains Mono", 12));
}

#[test]
fn sizes_out_of_range_are_clamped_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"{"ui_font":{"size":40},"editor_font":{"size":2},"grid_font":{"size":300}}"#;
    let l = load(&file(dir.path(), text), Appearance::System);
    assert_eq!(l.recovered, None, "out of range is not an error");
    let s = l.settings;
    assert_eq!(
        (s.ui_font.size, s.editor_font.size, s.grid_font.size),
        (16, 10, 20)
    );
    let s = load(
        &file(dir.path(), r#"{"ui_font":{"size":13.6}}"#),
        Appearance::System,
    )
    .settings;
    assert_eq!(s.ui_font.size, 14, "whole pixels, rounded");
}

#[test]
fn clamped_moves_every_size_into_its_range() {
    let mut s = Settings::default();
    s.ui_font.size = 11;
    s.editor_font.size = 30;
    s.grid_font.size = 9;
    let s = s.clamped();
    assert_eq!(
        (s.ui_font.size, s.editor_font.size, s.grid_font.size),
        (12, 24, 10)
    );
}

#[test]
fn unknown_top_level_fields_survive_a_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"{"future":{"a":[1,2]},"density":"compact"}"#;
    let s = load(&file(dir.path(), text), Appearance::System).settings;
    assert_eq!(s.extra().get("future"), Some(&json!({"a":[1,2]})));
    let back = serde_json::to_value(&s).unwrap();
    assert_eq!(back["future"], json!({"a":[1,2]}));
    assert_eq!(back["density"], json!("compact"));
}

#[test]
fn a_wrong_type_sets_the_file_aside_and_uses_defaults() {
    for text in [
        r#"{"density":5}"#,
        r#"{"appearance":"purple"}"#,
        r#"{"ui_font":{"size":"big"}}"#,
        "{",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), text);
        let l = load(&path, Appearance::Dark);
        assert_eq!(
            l.settings,
            Settings::default(),
            "{text}: defaults, seed ignored"
        );
        let message = l.recovered.unwrap_or_else(|| panic!("{text}: no message"));
        assert!(message.contains("settings.json.bad"), "{message}");
        assert!(!message.contains(text), "the message never quotes the file");
        assert!(
            !path.exists() && dir.path().join("settings.json.bad").exists(),
            "{text}"
        );
        assert!(l.writable);
    }
}

#[test]
fn an_unreadable_file_is_left_alone_and_not_writable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::create_dir(&path).unwrap();
    let l = load(&path, Appearance::Dark);
    assert_eq!(l.settings, Settings::default());
    assert!(l.recovered.is_some());
    assert!(!l.writable);
    assert!(path.is_dir(), "the unreadable entry stays where it is");
}
