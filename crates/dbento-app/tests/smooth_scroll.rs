//! Wheel notches over the results grid ease into its scroll offset instead of jumping, and
//! everything the kit handled before still reaches it.
use crate::support;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::support::MANY;
use dbento_app::results_pane::ResultsPane;
use dbento_app::settings::Settings;
use dbento_app::tab_model::TabModel;
use gpui_kit::Focusable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Entity, InputEvent as _, InteractiveElement as _,
    IntoElement, MouseMoveEvent, ParentElement as _, Pixels, Render, ScrollDelta, ScrollEvent,
    Styled as _, TestAppContext, Window, div, point, px,
};

/// The results pane under a root, with an optional occluding cover on top (as a dialog or
/// popover is) that counts the wheel events it receives.
struct Probe {
    pane: Entity<ResultsPane>,
    reached_cover: Rc<Cell<u32>>,
    covered: bool,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let cover = Rc::clone(&self.reached_cover);
        div()
            .id("probe")
            .size_full()
            .relative()
            .child(self.pane.clone())
            .when(self.covered, |d| {
                d.child(
                    div()
                        .id("cover")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_scroll(move |_, _, _| cover.set(cover.get() + 1)),
                )
            })
    }
}

struct Grid {
    win: AnyWindowHandle,
    probe: Entity<Probe>,
    _fx: support::Fx,
}

/// A 1,000-row result parked at its cap (nothing streams), drawn under a probe.
fn grid(cx: &mut TestAppContext) -> Grid {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    let (win, probe) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                let pane = cx.new(|cx| ResultsPane::new(m, window, cx));
                cx.new(|_| Probe {
                    pane,
                    reached_cover: Rc::default(),
                    covered: false,
                })
            })
        })
        .unwrap();
    model.update(cx, |m, cx| {
        m.set_max_rows(Some(1_000));
        m.run(MANY.into(), cx);
    });
    support::wait_real(cx, "the park at the cap", |cx| {
        model.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    for _ in 0..2 {
        cx.update_window(win, |_, w, cx| w.render_frame(cx))
            .unwrap();
    }
    Grid {
        win,
        probe,
        _fx: fx,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn line_height(cx: &mut TestAppContext, g: &Grid) -> Pixels {
    cx.update_window(g.win, |_, w, _| w.line_height()).unwrap()
}

fn offset(cx: &mut TestAppContext, g: &Grid) -> Pixels {
    g.probe.read_with(cx, |p, cx| {
        p.pane
            .read(cx)
            .table()
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .offset()
            .y
    })
}

/// One wheel event at the grid's centre, then a frame drawn at the same clock time.
fn wheel(cx: &mut TestAppContext, g: &Grid, delta: ScrollDelta) {
    cx.update_window(g.win, |_, w, cx| {
        let at = w.find("result-grid").bounds().center();
        w.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            ScrollEvent {
                position: at,
                delta,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.render_frame(cx);
    })
    .unwrap();
}

/// Whether the eased handler takes one wheel event over the hovered grid: taking a notch
/// notifies the pane and asks for frames until it settles, so any notification counts.
fn eased_takes(cx: &mut TestAppContext, g: &Grid, delta: ScrollDelta) -> bool {
    hover(cx, g);
    let pane = g.probe.read_with(cx, |p, _| p.pane.clone());
    let notified = Rc::new(Cell::new(0u32));
    let _sub = cx.update(|cx| {
        let notified = Rc::clone(&notified);
        cx.observe(&pane, move |_, _| notified.set(notified.get() + 1))
    });
    wheel(cx, g, delta);
    frames(cx, g, 20);
    notified.get() > 0
}

/// The pointer over the grid's centre, drawn, so hover changes settle before a wheel event.
fn hover(cx: &mut TestAppContext, g: &Grid) {
    cx.update_window(g.win, |_, w, cx| {
        let at = w.find("result-grid").bounds().center();
        w.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        w.render_frame(cx);
    })
    .unwrap();
}

fn notch_down(cx: &mut TestAppContext, g: &Grid) {
    wheel(cx, g, ScrollDelta::Lines(point(0., -3.)));
}

/// Advances the clock by `dt`, runs the frame callbacks the last draw asked for and draws;
/// returns how many callbacks ran.
fn frame(cx: &mut TestAppContext, g: &Grid, dt: Duration) -> usize {
    cx.executor().advance_clock(dt);
    cx.update_window(g.win, |_, w, cx| {
        let ran = w.simulate_next_frame(cx);
        w.render_frame(cx);
        ran
    })
    .unwrap()
}

fn frames(cx: &mut TestAppContext, g: &Grid, n: usize) {
    for _ in 0..n {
        frame(cx, g, ms(7));
    }
}

/// A notch, two 7 ms frames, and the offset then, asserted to be part-way.
fn mid_motion(cx: &mut TestAppContext, g: &Grid) -> Pixels {
    let line = line_height(cx, g);
    notch_down(cx, g);
    frames(cx, g, 2);
    let y = offset(cx, g);
    assert!(y < px(0.) && y > line * -3.0, "not part-way: {y:?}");
    y
}

#[gpui_kit::test]
fn a_notch_eases_over_about_100_ms_to_three_lines(cx: &mut TestAppContext) {
    let g = grid(cx);
    let line = line_height(cx, &g);
    notch_down(cx, &g);
    let mut last = offset(cx, &g);
    assert!(last > line * -3.0, "the notch jumped: {last:?}");
    for i in 0..13 {
        frame(cx, &g, ms(7));
        let y = offset(cx, &g);
        assert!(
            y <= last && y > line * -3.0,
            "frame {i}: {y:?} after {last:?}"
        );
        last = y;
    }
    frame(cx, &g, ms(20));
    assert_eq!(offset(cx, &g), line * -3.0);
}

#[gpui_kit::test]
fn a_second_notch_mid_motion_retargets_without_a_jump(cx: &mut TestAppContext) {
    let g = grid(cx);
    let line = line_height(cx, &g);
    notch_down(cx, &g);
    frames(cx, &g, 4);
    let y = offset(cx, &g);
    assert!(y > line * -3.0, "the first notch jumped: {y:?}");
    notch_down(cx, &g);
    assert_eq!(offset(cx, &g), y, "the second notch jumped");
    frames(cx, &g, 14);
    assert!(offset(cx, &g) > line * -6.0, "settled too early");
    frame(cx, &g, ms(10));
    assert_eq!(offset(cx, &g), line * -6.0);
}

// The kit's scroll area keeps a notch at its own edge rather than hand it to the parent,
// so these pin only the eased handler: at the edge it leaves the notch alone.
#[gpui_kit::test]
fn at_the_top_a_notch_up_is_left_to_the_kit(cx: &mut TestAppContext) {
    let g = grid(cx);
    let took = eased_takes(cx, &g, ScrollDelta::Lines(point(0., 3.)));
    assert!(!took, "the eased handler took a notch up at the top");
    assert_eq!(offset(cx, &g), px(0.));
    assert!(
        eased_takes(cx, &g, ScrollDelta::Lines(point(0., -3.))),
        "a notch down from the top is the eased handler's"
    );
    assert!(
        offset(cx, &g) < px(0.),
        "the notch down did not move the grid"
    );
}

#[gpui_kit::test]
fn at_the_bottom_a_notch_down_is_left_to_the_kit(cx: &mut TestAppContext) {
    let g = grid(cx);
    let table = g
        .probe
        .read_with(cx, |p, cx| p.pane.read(cx).table().clone());
    table.update(cx, |t, cx| t.scroll_to_row(999, cx));
    frames(cx, &g, 3);
    let bottom = offset(cx, &g);
    assert!(bottom < px(-20_000.), "{bottom:?}");
    let took = eased_takes(cx, &g, ScrollDelta::Lines(point(0., -3.)));
    assert!(!took, "the eased handler took a notch down at the bottom");
    assert_eq!(offset(cx, &g), bottom);
    assert!(
        eased_takes(cx, &g, ScrollDelta::Lines(point(0., 3.))),
        "a notch up from the bottom is the eased handler's"
    );
    assert!(
        offset(cx, &g) > bottom,
        "the notch up did not move the grid"
    );
}

#[gpui_kit::test]
fn over_an_occluding_cover_the_notch_passes_through(cx: &mut TestAppContext) {
    let g = grid(cx);
    g.probe.update(cx, |p, cx| {
        p.covered = true;
        cx.notify();
    });
    frames(cx, &g, 1);
    notch_down(cx, &g);
    frames(cx, &g, 20);
    assert_eq!(offset(cx, &g), px(0.));
    let reached = g.probe.read_with(cx, |p, _| p.reached_cover.get());
    assert_eq!(reached, 1, "the grid stopped an event meant for the cover");
}

#[gpui_kit::test]
fn a_pixel_delta_cancels_the_motion_and_scrolls_as_before(cx: &mut TestAppContext) {
    let g = grid(cx);
    let y = mid_motion(cx, &g);
    wheel(cx, &g, ScrollDelta::Pixels(point(px(0.), px(-40.))));
    assert_eq!(offset(cx, &g), y + px(-40.));
    frames(cx, &g, 20);
    assert_eq!(offset(cx, &g), y + px(-40.), "the motion went on");
}

#[gpui_kit::test]
fn a_horizontal_notch_cancels_the_vertical_motion(cx: &mut TestAppContext) {
    let g = grid(cx);
    let y = mid_motion(cx, &g);
    wheel(cx, &g, ScrollDelta::Lines(point(-3., 0.)));
    frames(cx, &g, 20);
    assert_eq!(offset(cx, &g), y, "the motion went on");
}

#[gpui_kit::test]
fn scroll_to_row_in_the_middle_of_the_motion_wins(cx: &mut TestAppContext) {
    let g = grid(cx);
    mid_motion(cx, &g);
    let table = g
        .probe
        .read_with(cx, |p, cx| p.pane.read(cx).table().clone());
    table.update(cx, |t, cx| t.scroll_to_row(500, cx));
    frames(cx, &g, 20);
    let there = offset(cx, &g);
    assert!(there <= px(-400. * 28.), "{there:?}");
    frames(cx, &g, 2);
    assert_eq!(offset(cx, &g), there, "the animator fought scroll_to_row");
}

#[gpui_kit::test]
fn a_settled_grid_asks_for_no_more_frames(cx: &mut TestAppContext) {
    let g = grid(cx);
    notch_down(cx, &g);
    frames(cx, &g, 18);
    frame(cx, &g, ms(7));
    assert_eq!(frame(cx, &g, ms(7)), 0, "an idle grid requested a frame");
}

#[gpui_kit::test]
fn a_key_press_in_the_middle_of_the_motion_keeps_the_selection_in_view(cx: &mut TestAppContext) {
    // The kit only scrolls to a selected row that is out of view; an ease still running after
    // the key would then carry the newly selected row off the top.
    let g = grid(cx);
    let table = g
        .probe
        .read_with(cx, |p, cx| p.pane.read(cx).table().clone());
    cx.update_window(g.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 0, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    frames(cx, &g, 1);
    mid_motion(cx, &g);
    cx.simulate_keystrokes(g.win, "down");
    frames(cx, &g, 20);
    let row = table
        .read_with(cx, |t, _| t.selected_cell())
        .map(|(r, _)| r);
    assert_eq!(row, Some(1), "the key did not move the selection");
    let top = Settings::default().metrics().grid_row * 1.0 + offset(cx, &g);
    assert!(
        top >= px(0.),
        "the selected row ended above the view: top {top:?}"
    );
}
