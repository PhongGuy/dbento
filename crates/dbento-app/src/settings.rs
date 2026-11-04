//! The user's settings file: appearance, the three font choices and density, kept in
//! `settings.json` beside the app state.

use std::ops::RangeInclusive;
use std::path::Path;

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Pixels, SharedString, px};
use serde::{Deserialize, Deserializer, Serialize};

use crate::persist::load_json;
use crate::theme::Appearance;

/// The sizes the UI font may take, in pixels.
pub const UI_SIZES: RangeInclusive<u8> = 12..=16;
/// The sizes the editor font may take, in pixels.
pub const EDITOR_SIZES: RangeInclusive<u8> = 10..=24;
/// The sizes the grid font may take, in pixels.
pub const GRID_SIZES: RangeInclusive<u8> = 10..=20;

const UI_FAMILY: &str = "Inter";
const MONO_FAMILY: &str = "JetBrains Mono";

/// Everything `settings.json` holds. Unknown top-level fields are kept and written back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawSettings")]
pub struct Settings {
    /// The appearance choice.
    pub appearance: Appearance,
    /// The font of the app's chrome.
    pub ui_font: FontChoice,
    /// The font of the SQL editor.
    pub editor_font: FontChoice,
    /// The font of the result grid.
    pub grid_font: FontChoice,
    /// How tightly rows are packed.
    pub density: Density,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            appearance: Appearance::System,
            ui_font: FontChoice::new(UI_FAMILY, 13),
            editor_font: FontChoice::new(MONO_FAMILY, 12),
            grid_font: FontChoice::new(MONO_FAMILY, 12),
            density: Density::Default,
            extra: serde_json::Map::new(),
        }
    }
}

impl Settings {
    /// Every size clamped into its range; families untouched.
    pub fn clamped(mut self) -> Settings {
        let clamp = |size: u8, range: RangeInclusive<u8>| size.clamp(*range.start(), *range.end());
        self.ui_font.size = clamp(self.ui_font.size, UI_SIZES);
        self.editor_font.size = clamp(self.editor_font.size, EDITOR_SIZES);
        self.grid_font.size = clamp(self.grid_font.size, GRID_SIZES);
        self
    }

    /// Row heights for this density: the density's base, raised so the font still fits.
    pub fn metrics(&self) -> Metrics {
        let (grid_base, tree_base) = match self.density {
            Density::Default => (28., 26.),
            Density::Compact => (24., 22.),
        };
        let fit = |size: u8, pad: f32| (size as f32 * 1.25).ceil() + pad;
        Metrics {
            grid_row: px(f32::max(grid_base, fit(self.grid_font.size, 8.))),
            tree_row: px(f32::max(tree_base, fit(self.ui_font.size, 4.))),
        }
    }

    /// Every known field back to its default; the unknown fields stay.
    pub(crate) fn reset_known(&mut self) {
        let extra = std::mem::take(&mut self.extra);
        *self = Settings {
            extra,
            ..Settings::default()
        };
    }

    /// The unknown top-level fields, written back unchanged.
    pub fn extra(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra
    }
}

/// A font family by name and a size in whole pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FontChoice {
    /// The family name; any string is kept, even one not installed.
    pub family: String,
    /// The size in pixels.
    pub size: u8,
}

impl FontChoice {
    fn new(family: &str, size: u8) -> FontChoice {
        FontChoice {
            family: family.into(),
            size,
        }
    }
}

/// How tightly the grid and tree pack their rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// The standard row heights.
    #[default]
    Default,
    /// Shorter rows.
    Compact,
}

// The file as written, every field optional, so a missing field (or a font missing its
// size) takes that field's own default instead of failing the file.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    appearance: Option<Appearance>,
    ui_font: Option<RawFont>,
    editor_font: Option<RawFont>,
    grid_font: Option<RawFont>,
    density: Option<Density>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFont {
    family: Option<String>,
    size: Option<Size>,
}

impl RawFont {
    fn or(self, default: FontChoice) -> FontChoice {
        FontChoice {
            family: self.family.unwrap_or(default.family),
            size: self.size.map_or(default.size, |s| s.0),
        }
    }
}

// Any JSON number, rounded to whole pixels and saturated into a u8; a string fails.
struct Size(u8);

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Size, D::Error> {
        // `as` saturates out-of-range floats, which clamping then fixes.
        Ok(Size(f64::deserialize(d)?.round() as u8))
    }
}

impl From<RawSettings> for Settings {
    fn from(raw: RawSettings) -> Settings {
        let d = Settings::default();
        let font = |f: Option<RawFont>, default| f.unwrap_or_default().or(default);
        Settings {
            appearance: raw.appearance.unwrap_or(d.appearance),
            ui_font: font(raw.ui_font, d.ui_font),
            editor_font: font(raw.editor_font, d.editor_font),
            grid_font: font(raw.grid_font, d.grid_font),
            density: raw.density.unwrap_or(d.density),
            extra: raw.extra,
        }
    }
}

/// A loaded settings file.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsLoad {
    /// The file's settings, clamped, or the defaults.
    pub settings: Settings,
    /// Set when the file could not be used; names the file, never its content.
    pub recovered: Option<String>,
    /// False when the file exists but could not be read: this session must not overwrite it.
    pub writable: bool,
}

/// Reads `path`. Missing: defaults with `appearance = seed`. Parsed: clamped, seed ignored.
/// Unparseable: moved to `settings.json.bad`, defaults (seed ignored). Unreadable: defaults,
/// not writable.
pub fn load(path: &Path, seed: Appearance) -> SettingsLoad {
    let missing = matches!(path.try_exists(), Ok(false));
    let loaded = load_json::<Settings>(path);
    let mut settings = loaded.value.clamped();
    if missing {
        settings.appearance = seed;
    }
    // A recovered file that is still in place was not set aside, so it was never read and
    // must survive this session.
    let writable = loaded.recovered.is_none() || matches!(path.try_exists(), Ok(false));
    SettingsLoad {
        settings,
        recovered: loaded.recovered,
        writable,
    }
}

/// Sizes of the density-dependent rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// The height of a result grid row and its header.
    pub grid_row: Pixels,
    /// The height of a schema tree row.
    pub tree_row: Pixels,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Settings::default().metrics()
    }
}

// The settings last applied, kept so a theme reload can put the fonts back.
struct Applied(Settings);
impl Global for Applied {}

/// Stores `settings` as the applied set and puts its fonts on the kit Theme
/// (font_family/font_size from ui_font, mono_font_family/mono_font_size from editor_font).
pub fn apply(settings: &Settings, cx: &mut App) {
    cx.set_global(Applied(settings.clone()));
    reapply(cx);
}

/// Puts the applied fonts back on the Theme after something reloaded it; no-op before any apply.
pub fn reapply(cx: &mut App) {
    let Some(Applied(s)) = cx.try_global::<Applied>() else {
        return;
    };
    let (ui, editor) = (s.ui_font.clone(), s.editor_font.clone());
    Theme::update(cx, |t| {
        t.font_family = ui.family.into();
        t.font_size = px(ui.size.into());
        t.mono_font_family = editor.family.into();
        t.mono_font_size = px(editor.size.into());
    });
}

/// The applied metrics, or `Metrics::default()` before any apply.
pub fn active_metrics(cx: &App) -> Metrics {
    cx.try_global::<Applied>()
        .map_or_else(Metrics::default, |a| a.0.metrics())
}

/// The applied grid font, or the theme mono font before any apply.
pub fn grid_font(cx: &App) -> (SharedString, Pixels) {
    match cx.try_global::<Applied>() {
        Some(Applied(s)) => (
            s.grid_font.family.clone().into(),
            px(s.grid_font.size.into()),
        ),
        None => {
            let t = Theme::global(cx);
            (t.mono_font_family.clone(), t.mono_font_size)
        }
    }
}
