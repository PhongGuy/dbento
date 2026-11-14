use dbento_app::app::AppPaths;
use dbento_app::persist::{load_json, save_json};
use dbento_app::settings::Settings;
use dbento_app::theme::Appearance;
use dbento_pilot::seed_appearance;

fn saved(dir: &std::path::Path) -> Settings {
    load_json(&AppPaths::in_dir(dir).settings).value
}
#[test]
fn the_variable_seeds_a_missing_settings_file_only() {
    let d = tempfile::tempdir().unwrap();
    seed_appearance(d.path(), None).unwrap();
    assert!(!AppPaths::in_dir(d.path()).settings.exists());
    for (v, want) in [
        ("dark", Appearance::Dark),
        ("light", Appearance::Light),
        ("system", Appearance::System),
    ] {
        let d = tempfile::tempdir().unwrap();
        seed_appearance(d.path(), Some(v)).unwrap();
        assert_eq!(saved(d.path()).appearance, want, "{v}");
    }
    let mut kept = Settings::default();
    kept.appearance = Appearance::Dark;
    save_json(&AppPaths::in_dir(d.path()).settings, &kept).unwrap();
    seed_appearance(d.path(), Some("light")).unwrap();
    assert_eq!(
        saved(d.path()).appearance,
        Appearance::Dark,
        "a kept settings file wins"
    );
    let e = seed_appearance(d.path(), Some("purple")).unwrap_err();
    assert!(e.to_string().contains("DBENTO_PILOT_THEME"), "{e}");
}
