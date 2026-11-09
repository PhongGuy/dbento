use crate::support;
use dbento_app::sidebar::menu_icon;
use dbento_app::theme::Appearance;
use dbento_app::ui::{self, APP_ICON, AppAssets, Icons};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, AssetSource as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window, div, px,
};

#[test]
fn every_icon_the_app_names_loads_through_the_app_assets() {
    let missing: Vec<_> = Icons::ALL
        .iter()
        .map(|i| i.path().to_string())
        .chain([APP_ICON.to_owned()])
        .filter(|p| !matches!(AppAssets.load(p), Ok(Some(b)) if !b.is_empty()))
        .collect();
    assert!(missing.is_empty(), "blank icons: {missing:?}");
}
#[test]
fn catalog_and_default_icons_load_and_a_missing_one_is_none() {
    assert!(matches!(AppAssets.load("icons/table-2.svg"), Ok(Some(_))));
    assert!(matches!(AppAssets.load("icons/search.svg"), Ok(Some(_))));
    assert!(matches!(AppAssets.load("icons/no-such-icon.svg"), Ok(None)));
}
struct Probe;
impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(ui::chip("probe-chip", "read-only", cx))
            .child(
                div()
                    .id("probe-keys")
                    .test_support()
                    .child(ui::key_chip("Ctrl O", cx)),
            )
            .child(ui::section_label("explorer", cx))
    }
}
#[gpui_kit::test]
fn a_chip_is_22_px_and_names_itself_and_a_key_chip_is_18_px(cx: &mut TestAppContext) {
    support::init(cx);
    let win = cx.add_window(|_, _| Probe);
    cx.update_window(win.into(), |_, w, cx| {
        w.render_frame(cx);
        let chip = w.find("probe-chip");
        assert_eq!(chip.label(), Some("read-only"));
        assert_eq!(chip.bounds().size.height, px(22.));
        assert_eq!(w.find("probe-keys").bounds().size.height, px(18.));
    })
    .unwrap();
}
struct Keys;
impl Render for Keys {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(
                div()
                    .id("keys-joined")
                    .test_support()
                    .child(ui::key_chip("Ctrl O", cx)),
            )
            .child(
                div()
                    .id("keys-apart")
                    .test_support()
                    .flex()
                    .gap_1()
                    .child(ui::key_chip("Ctrl", cx))
                    .child(ui::key_chip("O", cx)),
            )
    }
}
#[gpui_kit::test]
fn a_key_chip_boxes_each_key(cx: &mut TestAppContext) {
    support::init(cx);
    let win = cx.add_window(|_, _| Keys);
    cx.update_window(win.into(), |_, w, cx| {
        w.render_frame(cx);
        let (joined, apart) = (
            w.find("keys-joined").bounds(),
            w.find("keys-apart").bounds(),
        );
        let diff = (f32::from(joined.size.width) - f32::from(apart.size.width)).abs();
        assert!(diff < 0.5, "{joined:?} vs {apart:?}");
        assert_eq!(joined.size.height, px(18.));
    })
    .unwrap();
}
// macOS spells chords with its own modifier glyphs, so these Ctrl spellings hold elsewhere.
#[cfg(not(target_os = "macos"))]
mod chord_spelling {
    use dbento_app::keymap::{HISTORY_KEYS, RUN_STATEMENT_KEYS, chip_keys};
    use dbento_app::theme::APPEARANCE_KEYS;

    #[test]
    fn chip_keys_spell_every_stroke_of_a_chord() {
        assert_eq!(chip_keys(HISTORY_KEYS), "Ctrl Shift H");
        assert_eq!(chip_keys(APPEARANCE_KEYS), "Ctrl K Ctrl T");
        assert_eq!(chip_keys(RUN_STATEMENT_KEYS), "Ctrl \u{23CE}");
    }
}
#[test]
fn appearance_names_read_as_words() {
    assert_eq!(
        [Appearance::System, Appearance::Dark, Appearance::Light].map(Appearance::label),
        ["System", "Dark", "Light"]
    );
}
#[test]
fn every_explorer_menu_item_has_its_glyph() {
    assert_eq!(menu_icon("menu-new-connection"), Some(Icons::Plus));
    assert_eq!(menu_icon("menu-new-folder"), Some(Icons::FolderPlus));
    assert_eq!(menu_icon("menu-rename"), Some(Icons::Pencil));
    assert_eq!(menu_icon("menu-edit"), Some(Icons::Settings));
    assert_eq!(menu_icon("menu-delete"), Some(Icons::Trash));
    assert_eq!(menu_icon("menu-color"), None);
}
// The kit's variant draws the border in every state and fades it when disabled; a border on
// the button itself would be replayed over that fade, so the wrapper sets none.
#[test]
fn a_secondary_button_leaves_its_border_to_the_kit() {
    use gpui_kit::component::button::Button;
    let mut button = ui::secondary(Button::new("probe"));
    let style = button.style();
    let edges = &style.border_widths;
    assert_eq!(
        [edges.top, edges.right, edges.bottom, edges.left],
        [None; 4]
    );
    assert_eq!(style.border_color, None);
}
// Every secondary button goes through `ui::secondary`, so none can skip the border.
#[test]
fn no_source_file_styles_a_secondary_button_by_hand() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut bare = Vec::new();
    let mut dirs = vec![src];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("src/ui.rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    if line.contains(".secondary()") || line.contains("ButtonVariant::Secondary") {
                        bare.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
    }
    assert!(bare.is_empty(), "use ui::secondary instead: {bare:?}");
}
