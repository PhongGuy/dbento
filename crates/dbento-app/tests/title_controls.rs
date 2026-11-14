//! The title bar's window controls: named for assistive technology, and still usable while a
//! dialog's backdrop covers the window.
use crate::support;
use dbento_app::window_controls::{CloseAction, Controls, over_dialogs, over_kit};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, ParentElement as _, Render, Role,
    Styled as _, TestAppContext, Window, div, px,
};
use std::cell::Cell;
use std::rc::Rc;

// A 40 px bar standing in for the shell's title bar, with every control drawn as under
// client-side decorations; the test window itself is server-decorated.
struct Host {
    closes: Rc<Cell<usize>>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let closes = Rc::clone(&self.closes);
        let on_close: CloseAction =
            Rc::new(move |_: &mut Window, _: &mut gpui_kit::App| closes.set(closes.get() + 1));
        let all = Some(Controls::ALL);
        div().size_full().child(
            div()
                .relative()
                .h(px(40.))
                .w_full()
                .children(over_kit(all, Rc::clone(&on_close), window, cx))
                .children(over_dialogs(all, on_close, window, cx)),
        )
    }
}

fn host(cx: &mut TestAppContext) -> (AnyWindowHandle, Rc<Cell<usize>>) {
    support::init(cx);
    let closes = Rc::new(Cell::new(0));
    let c = Rc::clone(&closes);
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |_, cx| {
                cx.new(|_| Host { closes: c })
            })
        })
        .unwrap();
    (win, closes)
}

#[gpui_kit::test]
fn without_a_dialog_no_copy_sits_above_dialogs(cx: &mut TestAppContext) {
    let (win, _) = host(cx);
    assert!(!support::present(cx, win, "window-controls-over-dialogs"));
}

// The kit's own controls carry no role, so the copy over them is what assistive technology
// and the pilot's session tree see, each a named button.
fn assert_named_controls(cx: &mut TestAppContext, win: AnyWindowHandle) {
    for (id, name) in [
        ("window-minimize", "Minimize"),
        ("window-maximize", "Maximize"),
        ("window-close", "Close"),
    ] {
        let (role, label) = cx
            .update_window(win, |_, w, cx| {
                w.render_frame(cx);
                let e = w.find(id);
                (e.role(), e.label().map(str::to_owned))
            })
            .unwrap();
        assert_eq!(role, Some(Role::Button), "{id}");
        assert_eq!(label.as_deref(), Some(name), "{id}");
    }
}

#[gpui_kit::test]
fn without_a_dialog_named_controls_cover_the_kit_ones(cx: &mut TestAppContext) {
    let (win, closes) = host(cx);
    assert!(support::present(cx, win, "window-controls-over-kit"));
    assert_named_controls(cx, win);
    cx.update_window(win, |_, w, cx| w.click("window-close", cx))
        .unwrap();
    assert_eq!(closes.get(), 1, "the copy runs the close flow");
}

#[gpui_kit::test]
fn with_a_dialog_open_the_controls_keep_their_names(cx: &mut TestAppContext) {
    let (win, _) = host(cx);
    cx.update_window(win, |_, w, cx| {
        w.open_dialog(cx, |d, _, _| d.title("Open").child("body"));
    })
    .unwrap();
    assert!(!support::present(cx, win, "window-controls-over-kit"));
    assert_named_controls(cx, win);
}

#[gpui_kit::test]
fn with_a_dialog_open_the_close_control_runs_the_close_flow(cx: &mut TestAppContext) {
    let (win, closes) = host(cx);
    cx.update_window(win, |_, w, cx| {
        w.open_dialog(cx, |d, _, _| d.title("Open").child("body"));
    })
    .unwrap();
    assert!(support::present(cx, win, "window-close"));
    cx.update_window(win, |_, w, cx| w.click("window-close", cx))
        .unwrap();
    assert_eq!(closes.get(), 1, "the click reached the close control");
    let open = cx
        .update_window(win, |_, w, cx| w.has_active_dialog(cx))
        .unwrap();
    assert!(open, "the click did not land on the backdrop");
}
