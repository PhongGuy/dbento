//! The Settings dialog's view: theme and density segments, a family picker and size field for
//! each of the UI, editor and grid fonts, and the keymap file with its problems and commands.
//! Every settings change goes straight to the settings store.

use std::ops::RangeInclusive;

use gpui_kit::base::actions::Cancel;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div,
    px,
};

use crate::confirm;
use crate::keymap::{self, chip_keys};
use crate::settings::{Density, EDITOR_SIZES, FontChoice, GRID_SIZES, Settings, UI_SIZES};
use crate::settings_store::{self, SettingsStore};
use crate::theme::{self, Appearance};
use crate::ui::{self, Icons, Notice, NoticeKind};

/// Which of the three fonts a control edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontTarget {
    /// The app's chrome.
    Ui,
    /// The SQL editor.
    Editor,
    /// The result grid.
    Grid,
}

impl FontTarget {
    /// Every target, in the order the dialog lists them.
    pub const ALL: [FontTarget; 3] = [FontTarget::Ui, FontTarget::Editor, FontTarget::Grid];

    /// The id part: "ui", "editor" or "grid".
    pub fn slug(self) -> &'static str {
        match self {
            FontTarget::Ui => "ui",
            FontTarget::Editor => "editor",
            FontTarget::Grid => "grid",
        }
    }

    fn label(self) -> &'static str {
        match self {
            FontTarget::Ui => "UI",
            FontTarget::Editor => "Editor",
            FontTarget::Grid => "Grid",
        }
    }

    fn sizes(self) -> RangeInclusive<u8> {
        match self {
            FontTarget::Ui => UI_SIZES,
            FontTarget::Editor => EDITOR_SIZES,
            FontTarget::Grid => GRID_SIZES,
        }
    }

    fn choice(self, s: &Settings) -> &FontChoice {
        match self {
            FontTarget::Ui => &s.ui_font,
            FontTarget::Editor => &s.editor_font,
            FontTarget::Grid => &s.grid_font,
        }
    }

    fn choice_mut(self, s: &mut Settings) -> &mut FontChoice {
        match self {
            FontTarget::Ui => &mut s.ui_font,
            FontTarget::Editor => &mut s.editor_font,
            FontTarget::Grid => &mut s.grid_font,
        }
    }

    fn id(self, part: &str) -> SharedString {
        format!("settings-{}-{part}", self.slug()).into()
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The families the app ships, always offered first.
pub const BUNDLED: [&str; 2] = ["Inter", "JetBrains Mono"];

/// BUNDLED first, then `installed` sorted and deduplicated without the bundled names.
pub fn family_list(installed: &[String]) -> Vec<String> {
    let mut rest: Vec<String> = installed
        .iter()
        .filter(|f| !BUNDLED.contains(&f.as_str()))
        .cloned()
        .collect();
    rest.sort();
    rest.dedup();
    BUNDLED
        .iter()
        .map(|f| (*f).to_owned())
        .chain(rest)
        .collect()
}

/// The families containing `query`, ignoring case, in list order; all of them for "".
pub fn filter_families(list: &[String], query: &str) -> Vec<String> {
    let query = query.to_lowercase();
    list.iter()
        .filter(|f| f.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// Bundled, or among `installed`.
pub fn is_installed(family: &str, installed: &[String]) -> bool {
    BUNDLED.contains(&family) || installed.iter().any(|f| f == family)
}

/// What the view asks of its host.
#[derive(Debug, Clone)]
pub enum SettingsViewEvent {
    /// The Close button was pressed.
    Close,
}

// The open list shows at most this many matches, so a large font catalogue stays cheap.
const MAX_OPTIONS: usize = 50;
const LABEL_WIDTH: f32 = 96.;
const COMMAND_ID_WIDTH: f32 = 220.;

/// The Settings dialog's content, over the installed settings store.
pub struct SettingsView {
    store: Entity<SettingsStore>,
    focus: FocusHandle,
    installed: Vec<String>,
    families: Vec<String>,
    sizes: [Entity<InputState>; 3],
    search: Entity<InputState>,
    picker: Option<FontTarget>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    /// Over `settings_store::ensure(Appearance::System, cx)`; lists
    /// `cx.text_system().all_font_names()`.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = settings_store::ensure(Appearance::System, cx);
        let installed = cx.text_system().all_font_names();
        let current = store.read(cx).get().clone();
        let sizes = FontTarget::ALL.map(|t| {
            let (range, size) = (t.sizes(), t.choice(&current).size);
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(size.to_string())
                    .step(1.)
                    .min((*range.start()).into())
                    .max((*range.end()).into())
            })
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let mut subscriptions: Vec<Subscription> = FontTarget::ALL
            .iter()
            .map(|&t| {
                cx.subscribe_in(&sizes[t.index()], window, move |this, _, e, window, cx| {
                    this.size_event(t, e, window, cx)
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(&search, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        }));
        subscriptions.push(cx.observe_in(&store, window, |this, _, window, cx| {
            this.echo(window, cx);
            cx.notify();
        }));
        SettingsView {
            store,
            focus: cx.focus_handle(),
            families: family_list(&installed),
            installed,
            sizes,
            search,
            picker: None,
            _subscriptions: subscriptions,
        }
    }

    /// The size field of `target`.
    pub fn size_input(&self, target: FontTarget) -> &Entity<InputState> {
        &self.sizes[target.index()]
    }

    /// The target whose family list is open, if any.
    pub fn open_picker(&self) -> Option<FontTarget> {
        self.picker
    }

    // A typed or stepped value applies at once, clamped; Enter and blur then show the
    // clamped value, so a half-typed "1" on the way to "15" is never rewritten.
    fn size_event(
        &mut self,
        t: FontTarget,
        e: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match e {
            InputEvent::Change => {
                let Some(size) = self.typed_size(t, cx) else {
                    return;
                };
                if size != t.choice(self.store.read(cx).get()).size {
                    self.store
                        .update(cx, |s, cx| s.change(cx, |v| t.choice_mut(v).size = size));
                }
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => self.show_stored(t, window, cx),
            _ => {}
        }
    }

    fn typed_size(&self, t: FontTarget, cx: &App) -> Option<u8> {
        let range = t.sizes();
        let typed = self.sizes[t.index()]
            .read(cx)
            .value()
            .trim()
            .parse::<u8>()
            .ok()?;
        Some(typed.clamp(*range.start(), *range.end()))
    }

    fn show_stored(&self, t: FontTarget, window: &mut Window, cx: &mut App) {
        let stored = t.choice(self.store.read(cx).get()).size.to_string();
        if *self.sizes[t.index()].read(cx).value() != *stored {
            self.sizes[t.index()].update(cx, |s, cx| s.set_value(stored, window, cx));
        }
    }

    // A change from elsewhere (reset, another window) reaches a field only where the typed
    // value would not already clamp to it; `set_value` emits no change, so this cannot loop.
    fn echo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for t in FontTarget::ALL {
            if self.typed_size(t, cx) != Some(t.choice(self.store.read(cx).get()).size) {
                self.show_stored(t, window, cx);
            }
        }
    }

    fn toggle_picker(&mut self, t: FontTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker == Some(t) {
            self.close_picker(window, cx);
        } else {
            self.picker = Some(t);
            self.search.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.focus(window, cx);
            });
        }
        cx.notify();
    }

    // The search box leaves the tree with the list, so focus comes back to the view and
    // Escape still reaches the dialog.
    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn pick(&mut self, t: FontTarget, family: String, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(window, cx);
        self.store.update(cx, |s, cx| {
            s.change(cx, |v| t.choice_mut(v).family = family)
        });
        cx.notify();
    }

    fn row(label: &'static str, control: impl IntoElement, cx: &App) -> AnyElement {
        h_flex()
            .gap_3()
            .min_h(px(32.))
            .child(
                div()
                    .w(px(LABEL_WIDTH))
                    .flex_none()
                    .text_sm()
                    .text_color(theme::dim(cx))
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
            .into_any_element()
    }

    fn font_rows(&self, t: FontTarget, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (border, radius, fill) = (theme.border, theme.radius, theme::editor_fill(cx));
        let family = t.choice(self.store.read(cx).get()).family.clone();
        let open = self.picker == Some(t);
        let trigger = h_flex()
            .id(t.id("font-family"))
            .role(Role::ComboBox)
            .aria_label(SharedString::from(format!(
                "{} font family: {family}",
                t.label()
            )))
            .aria_expanded(open)
            .test_support()
            .flex_1()
            .min_w_0()
            .h(px(32.))
            .px_2()
            .gap_2()
            .border_1()
            .border_color(border)
            .rounded(radius)
            .bg(fill)
            .cursor_pointer()
            .child(div().flex_1().min_w_0().truncate().child(family.clone()))
            .child(ui::icon(Icons::ChevronDown, px(12.), theme::dim(cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.toggle_picker(t, window, cx)));
        let size = div()
            .id(t.id("font-size"))
            .role(Role::SpinButton)
            .aria_label(SharedString::from(format!("{} font size", t.label())))
            .test_support()
            .flex_none()
            .w(px(112.))
            .child(NumberInput::new(&self.sizes[t.index()]).small().w_full());
        let missing = (!is_installed(&family, &self.installed)).then(|| {
            div()
                .id(SharedString::from(format!(
                    "settings-font-not-installed-{}",
                    t.slug()
                )))
                .role(Role::Label)
                .aria_label("not installed")
                .test_support()
                .flex_none()
                .text_xs()
                .text_color(cx.theme().warning)
                .child("not installed")
        });
        let line = h_flex()
            .gap_2()
            .child(trigger)
            .children(missing)
            .child(size);
        let mut rows = vec![Self::row(t.label(), line, cx)];
        if open {
            rows.push(self.picker_list(t, cx));
        }
        rows
    }

    // The open list sits under its trigger: a search field, then the matching families.
    // Escape there closes only the list; the dialog's own Cancel never sees it.
    fn picker_list(&self, t: FontTarget, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, bg, hover, radius) =
            (theme.border, theme.popover, theme.list_hover, theme.radius);
        let query = self.search.read(cx).value();
        let rows = filter_families(&self.families, &query)
            .into_iter()
            .take(MAX_OPTIONS)
            .enumerate()
            .map(|(n, family)| {
                div()
                    .id(t.id(&format!("font-option-{n}")))
                    .role(Role::ListBoxOption)
                    .aria_label(SharedString::from(family.clone()))
                    .test_support()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .truncate()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(family.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(t, family.clone(), window, cx)
                    }))
            });
        let search = div()
            .id(t.id("font-search"))
            .role(Role::SearchInput)
            .aria_label(SharedString::from(format!(
                "Search {} font families",
                t.label()
            )))
            .test_support()
            .child(
                Input::new(&self.search)
                    .small()
                    .min_h(px(28.))
                    .bg(theme::editor_fill(cx)),
            );
        div()
            .pl(px(LABEL_WIDTH + 12.))
            .key_context("SettingsFontPicker")
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close_picker(window, cx)))
            .child(
                v_flex()
                    .p_1()
                    .gap_1()
                    .border_1()
                    .border_color(border)
                    .rounded(radius)
                    .bg(bg)
                    .child(search)
                    .child(
                        v_flex()
                            .id(t.id("font-list"))
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .children(rows),
                    ),
            )
            .into_any_element()
    }

    // The keymap file is edited outside the app; this section opens it, reloads it, lists what
    // was wrong with it and every command id with its effective keys (read-only).
    fn keyboard_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let path = keymap::keymap_path(cx);
        let (mono, warning) = (cx.theme().mono_font_family.clone(), cx.theme().warning);
        let shown: SharedString = match &path {
            Some(p) => p.to_string_lossy().into_owned().into(),
            None => "not saved: no config folder".into(),
        };
        let file = div()
            .id("settings-keymap-path")
            .role(Role::Label)
            .aria_label(shown.clone())
            .test_support()
            .truncate()
            .when(path.is_some(), |d| d.font_family(mono.clone()))
            .when(path.is_none(), |d| d.text_color(theme::dim(cx)))
            .child(shown);
        let buttons = h_flex()
            .gap_2()
            .child(
                ui::secondary(Button::new("settings-keymap-open"))
                    .h(px(32.))
                    .label("Open keymap file")
                    .disabled(path.is_none())
                    .on_click(cx.listener(|_, _, window, cx| Self::open_keymap(window, cx))),
            )
            .child(
                ui::secondary(Button::new("settings-keymap-reload"))
                    .h(px(32.))
                    .label("Reload keymap")
                    .disabled(path.is_none())
                    .on_click(cx.listener(|_, _, window, cx| {
                        let notice = keymap::reload(cx).notice();
                        ui::notice(window, cx, notice);
                        cx.notify();
                    })),
            );
        let problems = keymap::problems(cx);
        let status = if problems.is_empty() {
            vec![
                div()
                    .id("settings-keymap-ok")
                    .role(Role::Label)
                    .aria_label("No problems")
                    .test_support()
                    .text_color(theme::dim(cx))
                    .child("No problems")
                    .into_any_element(),
            ]
        } else {
            problems
                .iter()
                .enumerate()
                .map(|(n, p)| {
                    let text = SharedString::from(p.message());
                    div()
                        .id(SharedString::from(format!("settings-keymap-problem-{n}")))
                        .role(Role::Label)
                        .aria_label(text.clone())
                        .test_support()
                        .text_color(warning)
                        .child(text)
                        .into_any_element()
                })
                .collect()
        };
        let commands =
            keymap::commands(cx)
                .into_iter()
                .enumerate()
                .map(|(n, (id, label, keys))| {
                    let said = if keys.is_empty() {
                        format!("{id}: unbound")
                    } else {
                        format!("{id}: {}", keys.join(", "))
                    };
                    h_flex()
                        .id(SharedString::from(format!("settings-keymap-command-{n}")))
                        .role(Role::Label)
                        .aria_label(SharedString::from(said))
                        .test_support()
                        .min_h(px(24.))
                        .gap_3()
                        .child(
                            div()
                                .w(px(COMMAND_ID_WIDTH))
                                .flex_none()
                                .truncate()
                                .font_family(mono.clone())
                                .child(id),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "settings-keymap-command-{n}-label"
                                )))
                                .test_support()
                                .flex_grow(1.)
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_color(theme::dim(cx))
                                .child(label),
                        )
                        // The label never shrinks; an id with several chords wraps them
                        // onto further lines instead of squeezing the label.
                        .child(
                            h_flex()
                                .flex_shrink(1.)
                                .min_w_0()
                                .flex_wrap()
                                .justify_end()
                                .gap_1()
                                .children(keys.iter().map(|k| ui::key_chip(chip_keys(k), cx))),
                        )
                        .into_any_element()
                });
        let mut rows = vec![
            Self::row("File", file, cx),
            Self::row("", buttons, cx),
            Self::row("Problems", v_flex().gap_1().children(status), cx),
            div()
                .pt_1()
                .text_color(theme::dim(cx))
                .child("Commands")
                .into_any_element(),
        ];
        rows.extend(commands);
        rows
    }

    // Opening never blocks the dialog: a failure (no folder, a write error) is a toast naming
    // the file.
    fn open_keymap(window: &mut Window, cx: &mut App) {
        if let Err(e) = keymap::open_keymap_file(cx) {
            let name = keymap::keymap_path(cx)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "keymap.json".into());
            ui::notice(
                window,
                cx,
                Notice {
                    text: format!("{name} could not be opened: {e}"),
                    kind: NoticeKind::Warning,
                },
            );
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.store.read(cx).get().clone();
        let store = self.store.clone();
        let appearance = ui::segmented_ids(
            "settings-theme",
            &[
                ("settings-theme-system", "System"),
                ("settings-theme-dark", "Dark"),
                ("settings-theme-light", "Light"),
            ],
            current.appearance as usize,
            move |ix, _, cx| {
                let to = [Appearance::System, Appearance::Dark, Appearance::Light][*ix];
                store.update(cx, |s, cx| s.change(cx, |v| v.appearance = to));
            },
            window,
            cx,
        );
        let store = self.store.clone();
        let density = ui::segmented_ids(
            "settings-density",
            &[
                ("settings-density-default", "Default"),
                ("settings-density-compact", "Compact"),
            ],
            usize::from(current.density == Density::Compact),
            move |ix, _, cx| {
                let to = [Density::Default, Density::Compact][*ix];
                store.update(cx, |s, cx| s.change(cx, |v| v.density = to));
            },
            window,
            cx,
        );
        let look = vec![
            Self::row("Theme", h_flex().child(appearance), cx),
            Self::row("Density", h_flex().child(density), cx),
        ];
        let fonts: Vec<AnyElement> = FontTarget::ALL
            .into_iter()
            .flat_map(|t| self.font_rows(t, cx))
            .collect();
        let not_saved = self.store.read(cx).not_saved().map(|says| {
            div()
                .id("settings-not-saved")
                .role(Role::Label)
                .aria_label(says)
                .test_support()
                .text_sm()
                .text_color(theme::dim(cx))
                .child(says)
        });
        let border = cx.theme().border;
        let keyboard = self.keyboard_rows(cx);
        let sections = [
            ("Appearance", look),
            ("Fonts", fonts),
            ("Keyboard", keyboard),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (label, rows))| {
            v_flex()
                .gap_1p5()
                .when(ix > 0, |el| el.pt_3().border_t_1().border_color(border))
                .child(ui::section_label(label, cx).pb_0p5())
                .children(rows)
        });
        let footer = confirm::footer(cx)
            .child(
                ui::secondary(Button::new("settings-reset"))
                    .h(px(32.))
                    .label("Reset to defaults")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |s, cx| s.reset(cx));
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("settings-close")
                    .h(px(32.))
                    .primary()
                    .label("Close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsViewEvent::Close))),
            );
        v_flex()
            .id("settings-dialog")
            .role(Role::Dialog)
            .aria_label("Settings")
            .test_support()
            .key_context("SettingsView")
            .track_focus(&self.focus)
            .size_full()
            .flex_1()
            .min_h_0()
            .text_sm()
            .child(
                v_flex()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .children(sections)
                    .children(not_saved),
            )
            .child(footer)
    }
}
