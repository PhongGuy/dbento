//! The identity the desktop shell matches the window to its installed entry by.

use std::sync::{Arc, OnceLock};

use image::{ImageFormat, RgbaImage};

/// The window's app id and X11 class: the stem, `Icon` and `StartupWMClass` of the
/// installed desktop entry.
pub const APP_ID: &str = "dbento-app";

/// The 256 px icon for X11's `_NET_WM_ICON`, decoded from the committed
/// `assets/icon/generated/png/256x256.png` on first use and shared after; None if it fails
/// to decode, and the window then opens without one.
pub fn window_icon() -> Option<Arc<RgbaImage>> {
    static ICON: OnceLock<Option<Arc<RgbaImage>>> = OnceLock::new();
    let png = include_bytes!("../../../assets/icon/generated/png/256x256.png");
    ICON.get_or_init(|| {
        image::load_from_memory_with_format(png, ImageFormat::Png)
            .ok()
            .map(|img| Arc::new(img.to_rgba8()))
    })
    .clone()
}
