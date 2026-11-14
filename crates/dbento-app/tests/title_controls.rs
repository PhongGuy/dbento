//! The title bar's window controls stay usable while a dialog's backdrop covers the window.
use crate::support;
use dbento_app::window_controls::{Controls, over_dialogs};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, ParentElement as _, Render,
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
        let on_close =
            Rc::new(move |_: &mut Window, _: &mut gpui_kit::App| closes.set(closes.get() + 1));
        let all = Some(Controls::ALL);
        div().size_full().child(
            div()
                .relative()
                .h(px(40.))
                .w_full()
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
fn without_a_dialog_the_kit_controls_are_left_alone(cx: &mut TestAppContext) {
    let (win, _) = host(cx);
    assert!(!support::present(cx, win, "window-close"));
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
