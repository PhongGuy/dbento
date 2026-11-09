use crate::support;
use dbento_app::settings::{self, Density, Metrics, Settings};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{TestAppContext, px};

fn metrics(density: Density, ui: u8, grid: u8) -> Metrics {
    let mut s = Settings::default();
    s.density = density;
    s.ui_font.size = ui;
    s.grid_font.size = grid;
    s.metrics()
}
fn custom() -> Settings {
    let mut s = Settings::default();
    s.ui_font.family = "JetBrains Mono".into();
    s.ui_font.size = 15;
    s.editor_font.family = "Inter".into();
    s.editor_font.size = 14;
    s.grid_font.family = "Inter".into();
    s.grid_font.size = 11;
    s
}
fn fonts(cx: &mut TestAppContext) -> (String, f32, String, f32) {
    cx.update(|cx| {
        let t = Theme::global(cx);
        (
            t.font_family.to_string(),
            f32::from(t.font_size),
            t.mono_font_family.to_string(),
            f32::from(t.mono_font_size),
        )
    })
}
const CUSTOM: (&str, f32, &str, f32) = ("JetBrains Mono", 15., "Inter", 14.);
fn is_custom(f: (String, f32, String, f32)) -> bool {
    (f.0.as_str(), f.1, f.2.as_str(), f.3) == CUSTOM
}

#[test]
fn density_sets_the_base_and_the_font_sets_the_floor() {
    let m = |grid: f32, tree: f32| Metrics {
        grid_row: px(grid),
        tree_row: px(tree),
    };
    assert_eq!(Metrics::default(), m(28., 26.));
    assert_eq!(metrics(Density::Default, 13, 12), m(28., 26.));
    assert_eq!(
        metrics(Density::Compact, 13, 12),
        m(24., 22.),
        "the base wins at default sizes"
    );
    assert_eq!(
        metrics(Density::Default, 13, 17),
        m(30., 26.),
        "ceil(21.25) + 8"
    );
    assert_eq!(metrics(Density::Compact, 13, 20), m(33., 22.));
    assert_eq!(
        metrics(Density::Compact, 16, 12),
        m(24., 24.),
        "ceil(20) + 4"
    );
    assert_eq!(metrics(Density::Default, 16, 12), m(28., 26.));
    assert_eq!(metrics(Density::Compact, 12, 12), m(24., 22.));
}

#[gpui_kit::test]
fn before_any_apply_the_defaults_and_theme_mono_are_read(cx: &mut TestAppContext) {
    support::init(cx);
    let (metrics, grid, mono) = cx.update(|cx| {
        let t = Theme::global(cx);
        (
            settings::active_metrics(cx),
            settings::grid_font(cx),
            (t.mono_font_family.clone(), t.mono_font_size),
        )
    });
    assert_eq!(metrics, Metrics::default());
    assert_eq!(grid, mono);
}

#[gpui_kit::test]
fn apply_puts_the_fonts_on_the_theme_and_metrics_in_the_global(cx: &mut TestAppContext) {
    support::init(cx);
    let mut s = custom();
    s.density = Density::Compact;
    cx.update(|cx| settings::apply(&s, cx));
    cx.run_until_parked();
    assert!(is_custom(fonts(cx)), "{:?}", fonts(cx));
    let (metrics, (family, size)) =
        cx.update(|cx| (settings::active_metrics(cx), settings::grid_font(cx)));
    assert_eq!(metrics, s.metrics());
    assert_eq!((family.as_ref(), size), ("Inter", px(11.)));
}

#[gpui_kit::test]
fn applied_fonts_survive_a_mode_switch(cx: &mut TestAppContext) {
    support::init(cx);
    cx.update(|cx| settings::apply(&custom(), cx));
    for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| dbento_app::theme::set_mode(mode, None, cx));
        cx.run_until_parked();
        assert!(is_custom(fonts(cx)), "{mode:?}: {:?}", fonts(cx));
    }
}

#[gpui_kit::test]
fn applied_fonts_survive_a_later_theme_load(cx: &mut TestAppContext) {
    // A later registry load fires the kit observer (registry.rs:46-69), whose Theme::change
    // resets the fonts from the theme JSON.
    support::init(cx);
    cx.update(|cx| settings::apply(&custom(), cx));
    cx.run_until_parked();
    cx.update(dbento_app::theme::install);
    cx.run_until_parked();
    assert!(is_custom(fonts(cx)), "{:?}", fonts(cx));
}

#[gpui_kit::test]
fn applied_fonts_survive_startup_in_one_update(cx: &mut TestAppContext) {
    support::init_then(cx, |cx| settings::apply(&custom(), cx));
    cx.run_until_parked();
    assert!(is_custom(fonts(cx)), "{:?}", fonts(cx));
}
