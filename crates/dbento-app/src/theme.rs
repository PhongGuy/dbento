//! Installs the bundled fonts and the Dbento light and dark themes into gpui-kit's global
//! `Theme`, and switches between them by the user's System/Dark/Light appearance choice.

use std::borrow::Cow;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Colorize, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{
    AnyView, App, Global, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    WindowAppearance, actions, div, px, rgb,
};
use serde::{Deserialize, Serialize};

use crate::keymap::chip_keys;
use crate::ui;

actions!(
    dbento,
    [
        /// Moves the appearance choice to the next one: System, Dark, Light, then System.
        CycleAppearance
    ]
);

/// The user's appearance choice, saved in the app state. System follows the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// The OS window appearance decides the mode.
    #[default]
    System,
    /// Always the dark theme.
    Dark,
    /// Always the light theme.
    Light,
}

impl Appearance {
    /// The choice the shortcut moves to: System, Dark, Light, then System again.
    pub fn next(self) -> Appearance {
        match self {
            Appearance::System => Appearance::Dark,
            Appearance::Dark => Appearance::Light,
            Appearance::Light => Appearance::System,
        }
    }

    /// The choice as a word for labels: "System", "Dark" or "Light".
    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Dark => "Dark",
            Appearance::Light => "Light",
        }
    }
}

/// The theme mode for `choice` when the OS reports `os` (from Apinata).
pub fn mode_for(choice: Appearance, os: WindowAppearance) -> ThemeMode {
    match choice {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => os.into(),
    }
}

/// Puts the kit in the mode for `choice` under `os`. The one path for startup, the shortcut
/// and OS changes; a mode change refreshes every window, so it happens only on a difference.
pub fn apply(choice: Appearance, os: WindowAppearance, window: &mut Window, cx: &mut App) {
    let mode = mode_for(choice, os);
    if Theme::global(cx).mode != mode {
        set_mode(mode, Some(window), cx);
    }
}

/// The shipped theme set: one dark and one light theme setting every kit colour key.
pub const THEME_JSON: &str = include_str!("../assets/theme/dbento.json");
/// The name of the shipped dark theme.
pub const DARK: &str = "Dbento Dark";
/// The name of the shipped light theme.
pub const LIGHT: &str = "Dbento Light";
/// The bundled TrueType fonts: Inter Regular, Medium, SemiBold and Italic, then JetBrains
/// Mono Regular and Bold.
pub const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
];

/// The appearance chord, Zed's theme keys.
pub const APPEARANCE_KEYS: &str = "secondary-k secondary-t";

/// Marks an app whose keymap table is bound, so a second [`install`] leaves it alone.
struct KeymapBound;
impl Global for KeymapBound {}

/// Marks an app whose theme registry is watched for reloads that reset the fonts.
struct RegistryWatched;
impl Global for RegistryWatched {}

/// Registers the bundled fonts and themes, makes the themes the kit's light and dark themes,
/// reloads the current mode and binds the whole keymap table once per app; calling it again
/// binds nothing more. Fonts or a theme that fail to load leave the system fonts or the kit
/// default and log to stderr.
pub fn install(cx: &mut App) {
    // After gpui_kit::init, so the run keys win over the editor's own Ctrl+Enter; the table
    // holds the appearance and copy keys too.
    if !cx.has_global::<KeymapBound>() {
        crate::keymap::bind(cx);
        cx.set_global(KeymapBound);
    }
    // Fonts go first so the theme reload below already resolves "Inter" on the first frame.
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("dbento: bundled fonts failed to register ({err}); using system fonts");
    }
    // Watch before loading: the kit's own registry observer resets the fonts on every load,
    // and ours, registered after it, runs after it to put the user's fonts back.
    if !cx.has_global::<RegistryWatched>() {
        cx.observe_global::<ThemeRegistry>(crate::settings::reapply)
            .detach();
        cx.set_global(RegistryWatched);
    }
    if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(THEME_JSON) {
        eprintln!("dbento: the bundled theme failed to load: {err}");
        return;
    }
    // The registry keeps the first theme loaded under a name, so a second call finds the
    // same configs and the reassignment below is idempotent.
    let themes = ThemeRegistry::global(cx).themes();
    let (dark, light) = (themes.get(DARK).cloned(), themes.get(LIGHT).cloned());
    Theme::update(cx, |t| {
        if let Some(dark) = dark {
            t.dark_theme = dark;
        }
        if let Some(light) = light {
            t.light_theme = light;
        }
    });
    // `change` reloads the current mode's config even when the mode is unchanged.
    set_mode(Theme::global(cx).mode, None, cx);
}

/// The dim text colour (types, hints, NULL): the active theme's `highlight.hint`, else
/// `muted_foreground`.
pub fn dim(cx: &App) -> Hsla {
    // The key itself is private; the kit answers its cyan when the theme leaves it unset.
    let theme = Theme::global(cx);
    let hint = theme.highlight_theme.style.status.hint(cx);
    if hint == theme.cyan {
        theme.muted_foreground
    } else {
        hint
    }
}

/// The editor's fill (inputs, the title pill, result cells): the active theme's editor
/// background, else `background`.
pub fn editor_fill(cx: &App) -> Hsla {
    let theme = Theme::global(cx);
    theme
        .highlight_theme
        .style
        .editor_background
        .unwrap_or(theme.background)
}

/// The edge of a boxed SQL well (the approval and commit prompts): the input border, since
/// the well's fill matches the light dialog's and a hairline alone barely shows it.
pub fn code_edge(cx: &App) -> Hsla {
    Theme::global(cx).input
}

/// The colour of small affordance icons (the gear, undo, redo): `muted_foreground` made
/// fully opaque, so it reads on the sidebar, the background and the editor fill.
pub fn affordance(cx: &App) -> Hsla {
    Hsla {
        a: 1.,
        ..Theme::global(cx).muted_foreground
    }
}

/// The rail glyph for an appearance choice: both halves for System, else the chosen one.
pub fn appearance_icon(choice: Appearance) -> IconName {
    match choice {
        Appearance::System => ui::Icons::SunMoon,
        Appearance::Dark => ui::Icons::Moon,
        Appearance::Light => ui::Icons::Sun,
    }
}

/// The faint colour (grid row numbers, editor line numbers): the active theme's editor
/// line-number colour, else `dim`.
pub fn faint(cx: &App) -> Hsla {
    Theme::global(cx)
        .highlight_theme
        .style
        .editor_line_number
        .unwrap_or_else(|| dim(cx))
}

// The edit tints are derived, not theme keys: the kit's theme schema drops unknown keys.
const EDIT_TINT: f32 = 0.15;

/// The amber look of staged, uncommitted edits: the cell fill and edge, and the pending chip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingLook {
    /// A staged cell's (and the record form's modified field's) fill, and the pending chip's.
    pub fill: Hsla,
    /// A staged cell's edge and the pending chip's edge.
    pub border: Hsla,
    /// The pending chip's and the footer count's text.
    pub chip_text: Hsla,
}

/// The pending look for the active mode: dark tints `warning` at 20% and draws edge and text
/// in `warning`; light pairs a cream fill with a deep amber ink that reads on it.
pub fn edit_pending(cx: &App) -> PendingLook {
    let theme = Theme::global(cx);
    if theme.mode.is_dark() {
        PendingLook {
            fill: theme.warning.opacity(0.2),
            border: theme.warning,
            chip_text: theme.warning,
        }
    } else {
        let (fill, ink) = (Hsla::from(rgb(0xFFF4E0)), Hsla::from(rgb(0x8F5F00)));
        PendingLook {
            fill,
            border: ink,
            chip_text: ink,
        }
    }
}

/// The fill of a staged new row: the active theme's `success` at 15% alpha.
pub fn edit_inserted(cx: &App) -> Hsla {
    Theme::global(cx).success.opacity(EDIT_TINT)
}

/// The fill of a row marked deleted: the active theme's `danger` at 15% alpha.
pub fn edit_deleted(cx: &App) -> Hsla {
    Theme::global(cx).danger.opacity(EDIT_TINT)
}

/// A kit tooltip on the raised surface with `radius_lg` and at most 360 px wide.
pub fn tooltip(text: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> AnyView {
    // The kit puts text in a flex child sized to its content, so the cap goes on our own
    // content (360 less the kit's 8 px side padding) for the text to wrap inside the box.
    let text = text.into();
    framed(
        move |_, _| div().max_w(px(344.)).child(text.clone()),
        window,
        cx,
    )
}

/// [`tooltip`]'s box with `text`, then the boxed keys of `keys` (a binding string) at right;
/// no `keys` (an unbound action) is the plain [`tooltip`].
pub fn tooltip_keys(
    text: impl Into<SharedString>,
    keys: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let Some(keys) = keys else {
        return tooltip(text, window, cx);
    };
    let (text, chip) = (text.into(), SharedString::from(chip_keys(&keys)));
    framed(
        move |_, cx| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().max_w(px(344.)).child(text.clone()))
                .child(ui::key_chip(chip.clone(), cx))
        },
        window,
        cx,
    )
}

// The kit tooltip with `content`, on `radius_lg` and capped at 360 px.
fn framed<E: IntoElement>(
    content: impl Fn(&mut Window, &mut App) -> E + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let radius = Theme::global(cx).radius_lg;
    Tooltip::element(content)
        .rounded(radius)
        .max_w(px(360.))
        .build(window, cx)
}

/// Switches the kit to `mode`, reloads its theme and restores the opaque selected-row fill.
/// Every mode change in the app goes through here.
pub fn set_mode(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, window, cx);
    // The kit clamps `list.active.background` to 20% alpha on load, which hides the fill.
    Theme::update(cx, |t| {
        let config = if t.mode.is_dark() {
            &t.dark_theme
        } else {
            &t.light_theme
        };
        let opaque = config
            .colors
            .list_active
            .as_deref()
            .and_then(|hex| Hsla::parse_hex(hex).ok());
        if let Some(fill) = opaque {
            t.list_active = fill;
            t.tokens.list_active = fill.into();
        }
    });
    // The reload also reset the fonts from the theme JSON; the user's choices win.
    crate::settings::reapply(cx);
}
