use dbento_app::ui::{APP_ICON, Icons};
use dbento_pilot::Dbento;
use gpui_pilot_host::PilotApp as _;

#[test]
fn the_pilot_host_serves_every_app_icon() {
    let assets = Dbento
        .assets()
        .expect("the pilot registers an asset source");
    let missing: Vec<_> = Icons::ALL
        .iter()
        .map(|i| i.path().to_string())
        .chain([APP_ICON.to_owned()])
        .filter(|p| !matches!(assets.load(p), Ok(Some(b)) if !b.is_empty()))
        .collect();
    assert!(missing.is_empty(), "blank in the pilot: {missing:?}");
}
