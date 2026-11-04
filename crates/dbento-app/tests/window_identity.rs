//! The window's identity matches what `make install` puts on disk, so the desktop shell
//! groups the window under the Dbento entry and shows its icon.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dbento_app::app::{WindowState, window_options, window_options_for};
use dbento_app::identity::{APP_ID, window_icon};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

#[test]
fn the_app_id_is_dbento_app_and_named_on_the_window() {
    assert_eq!(APP_ID, "dbento-app");
    assert_eq!(window_options().app_id.as_deref(), Some(APP_ID));
}

#[test]
fn the_window_icon_is_the_committed_256_px_png() {
    let icon = window_icon().expect("the committed PNG decodes");
    let png = std::fs::read(repo("assets/icon/generated/png/256x256.png")).unwrap();
    let expected = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(icon.dimensions(), (256, 256));
    assert!(
        icon.as_raw() == expected.as_raw(),
        "pixels match the committed PNG"
    );
}

#[test]
fn the_icon_is_decoded_once_and_set_on_every_main_window() {
    let icon = window_icon().unwrap();
    assert!(Arc::ptr_eq(&icon, &window_icon().unwrap()), "decoded once");
    let saved = WindowState {
        x: 10.,
        y: 10.,
        width: 900.,
        height: 600.,
        maximized: false,
    };
    for opts in [window_options(), window_options_for(Some(&saved))] {
        assert!(Arc::ptr_eq(opts.icon.as_ref().expect("icon set"), &icon));
    }
}

#[test]
fn the_windows_resource_embeds_the_committed_ico() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(manifest.join("build.rs")).expect("build.rs exists");
    let rel = "../../assets/icon/generated/dbento.ico";
    assert!(
        script.contains(&format!("\"{rel}\"")),
        "build.rs names {rel}"
    );
    assert!(
        script.contains("CARGO_CFG_TARGET_OS"),
        "the target, not the host, decides"
    );
    let ico = std::fs::read(manifest.join(rel)).unwrap();
    assert_eq!(&ico[..4], &[0, 0, 1, 0], "an ICO header");
}
