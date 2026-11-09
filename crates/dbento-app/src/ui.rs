//! The shared look: the app's icon list, its asset source, and the chip, key-chip,
//! section-label and icon helpers every surface draws with.

use std::borrow::Cow;
use std::rc::Rc;

use gpui_kit::assets::{Assets, IconName};
use gpui_kit::base::ObservedElement;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Size, ThemeStyled as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AssetSource, Div, ElementId, Entity, FontWeight, Global, Hsla, InteractiveElement,
    Interactivity, IntoElement as _, ParentElement as _, Pixels, Role, SharedString, Stateful,
    StatefulInteractiveElement, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::keymap::chip_keys;
use crate::theme;

/// Every icon the app draws, as Lucide catalog names. Nothing outside this list is
/// guaranteed to be served by [`AppAssets`].
pub struct Icons;

#[allow(missing_docs, non_upper_case_globals)]
impl Icons {
    pub const Eye: IconName = IconName::Eye;
    pub const EyeOff: IconName = IconName::EyeOff;
    pub const Play: IconName = IconName::Play;
    pub const Square: IconName = IconName::Square;
    pub const Search: IconName = IconName::Search;
    pub const Plus: IconName = IconName::Plus;
    pub const Minus: IconName = IconName::Minus;
    pub const Settings: IconName = IconName::Settings;
    pub const Folder: IconName = IconName::Folder;
    pub const FolderOpen: IconName = IconName::FolderOpen;
    pub const ChevronDown: IconName = IconName::ChevronDown;
    pub const ChevronRight: IconName = IconName::ChevronRight;
    pub const ChevronLeft: IconName = IconName::ChevronLeft;
    pub const CircleCheck: IconName = IconName::CircleCheck;
    pub const CircleX: IconName = IconName::CircleX;
    pub const TriangleAlert: IconName = IconName::TriangleAlert;
    pub const Bell: IconName = IconName::Bell;
    pub const PanelLeft: IconName = IconName::PanelLeft;
    pub const PanelRight: IconName = IconName::PanelRight;
    pub const Bot: IconName = IconName::Bot;
    pub const X: IconName = IconName::X;
    pub const FileText: IconName = IconName::FileText;
    pub const RotateCw: IconName = IconName::RotateCw;
    pub const Database: IconName = IconName::Database;
    pub const Table2: IconName = IconName::Table2;
    pub const Columns3: IconName = IconName::Columns3;
    pub const KeyRound: IconName = IconName::KeyRound;
    pub const Lock: IconName = IconName::Lock;
    pub const Clock: IconName = IconName::Clock;
    pub const Layers: IconName = IconName::Layers;
    pub const Server: IconName = IconName::Server;
    pub const FileBox: IconName = IconName::FileBox;
    pub const FileCode: IconName = IconName::FileCode;
    pub const ListTree: IconName = IconName::ListTree;
    pub const FastForward: IconName = IconName::FastForward;
    pub const Save: IconName = IconName::Save;
    pub const Rows3: IconName = IconName::Rows3;
    pub const CircleStop: IconName = IconName::CircleStop;
    pub const SunMoon: IconName = IconName::SunMoon;
    pub const Sun: IconName = IconName::Sun;
    pub const Moon: IconName = IconName::Moon;
    pub const FolderPlus: IconName = IconName::FolderPlus;
    pub const FolderSync: IconName = IconName::FolderSync;
    pub const Pencil: IconName = IconName::Pencil;
    pub const Trash: IconName = IconName::Trash;
    pub const WandSparkles: IconName = IconName::WandSparkles;
    pub const Funnel: IconName = IconName::Funnel;
    pub const ArrowUp: IconName = IconName::ArrowUp;
    pub const ArrowDown: IconName = IconName::ArrowDown;
    pub const Link: IconName = IconName::Link;
    pub const Ban: IconName = IconName::Ban;
    pub const Copy: IconName = IconName::Copy;
    pub const CopyPlus: IconName = IconName::CopyPlus;
    pub const RotateCcw: IconName = IconName::RotateCcw;
    pub const Undo2: IconName = IconName::Undo2;
    pub const Redo2: IconName = IconName::Redo2;
    pub const Download: IconName = IconName::Download;
    pub const Key: IconName = IconName::Key;
    pub const Hash: IconName = IconName::Hash;
    pub const ShieldCheck: IconName = IconName::ShieldCheck;
    pub const Zap: IconName = IconName::Zap;
    pub const Shapes: IconName = IconName::Shapes;
    pub const SquareFunction: IconName = IconName::SquareFunction;

    /// Every const above; the registry test loads each one through [`AppAssets`].
    pub const ALL: &'static [IconName] = &[
        Self::Eye,
        Self::EyeOff,
        Self::Play,
        Self::Square,
        Self::Search,
        Self::Plus,
        Self::Settings,
        Self::Folder,
        Self::FolderOpen,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::ChevronLeft,
        Self::CircleCheck,
        Self::CircleX,
        Self::TriangleAlert,
        Self::Bell,
        Self::PanelLeft,
        Self::PanelRight,
        Self::Bot,
        Self::X,
        Self::FileText,
        Self::RotateCw,
        Self::Database,
        Self::Table2,
        Self::Columns3,
        Self::KeyRound,
        Self::Lock,
        Self::Clock,
        Self::Layers,
        Self::Server,
        Self::FileBox,
        Self::FileCode,
        Self::ListTree,
        Self::FastForward,
        Self::Save,
        Self::Rows3,
        Self::CircleStop,
        Self::SunMoon,
        Self::Sun,
        Self::Moon,
        Self::FolderPlus,
        Self::FolderSync,
        Self::Pencil,
        Self::Trash,
        Self::WandSparkles,
        Self::Funnel,
        Self::ArrowUp,
        Self::ArrowDown,
        Self::Link,
        Self::Ban,
        Self::Copy,
        Self::CopyPlus,
        Self::RotateCcw,
        Self::Undo2,
        Self::Redo2,
        Self::Download,
        Self::Key,
        Self::Hash,
        Self::ShieldCheck,
        Self::Zap,
        Self::Shapes,
        Self::SquareFunction,
    ];
}

mod extra {
    #![allow(missing_docs)]
    use gpui_kit::assets::icon_assets;
    icon_assets!(pub ExtraIcons, [
        X, Database, Table2, Columns3, KeyRound, Lock, Clock, Layers, Server, FileBox,
        FileCode, FastForward, Save, Rows3, CircleStop, SunMoon, FolderPlus, FolderSync,
        Pencil, Trash, WandSparkles, Funnel, Link, CopyPlus, RotateCcw, Download, Key, Hash,
        ShieldCheck, Zap, Shapes, SquareFunction, ListTree,
    ]);
}
/// The names in [`Icons`] that the kit's default bundle lacks; only these are embedded.
pub use extra::ExtraIcons;

/// The asset path of the app icon drawn in the title bar.
pub const APP_ICON: &str = "brand/dbento-32.svg";
const APP_ICON_SVG: &[u8] = include_bytes!("../../../assets/icon/dbento-32.svg");

/// The app's asset source: the extra catalog icons, the app icon, then the kit's
/// default bundle. A path none of them hold loads as `Ok(None)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(APP_ICON_SVG)));
        }
        // The kit's bundle errors on a path it lacks; a miss here is just absent.
        Ok(Assets.load(path).ok().flatten())
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut all = ExtraIcons.list(path)?;
        if APP_ICON.starts_with(path) {
            all.push(APP_ICON.into());
        }
        all.extend(Assets.list(path)?);
        Ok(all)
    }
}

/// A 22 px label chip on the raised fill with a hairline edge, named by its label for
/// tests and assistive tech. Callers add icons with `.child`. The observed wrapper is the
/// plain `Stateful<Div>` outside test builds.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    let label = label.into();
    chip_base(cx)
        .id(id)
        .aria_label(label.clone())
        .test_support()
        .child(label)
}

/// The chip's box alone, for chips whose content leads with an icon or carries no id.
pub fn chip_base(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .h(px(22.))
        .px_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_xs()
}

/// The loud "PRODUCTION" chip, filled in danger; callers give it an id if it needs one.
pub fn production_chip(cx: &App) -> Div {
    let theme = cx.theme();
    chip_base(cx)
        .bg(theme.danger)
        .border_color(theme.danger)
        .text_color(theme.danger_foreground)
        .font_weight(FontWeight::SEMIBOLD)
        .child("PRODUCTION")
}

/// An 18 px mono key hint such as "Ctrl O": one boxed key per space-separated part, in dim
/// with a hairline edge. The row itself has no padding or border.
pub fn key_chip(keys: impl Into<SharedString>, cx: &App) -> Div {
    let (text, edge) = (theme::dim(cx), cx.theme().border);
    key_chip_colored(keys, text, edge, cx)
}

/// One menu row: the muted 14 px `icon` as element `"<id>-icon"`, `label`, then `keys` as a
/// key chip, element `"<id>-keys"` labelled with its chip text. No `icon` leaves its slot blank
/// so the row lines up with glyphed rows; no `keys`, no chip element.
pub fn menu_item(
    id: &'static str,
    icon: Option<IconName>,
    label: impl Into<SharedString>,
    keys: Option<SharedString>,
    cx: &App,
) -> PopupMenuItem {
    menu_row(id, Some(icon), label.into(), keys, cx)
}

/// [`menu_item`] without a glyph slot, for a menu none of whose rows has a glyph.
pub fn text_menu_item(
    id: &'static str,
    label: impl Into<SharedString>,
    keys: Option<SharedString>,
    cx: &App,
) -> PopupMenuItem {
    menu_row(id, None, label.into(), keys, cx)
}

// The row draws its own glyph: the kit forces menu icons to 12 px whatever their size.
fn menu_row(
    id: &'static str,
    slot: Option<Option<IconName>>,
    label: SharedString,
    keys: Option<SharedString>,
    cx: &App,
) -> PopupMenuItem {
    let muted = cx.theme().muted_foreground;
    PopupMenuItem::element(move |_, cx| {
        let chip = keys.as_ref().map(|keys| {
            let text = chip_keys(keys);
            key_chip(text.clone(), cx)
                .id(SharedString::from(format!("{id}-keys")))
                .aria_label(text)
                .test_support()
        });
        let glyph = slot.map(|glyph| {
            div()
                .id(SharedString::from(format!("{id}-icon")))
                .test_support()
                .flex_none()
                .size(px(14.))
                .children(glyph.map(|g| icon(g, px(14.), muted)))
        });
        div()
            .flex()
            .flex_1()
            .items_center()
            .gap_1()
            .children(glyph)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label.clone())
                    .children(chip),
            )
            .id(id)
            .aria_label(label.clone())
            .test_support()
    })
}

/// A key chip's height and that of each key box in it.
const KEY_HEIGHT: Pixels = px(18.);
/// The return glyph's size and line box: JetBrains Mono draws U+23CE small, so it is drawn a
/// size up on a line box that still fits inside a key box's edges.
const RETURN_GLYPH: (Pixels, Pixels) = (px(15.), px(14.));

/// [`key_chip`] with the key text and box edge in the given colours.
pub fn key_chip_colored(keys: impl Into<SharedString>, text: Hsla, edge: Hsla, cx: &App) -> Div {
    let theme = cx.theme();
    let keys: SharedString = keys.into();
    let boxes = keys.split(' ').map(|k| {
        let part = div()
            .flex()
            .items_center()
            .h(KEY_HEIGHT)
            .px_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(edge)
            .child(SharedString::from(k.to_owned()));
        if k == "\u{23CE}" {
            part.text_size(RETURN_GLYPH.0).line_height(RETURN_GLYPH.1)
        } else {
            part
        }
    });
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .h(KEY_HEIGHT)
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .text_color(text)
        .children(boxes)
}

/// How one option of a [`segmented`] row is painted.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentColors {
    /// The option's background.
    pub fill: Hsla,
    /// Its 1 px edge.
    pub border: Hsla,
    /// Its label.
    pub text: Hsla,
}

/// The selected option sits on the selected-row fill with a `primary` edge, which reaches 3:1
/// on the dialog's `popover` in both modes; the rest are bare muted labels.
pub fn segment_colors(selected: bool, cx: &App) -> SegmentColors {
    let theme = cx.theme();
    if selected {
        SegmentColors {
            fill: theme.list_active,
            border: theme.primary,
            text: theme.foreground,
        }
    } else {
        let clear = gpui_kit::transparent_black();
        SegmentColors {
            fill: clear,
            border: clear,
            text: theme.muted_foreground,
        }
    }
}

/// A row of options; option `ix` has element id `ix`, role Tab, its label as aria label and
/// `aria_selected(ix == selected)`; a click calls `on_select(&ix, ..)`. The row is one tab
/// stop with the kit's focus ring; while focused, Left/Up and Right/Down step the choice
/// without wrapping and Home/End jump to the ends, each through `on_select`.
pub fn segmented(
    id: impl Into<ElementId>,
    labels: &[&'static str],
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> ObservedElement<Stateful<Div>> {
    let options = labels
        .iter()
        .enumerate()
        .map(|(ix, &label)| (ElementId::from(ix), label))
        .collect();
    segmented_row(id.into(), options, selected, on_select, window, cx)
}

/// `segmented` with option ids taken from `options[i].0` and labels from `options[i].1`.
pub fn segmented_ids(
    id: impl Into<ElementId>,
    options: &[(&'static str, &'static str)],
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> ObservedElement<Stateful<Div>> {
    let options = options
        .iter()
        .map(|&(option, label)| (ElementId::from(option), label))
        .collect();
    segmented_row(id.into(), options, selected, on_select, window, cx)
}

fn segmented_row(
    id: ElementId,
    labels: Vec<(ElementId, &'static str)>,
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> ObservedElement<Stateful<Div>> {
    // The handle lives in keyed state under the row's id, so focus survives re-renders; an
    // explicit handle ignores the element's `tab_stop`, so the handle itself carries it.
    let focus = window
        .use_keyed_state(id.clone(), cx, |_, cx| cx.focus_handle().tab_stop(true))
        .read(cx)
        .clone();
    let focused = focus.is_focused(window);
    let theme = cx.theme();
    let on_select = Rc::new(on_select);
    let last = labels.len().saturating_sub(1);
    let options = labels.into_iter().enumerate().map(|(ix, (option, label))| {
        let colors = segment_colors(ix == selected, cx);
        let on_select = on_select.clone();
        div()
            .id(option)
            .flex()
            .items_center()
            .h(px(24.))
            .px_2p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(colors.border)
            .bg(colors.fill)
            .text_sm()
            .text_color(colors.text)
            .cursor_pointer()
            .when(ix != selected, |o| {
                o.hover(|s| s.text_color(theme.foreground))
            })
            .role(Role::Tab)
            .aria_label(label)
            .aria_selected(ix == selected)
            .on_click(move |_, window, cx| on_select(&ix, window, cx))
            .test_support()
            .child(label)
    });
    let on_key = on_select.clone();
    div()
        .id(id)
        .test_support()
        .track_focus(&focus)
        .on_key_down(move |event, window, cx| {
            let to = match event.keystroke.key.as_str() {
                "left" | "up" => selected.saturating_sub(1),
                "right" | "down" => (selected + 1).min(last),
                "home" => 0,
                "end" => last,
                _ => return,
            };
            cx.stop_propagation();
            if to != selected {
                on_key(&to, window, cx);
            }
        })
        // Options that do not fit the row's column flow onto another line, never past its edge.
        .flex()
        .flex_wrap()
        .max_w_full()
        .items_center()
        .gap_0p5()
        .p(px(2.))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .children(options)
        .when(focused, |row| row.focus_ring_style(window, cx))
}

/// A section heading: SemiBold 11 px, uppercased, in dim.
pub fn section_label(text: &str, cx: &App) -> Div {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::dim(cx))
        .child(text.to_uppercase())
}

/// [`section_label`] letter-spaced by [`TRACKING`] em, as element `id` labelled with its text.
/// GPUI has no letter spacing, so each letter is laid out on its own with the tracking as gap.
pub fn tracked_label(id: &'static str, text: &str, cx: &App) -> ObservedElement<Stateful<Div>> {
    let upper = text.to_uppercase();
    let letters = upper
        .chars()
        .map(|c| div().child(SharedString::from(c.to_string())));
    div()
        .flex()
        .flex_none()
        .gap(px(11. * TRACKING))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::dim(cx))
        .children(letters)
        .id(id)
        .aria_label(SharedString::from(upper))
        .test_support()
}

/// The letter spacing of a tracked section label, in em: DBFlux's about 0.12.
pub const TRACKING: f32 = 0.12;

/// A side panel's 36 px header row: the section label, then a spacer that pushes
/// the buttons callers append to the right edge.
pub fn panel_header(label: &str, cx: &App) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(36.))
        .pl_3()
        .pr_2()
        .child(section_label(label, cx))
        .child(div().flex_1())
}

/// A side panel's search box under `id`: a 28 px filled input with a Search prefix.
pub fn search_input(
    id: impl Into<ElementId>,
    state: &Entity<InputState>,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    div()
        .id(id)
        .test_support()
        .flex_none()
        .px_2()
        .pb_2()
        // Extra-small text in a 28 px box; `h` is multi-line only on Input, hence `min_h`.
        .child(
            Input::new(state)
                .xsmall()
                .min_h(px(28.))
                .px_2()
                .bg(theme::editor_fill(cx))
                .prefix(icon(Icons::Search, px(14.), cx.theme().muted_foreground)),
        )
}

/// A catalog icon at `size` in `color`.
pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Icon {
    Icon::new(name)
        .with_size(Size::Size(size))
        .text_color(color)
}

/// `button` as a secondary button: the neutral fill with a 1 px edge in the input-border
/// colour, so it reads as a button beside a filled primary. Every secondary button uses it.
pub fn secondary(button: Button) -> Button {
    // The kit's default variant draws that edge in `theme.input` in every state and fades it
    // when disabled; the theme gives it the secondary fill.
    button.with_variant(ButtonVariant::Default)
}

/// `element` with AT-SPI id `id`, for a widget such as the kit's `Button` that lacks
/// `StatefulInteractiveElement`; gpui-pilot's session backend finds elements by this id.
pub fn accessibility_id<E: InteractiveElement>(element: E, id: &'static str) -> E {
    Lent(element).accessibility_id(id).0
}

/// Lends the stateful builder methods to any interactive element.
struct Lent<E>(E);

impl<E: InteractiveElement> InteractiveElement for Lent<E> {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl<E: InteractiveElement> StatefulInteractiveElement for Lent<E> {}

/// How a [`Notice`] reads: the kit's info, success or warning toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Neutral information.
    Info,
    /// Something the user asked for landed.
    Success,
    /// Something needs the user's attention.
    Warning,
}

/// The number of the newest notice pushed in this app, in any window.
#[derive(Default)]
struct NoticeSeq(u64);
impl Global for NoticeSeq {}

/// A short message shown on the window's toast layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The text, drawn once and used as the element's aria label.
    pub text: String,
    /// The toast's look.
    pub kind: NoticeKind,
}

/// Pushes `notice` onto the window's toast layer, keyed `Notification::id1::<Notice>(text)`
/// so the same text twice stays one toast. Its content is element `notice` while it is the
/// newest, `("notice", n)` after, with `Role::Status`, labelled by the text; the kit hides it
/// after 5 s.
pub fn notice(window: &mut Window, cx: &mut App, notice: Notice) {
    let kind = match notice.kind {
        NoticeKind::Info => NotificationType::Info,
        NoticeKind::Success => NotificationType::Success,
        NoticeKind::Warning => NotificationType::Warning,
    };
    let text = SharedString::from(notice.text);
    // Toasts stack, so only the newest takes the id `notice`; older ones are `(notice, n)`
    // and every lookup stays unambiguous. Older toasts also expire first.
    let seq = {
        let newest = cx.default_global::<NoticeSeq>();
        newest.0 += 1;
        newest.0
    };
    // `message` stays unset so the text draws once, inside the observable content.
    let toast = Notification::new()
        .id1::<Notice>(text.clone())
        .with_type(kind)
        .content(move |_, _, cx| {
            let id = if cx.global::<NoticeSeq>().0 == seq {
                ElementId::from("notice")
            } else {
                ElementId::from(("notice", seq as usize))
            };
            div()
                .id(id)
                .role(Role::Status)
                .aria_label(text.clone())
                .test_support()
                .child(text.clone())
                .into_any_element()
        });
    window.push_notification(toast, cx);
}
