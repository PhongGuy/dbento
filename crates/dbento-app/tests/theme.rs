mod support;
use dbento_app::theme::{DARK, LIGHT, THEME_JSON};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::{Hsla, Rgba};
use serde_json::Value;

fn theme(name: &str) -> Value {
    let all: Value = serde_json::from_str(THEME_JSON).unwrap();
    all["themes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap()
        .clone()
}
// "syntax.k" is a syntax colour, "highlight.k" a highlight key, anything else a colour key.
fn hex(t: &Value, key: &str) -> String {
    let v = if let Some(k) = key.strip_prefix("syntax.") {
        &t["highlight"]["syntax"][k]["color"]
    } else if let Some(k) = key.strip_prefix("highlight.") {
        &t["highlight"][k]
    } else {
        &t["colors"][key]
    };
    v.as_str()
        .unwrap_or_else(|| panic!("{} has no {key}", t["name"]))
        .to_ascii_uppercase()
}
fn luminance(hex: &str) -> f64 {
    let h = hex.trim_start_matches('#');
    assert_eq!(h.len(), 6, "{hex} must be #RRGGBB");
    let ch = |i: usize| {
        let c = f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap()) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(0) + 0.7152 * ch(2) + 0.0722 * ch(4)
}
fn ratio(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
// The spec's palette: every key in a row carries that row's dark and light colour.
const PALETTE: &[(&[&str], &str, &str)] = &[
    (
        &[
            "background",
            "title_bar.background",
            "status_bar.background",
        ],
        "#060608",
        "#ECECF1",
    ),
    (
        &[
            "sidebar.background",
            "table.background",
            "list.background",
            "table.row.border",
        ],
        "#111115",
        "#FFFFFF",
    ),
    (
        &[
            "tab_bar.background",
            "tab.background",
            "table.head.background",
            "list.head.background",
        ],
        "#111115",
        "#F4F4F7",
    ),
    (
        &[
            "highlight.editor.background",
            "highlight.editor.gutter.background",
            "tab.active.background",
        ],
        "#0B0B0E",
        "#FFFFFF",
    ),
    (
        &["table.even.background", "list.even.background"],
        "#141418",
        "#F7F7F9",
    ),
    (
        &[
            "popover.background",
            "secondary.background",
            "button.secondary.background",
        ],
        "#18181D",
        "#FFFFFF",
    ),
    (
        &[
            "list.hover.background",
            "table.hover.background",
            "highlight.editor.active_line.background",
        ],
        "#1F1F25",
        "#EEEEF3",
    ),
    (
        &["list.active.background", "table.active.background"],
        "#10241F",
        "#DDF3EC",
    ),
    (
        &[
            "border",
            "title_bar.border",
            "sidebar.border",
            "status_bar.border",
        ],
        "#232329",
        "#E4E4EA",
    ),
    (&["input.border"], "#6A6A76", "#85858F"),
    (
        &[
            "foreground",
            "popover.foreground",
            "sidebar.foreground",
            "tab.active.foreground",
            "table.head.foreground",
            "secondary.foreground",
            "button.secondary.foreground",
            "highlight.editor.active_line_number",
        ],
        "#E6E6EB",
        "#1B1530",
    ),
    (
        &["muted.foreground", "tab.foreground"],
        "#A1A1AA",
        "#52525E",
    ),
    (&["highlight.hint"], "#9494A0", "#62626F"),
    (&["highlight.editor.line_number"], "#6C6C76", "#85858F"),
    (
        &[
            "primary.background",
            "button.primary.background",
            "ring",
            "caret",
        ],
        "#20C997",
        "#066B4D",
    ),
    (
        &["primary.foreground", "button.primary.foreground"],
        "#1B1530",
        "#FFFFFF",
    ),
    (
        &["danger.background", "button.danger.background"],
        "#FF6B6B",
        "#C92A2A",
    ),
    (
        &["danger.foreground", "button.danger.foreground"],
        "#1B1530",
        "#FFFFFF",
    ),
    (&["warning.background"], "#FCC419", "#8F5F00"),
    (&["syntax.keyword"], "#F06595", "#C2255C"),
    (&["syntax.number"], "#FF922B", "#A63E00"),
    (&["syntax.string"], "#FCC419", "#7A5600"),
    (&["syntax.function"], "#74C0FC", "#1864AB"),
];
// Frame, panel, light chrome, editor, zebra, raised, hover, selection.
const SURFACES: [&str; 8] = [
    "background",
    "table.background",
    "table.head.background",
    "highlight.editor.background",
    "table.even.background",
    "popover.background",
    "list.hover.background",
    "list.active.background",
];
const TEXT: [&str; 10] = [
    "foreground",
    "muted.foreground",
    "highlight.hint",
    "primary.background",
    "danger.background",
    "warning.background",
    "syntax.keyword",
    "syntax.number",
    "syntax.string",
    "syntax.function",
];

fn failures(min: f64, pairs: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    for name in [DARK, LIGHT] {
        let t = theme(name);
        for (fg, bg) in pairs {
            let r = ratio(&hex(&t, fg), &hex(&t, bg));
            if r < min {
                out.push(format!("{name}: {fg} on {bg} = {r:.2}"));
            }
        }
    }
    out
}
#[test]
fn both_themes_carry_the_spec_palette() {
    for (keys, dark, light) in PALETTE {
        for key in *keys {
            assert_eq!(hex(&theme(DARK), key), *dark, "{DARK} {key}");
            assert_eq!(hex(&theme(LIGHT), key), *light, "{LIGHT} {key}");
        }
    }
}
#[test]
fn every_text_role_reads_on_every_surface() {
    let pairs: Vec<_> = TEXT
        .iter()
        .flat_map(|f| SURFACES.iter().map(move |b| (*f, *b)))
        .collect();
    let bad = failures(4.5, &pairs);
    assert!(bad.is_empty(), "{bad:#?}");
}
#[test]
fn filled_buttons_read_in_every_state() {
    let mut pairs = vec![
        ("primary.foreground", "primary.background"),
        ("danger.foreground", "danger.background"),
    ];
    for v in ["primary", "danger", "secondary"] {
        for s in ["background", "hover.background", "active.background"] {
            let (fg, bg) = (format!("button.{v}.foreground"), format!("button.{v}.{s}"));
            pairs.push((&*fg.leak(), &*bg.leak()));
        }
    }
    let bad = failures(4.5, &pairs);
    assert!(bad.is_empty(), "{bad:#?}");
}
#[test]
fn input_borders_and_the_focus_ring_reach_three_to_one() {
    let mut pairs: Vec<_> = SURFACES.iter().map(|s| ("input.border", *s)).collect();
    pairs.extend([
        ("ring", "background"),
        ("ring", "highlight.editor.background"),
        ("ring", "popover.background"),
    ]);
    let bad = failures(3.0, &pairs);
    assert!(bad.is_empty(), "{bad:#?}");
}
#[test]
fn faint_numbers_reach_three_to_one_where_they_sit() {
    let bad = failures(
        3.0,
        &[
            ("highlight.editor.line_number", "table.background"),
            (
                "highlight.editor.line_number",
                "highlight.editor.background",
            ),
            ("highlight.editor.line_number", "table.even.background"),
            ("highlight.editor.line_number", "list.active.background"),
            ("highlight.editor.line_number", "list.hover.background"),
        ],
    );
    assert!(bad.is_empty(), "{bad:#?}");
}
#[test]
fn both_themes_are_dense_and_never_italic() {
    for name in [DARK, LIGHT] {
        let t = theme(name);
        for (key, want) in [
            ("font.size", 13.0),
            ("mono_font.size", 12.0),
            ("radius", 2.0),
            ("radius.lg", 4.0),
        ] {
            assert_eq!(t[key].as_f64(), Some(want), "{name} {key}");
        }
        assert_eq!(t["shadow"], false, "{name}: flat surfaces");
        for (k, style) in t["highlight"]["syntax"].as_object().unwrap() {
            assert_ne!(
                style["font_style"], "italic",
                "{name} syntax.{k}: no mono italic is bundled"
            );
        }
    }
}
#[test]
fn the_dialog_overlay_is_black_at_sixty_percent() {
    for name in [DARK, LIGHT] {
        assert_eq!(hex(&theme(name), "overlay"), "#00000099", "{name}");
    }
}

fn channel(c: f32) -> f64 {
    let c = f64::from(c);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn rgba_luminance(c: Rgba) -> f64 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}
/// `fg` composited over opaque `bg`.
fn over(fg: Hsla, bg: Rgba) -> Rgba {
    let f = Rgba::from(fg);
    let mix = |a: f32, b: f32| a * f.a + b * (1.0 - f.a);
    Rgba {
        r: mix(f.r, bg.r),
        g: mix(f.g, bg.g),
        b: mix(f.b, bg.b),
        a: 1.0,
    }
}
fn rgba_ratio(a: Rgba, b: Rgba) -> f64 {
    let (x, y) = (rgba_luminance(a), rgba_luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[gpui_kit::test]
fn the_pending_chip_and_border_read_in_both_themes(cx: &mut gpui_kit::TestAppContext) {
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let p = dbento_app::theme::edit_pending(cx);
            let table = Rgba::from(cx.theme().table);
            let fill = over(p.fill, table);
            let text = rgba_ratio(over(p.chip_text, fill), fill);
            assert!(text >= 4.5, "{mode:?}: chip text {text:.2}");
            for (what, bg) in [("fill", fill), ("table", table)] {
                let edge = rgba_ratio(over(p.border, bg), bg);
                assert!(edge >= 3.0, "{mode:?}: border on {what} {edge:.2}");
            }
        });
    }
}

#[gpui_kit::test]
fn the_sql_well_edge_reaches_three_to_one_on_the_dialog(cx: &mut gpui_kit::TestAppContext) {
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let edge = dbento_app::theme::code_edge(cx);
            let popover = Rgba::from(cx.theme().popover);
            let well = over(dbento_app::theme::editor_fill(cx), popover);
            for (what, bg) in [("dialog", popover), ("well", well)] {
                let r = rgba_ratio(over(edge, bg), bg);
                assert!(r >= 3.0, "{mode:?}: SQL well edge on the {what} {r:.2}");
            }
        });
    }
}
// `ui::secondary` draws on the kit's bordered neutral button, whose fill must stay the
// secondary fill so the border is the only change.
#[test]
fn the_neutral_button_fill_is_the_secondary_fill() {
    for name in [DARK, LIGHT] {
        let t = theme(name);
        for state in [
            "background",
            "hover.background",
            "active.background",
            "foreground",
        ] {
            assert_eq!(
                hex(&t, &format!("button.{state}")),
                hex(&t, &format!("button.secondary.{state}")),
                "{name} {state}"
            );
        }
    }
}
// The kit's default button, which `ui::secondary` uses, draws its border in `theme.input`;
// that must be the theme file's input border in both modes.
#[gpui_kit::test]
fn the_secondary_button_border_is_the_input_border(cx: &mut gpui_kit::TestAppContext) {
    support::init(cx);
    for (mode, name) in [(ThemeMode::Dark, DARK), (ThemeMode::Light, LIGHT)] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let c = Rgba::from(cx.theme().input);
            let byte = |v: f32| (v * 255.).round() as u8;
            let got = format!("#{:02X}{:02X}{:02X}", byte(c.r), byte(c.g), byte(c.b));
            assert_eq!(got, hex(&theme(name), "input.border"), "{mode:?}");
        });
    }
}
