//! One connection row as every connection picker draws it: the tag dot, the name, the
//! engine and the read-only lock, so the query toolbar, history filter and pick dialog agree.

use dbento_core::{Color, ConnectionConfig, Workspace};
use gpui_kit::{
    App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, WeakEntity, div, px,
};

use crate::sidebar::{Sidebar, color_name, tag_color};
use crate::theme;
use crate::ui::{self, Icons};

/// What a picker row shows of one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionPick {
    /// The connection name.
    pub name: String,
    /// The engine as people name it, "mariadb" included.
    pub engine: &'static str,
    /// The tag colour, own or inherited from its folder.
    pub color: Option<Color>,
    /// Whether the connection is read-only by its config.
    pub read_only: bool,
}

impl ConnectionPick {
    /// The row for `c`.
    pub fn of(c: &ConnectionConfig) -> Self {
        ConnectionPick {
            name: c.name().to_owned(),
            engine: c.engine_label(),
            color: c.effective_color(),
            read_only: c.read_only(),
        }
    }
}

/// The rows a connection picker offers: the explorer's known connections, children included,
/// or `ws`'s declared ones when there is no explorer.
pub fn pickable_connections(
    ws: &Workspace,
    sidebar: Option<&WeakEntity<Sidebar>>,
    cx: &App,
) -> Vec<ConnectionPick> {
    match sidebar.and_then(WeakEntity::upgrade) {
        Some(s) => (s.read(cx).tree().known_connections().iter())
            .map(ConnectionPick::of)
            .collect(),
        None => ws.connections().iter().map(ConnectionPick::of).collect(),
    }
}

/// An 8 px slot, `"<prefix>-slot"`, that every row keeps so names line up; a tagged row fills
/// it with the dot `"<prefix>-dot"` labelled by its colour.
pub fn dot_slot(prefix: &str, n: usize, color: Option<Color>) -> impl IntoElement {
    let slot = div()
        .id(ElementId::named_usize(format!("{prefix}-slot"), n))
        .test_support()
        .size(px(8.))
        .flex_none();
    let dot = color.map(|c| {
        div()
            .id(ElementId::named_usize(format!("{prefix}-dot"), n))
            .aria_label(color_name(c))
            .test_support()
            .size_full()
            .rounded_full()
            .bg(tag_color(c))
    });
    slot.children(dot)
}

/// The lock a read-only connection ends its row with, as `"<prefix>-lock"`, as the explorer
/// draws it.
pub fn lock(prefix: &str, n: usize, cx: &App) -> impl IntoElement {
    div()
        .id(ElementId::named_usize(format!("{prefix}-lock"), n))
        .aria_label("read-only")
        .test_support()
        .flex_none()
        .child(ui::icon(Icons::Lock, px(14.), theme::dim(cx)))
}

/// The row's content: slot, name, dim engine as `"<prefix>-engine"` when `engine`, then the
/// lock pushed to the end.
pub fn pick_row(prefix: &str, n: usize, pick: &ConnectionPick, engine: bool, cx: &App) -> Div {
    let label = engine.then(|| {
        div()
            .id(ElementId::named_usize(format!("{prefix}-engine"), n))
            .aria_label(pick.engine)
            .test_support()
            .text_color(theme::dim(cx))
            .child(pick.engine)
    });
    div()
        .flex()
        .flex_1()
        .items_center()
        .gap_2()
        .child(dot_slot(prefix, n, pick.color))
        .child(SharedString::from(pick.name.clone()))
        .children(label)
        .child(div().flex_1())
        .children(pick.read_only.then(|| lock(prefix, n, cx)))
}
