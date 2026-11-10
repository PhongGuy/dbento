//! Eased wheel scrolling for the results grid and the definition editor.
//!
//! [`Animator`] is pure state: the grid feeds it wheel notches and frame times and writes the
//! offset it returns into a [`Scroller`]; [`layer`] is that wiring. Offsets follow GPUI's
//! sign: 0 at the top, negative further down, `-max` at the bottom.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::EditorState;
use gpui_kit::{
    App, Entity, HitboxBehavior, IntoElement, Pixels, ScrollDelta, ScrollEvent, ScrollHandle,
    Styled as _, Window, canvas, px,
};

/// A retargetable eased motion between scroll offsets, driven by wall-clock time so it looks
/// the same at any frame rate.
#[derive(Clone, Debug, Default)]
pub struct Animator {
    offset: f32,
    from: f32,
    target: f32,
    start: Option<Instant>,
    written: f32,
}

impl Animator {
    /// How long a notch's motion lasts.
    pub const DURATION: Duration = Duration::from_millis(100);

    /// The offset shown after the last `step` or `sync`.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// Where the current motion ends; the offset itself when settled.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// True when no motion is under way.
    pub fn is_settled(&self) -> bool {
        self.start.is_none()
    }

    /// Adopts the handle's real offset when settled, or when it differs from the offset this
    /// animator last wrote (another scroller moved it), which also ends the motion there.
    pub fn sync(&mut self, actual: f32) {
        if self.is_settled() || actual != self.written {
            *self = Self {
                offset: actual,
                from: actual,
                target: actual,
                start: None,
                written: actual,
            };
        }
    }

    /// Clamps the target and the offset shown at `now` into `[-max, 0]`, then moves the target
    /// by `delta` within it and restarts the motion from the shown offset. Returns false (the
    /// caller lets the event go on) only when target and shown offset both sit at the edge
    /// `delta` pushes toward; a notch toward an edge still being reached returns true and
    /// leaves the motion as it is.
    pub fn scroll_by(&mut self, delta: f32, max: f32, now: Instant) -> bool {
        let max = max.max(0.0);
        let shown = self.position_at(now).clamp(-max, 0.0);
        let target = self.target.clamp(-max, 0.0);
        let edge = if delta < 0.0 { -max } else { 0.0 };
        if shown == edge && target == edge {
            return false;
        }
        let next = (target + delta).clamp(-max, 0.0);
        if next == self.target {
            return true;
        }
        self.from = shown;
        self.target = next;
        self.start = Some(now);
        true
    }

    /// The offset to show at `now`, eased with `1 - (1 - u)^3` over DURATION; exactly the
    /// target, and settled, from DURATION on. Records the value as written.
    pub fn step(&mut self, now: Instant) -> f32 {
        if self.is_settled() {
            return self.offset;
        }
        self.offset = self.position_at(now);
        if self.progress(now) >= 1.0 {
            self.start = None;
        }
        self.written = self.offset;
        self.offset
    }

    /// Ends the motion where the shown offset stands.
    pub fn cancel(&mut self) {
        self.from = self.offset;
        self.target = self.offset;
        self.start = None;
    }

    /// Fraction of DURATION elapsed since the motion began; 1 when settled.
    fn progress(&self, now: Instant) -> f32 {
        self.start.map_or(1.0, |start| {
            (now.saturating_duration_since(start).as_secs_f32() / Self::DURATION.as_secs_f32())
                .min(1.0)
        })
    }

    /// Where the motion stands at `now`, computed from time alone so a notch arriving between
    /// frames retargets from the true position rather than the last stepped one.
    fn position_at(&self, now: Instant) -> f32 {
        if self.is_settled() {
            return self.offset;
        }
        let u = self.progress(now);
        if u >= 1.0 {
            return self.target;
        }
        self.from + (self.target - self.from) * (1.0 - (1.0 - u).powi(3))
    }
}

/// What a wheel layer eases: a vertical offset it reads and writes, its scroll range, and the
/// line height a notch is measured in.
pub trait Scroller: Clone + 'static {
    /// The vertical offset shown now.
    fn offset_y(&self, cx: &App) -> f32;
    /// Shows `y` from this frame on.
    fn set_offset_y(&self, y: f32, cx: &mut App);
    /// How far down it scrolls, as a positive distance.
    fn max_y(&self, cx: &App) -> f32;
    /// The height of one wheel line.
    fn line_height(&self, window: &Window, _cx: &App) -> Pixels {
        window.line_height()
    }
}

impl Scroller for ScrollHandle {
    fn offset_y(&self, _: &App) -> f32 {
        f32::from(self.offset().y)
    }
    fn set_offset_y(&self, y: f32, _: &mut App) {
        let mut offset = self.offset();
        offset.y = px(y);
        self.set_offset(offset);
    }
    fn max_y(&self, _: &App) -> f32 {
        f32::from(self.max_offset().y)
    }
}

/// A kit editor's viewport. The kit keeps its scroll range private and clamps an offset at
/// its next layout, so the range is open here: a motion past the end stops where the clamp
/// moves the offset off the value written, as [`Animator::sync`] treats another scroller.
impl Scroller for Entity<EditorState> {
    fn offset_y(&self, cx: &App) -> f32 {
        f32::from(self.read(cx).scroll_offset().y)
    }
    fn set_offset_y(&self, y: f32, cx: &mut App) {
        self.update(cx, |editor, cx| {
            let mut offset = editor.scroll_offset();
            offset.y = px(y);
            editor.set_scroll_offset(offset, cx);
        });
    }
    fn max_y(&self, _: &App) -> f32 {
        f32::INFINITY
    }
    fn line_height(&self, window: &Window, cx: &App) -> Pixels {
        self.read(cx)
            .line_height()
            .unwrap_or_else(|| window.line_height())
    }
}

/// The wheel layer of a scrolled element. As the first child of the element that holds the
/// scrolled view, its prepaint steps the animator before the view lays out, and its paint
/// registers a Capture wheel listener before the view's own, so it sees each notch first.
pub fn layer(scroller: impl Scroller, animator: Rc<RefCell<Animator>>) -> impl IntoElement {
    let step_scroller = scroller.clone();
    let step_animator = Rc::clone(&animator);
    canvas(
        move |bounds, window, cx| {
            // Follow any offset another scroller set, then write this frame's eased offset.
            let mut a = step_animator.borrow_mut();
            a.sync(step_scroller.offset_y(cx));
            if !a.is_settled() {
                let y = a.step(cx.background_executor().now());
                step_scroller.set_offset_y(y, cx);
                if !a.is_settled() {
                    window.request_animation_frame();
                }
            }
            window.insert_hitbox(bounds, HitboxBehavior::Normal)
        },
        move |_, hitbox, window, cx| {
            // Same line height and occlusion test as the view's own handler, so a notch covers
            // the same distance as before and nothing under a dialog or popover moves.
            let line_height = scroller.line_height(window, cx);
            let view = window.current_view();
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                if !phase.capture() || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let mut a = animator.borrow_mut();
                a.sync(scroller.offset_y(cx));
                // A NaN or zero line delta is no notch; the animator does not guard against one.
                let vertical_notch = matches!(event.delta,
                    ScrollDelta::Lines(l) if l.x == 0. && l.y != 0. && l.y.is_finite());
                if !vertical_notch {
                    // Trackpads, horizontal and diagonal deltas stay with the kit.
                    a.cancel();
                    return;
                }
                let delta = event.delta.pixel_delta(line_height).y;
                let max = scroller.max_y(cx);
                let now = cx.background_executor().now();
                if a.scroll_by(f32::from(delta), max, now) {
                    cx.notify(view);
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .size_full()
}
