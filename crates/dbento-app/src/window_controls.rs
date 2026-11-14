//! A copy of the title bar's minimise, maximise and close buttons that stays clickable while
//! a dialog is open. The kit's dialog backdrop covers the whole window, title bar included,
//! and offers no inset, so this copy is painted above every dialog layer over the kit's own.
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, TITLE_BAR_HEIGHT, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Decorations, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, WindowControlArea,
    deferred, div,
};
use std::rc::Rc;

/// The title bar's close action, shared by the kit's button and the copy above dialogs.
pub type CloseAction = Rc<dyn Fn(&mut Window, &mut App)>;

// Clear of any realistic stack of dialogs, which take priority 10 plus their depth.
const ABOVE_DIALOGS: usize = 1_000;

/// The controls a window draws itself beside the always-present close button.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    /// The window manager can minimise the window.
    pub minimize: bool,
    /// The window manager can maximise and restore the window.
    pub maximize: bool,
}

impl Controls {
    /// Minimise, maximise and close.
    pub const ALL: Controls = Controls {
        minimize: true,
        maximize: true,
    };

    /// What the kit's title bar draws in `window`: nothing on macOS or under server-side
    /// decorations, where the system draws the controls; else what the window manager offers.
    pub fn drawn(window: &Window) -> Option<Controls> {
        if cfg!(target_os = "macos") || cfg!(target_family = "wasm") {
            return None;
        }
        if cfg!(target_os = "linux")
            && !matches!(window.window_decorations(), Decorations::Client { .. })
        {
            return None;
        }
        let offered = window.window_controls();
        Some(Controls {
            minimize: offered.minimize,
            maximize: offered.maximize,
        })
    }
}

#[derive(Clone, Copy)]
enum Button {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// While a dialog is open, the `controls` pinned to the top right of the caller's relative
/// title bar; `on_close` is the title bar's close action. None without a dialog or controls.
pub fn over_dialogs(
    controls: Option<Controls>,
    on_close: CloseAction,
    window: &mut Window,
    cx: &mut App,
) -> Option<impl IntoElement + use<>> {
    let controls = controls.filter(|_| window.has_active_dialog(cx))?;
    let zoom = if window.is_maximized() {
        Button::Restore
    } else {
        Button::Maximize
    };
    let row = div()
        .id("window-controls-over-dialogs")
        .test_support()
        .absolute()
        .top_0()
        .right_0()
        .h_full()
        .flex()
        .occlude()
        .when(controls.minimize, |d| {
            d.child(button(Button::Minimize, &on_close, cx))
        })
        .when(controls.maximize, |d| d.child(button(zoom, &on_close, cx)))
        .child(button(Button::Close, &on_close, cx));
    Some(deferred(row).with_priority(ABOVE_DIALOGS))
}

// One control styled as the kit's, acting as the kit's does on each platform.
fn button(kind: Button, on_close: &CloseAction, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (id, icon, area) = match kind {
        Button::Minimize => (
            "window-minimize",
            IconName::WindowMinimize,
            WindowControlArea::Min,
        ),
        Button::Maximize => (
            "window-maximize",
            IconName::WindowMaximize,
            WindowControlArea::Max,
        ),
        Button::Restore => (
            "window-restore",
            IconName::WindowRestore,
            WindowControlArea::Max,
        ),
        Button::Close => (
            "window-close",
            IconName::WindowClose,
            WindowControlArea::Close,
        ),
    };
    let (hover_fg, hover_bg, active_bg): (Hsla, Hsla, Hsla) = match kind {
        Button::Close => (theme.danger_foreground, theme.danger, theme.danger_active),
        Button::Minimize | Button::Maximize | Button::Restore => (
            theme.secondary_foreground,
            theme.secondary_hover,
            theme.secondary_active,
        ),
    };
    let on_close = Rc::clone(on_close);
    div()
        .id(id)
        .test_support()
        .flex()
        .flex_shrink_0()
        .w(TITLE_BAR_HEIGHT)
        .h_full()
        .justify_center()
        .items_center()
        .text_color(theme.foreground)
        .hover(|s| s.bg(hover_bg).text_color(hover_fg))
        .active(|s| s.bg(active_bg).text_color(hover_fg))
        .when(cfg!(target_os = "windows"), |d| d.window_control_area(area))
        .when(!cfg!(target_os = "windows"), |d| {
            d.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match kind {
                    Button::Minimize => window.minimize_window(),
                    Button::Maximize | Button::Restore => window.zoom_window(),
                    Button::Close => on_close(window, cx),
                }
            })
        })
        .child(Icon::new(icon).small())
}
