//! The row inspector: a right dock that follows the active tab's displayed result and current
//! row, headed by the row's position, its browsed table and its identity, with the row as a
//! stacked form, on an editable browse tab Edit, Duplicate and Delete buttons, and on a loaded
//! browse row its References: links to the rows its foreign keys point at or come from.

use dbento_core::{FkDirection, RelationRef};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window, div, px, relative,
};

use crate::keymap::{NavBack, NavForward, RedoEdit, SaveEdits, UndoEdit, keys_for, plus_keys};
use crate::query_tab::QueryTab;
use crate::record::{key_text, record_at, record_fields, references};
use crate::record_form::RecordForm;
use crate::shell::panel_box;
use crate::staged::{GridRow, edit_value};
use crate::tab_model::SaveState;
use crate::theme;
use crate::ui::{self, Icons};

/// What the inspector asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InspectorEvent {
    /// The close button was clicked.
    Close,
    /// Open `relation` on `connection` filtered by `filter`.
    Follow {
        /// The followed tab's connection name.
        connection: String,
        /// The far table of the link.
        relation: RelationRef,
        /// The `key_filter` WHERE text for the far table.
        filter: String,
    },
}

/// The row inspector's view.
pub struct Inspector {
    tab: Option<Entity<QueryTab>>,
    /// The followed tab's row as a form; a new tab gets a new form.
    form: Option<Entity<RecordForm>>,
    /// (run, displayed result, inserted rows) last seen; a change re-addresses every row.
    seen: Option<(u64, Option<usize>, usize)>,
    // The panel's own focus, so Alt+Left and Alt+Right reach it outside its inputs.
    focus: FocusHandle,
    // The followed tab's model, results pane and grid state; any change re-renders.
    _follows: Vec<Subscription>,
}

impl EventEmitter<InspectorEvent> for Inspector {}

impl Focusable for Inspector {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Inspector {
    /// An inspector following no tab.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Inspector {
            tab: None,
            form: None,
            seen: None,
            focus: cx.focus_handle(),
            _follows: Vec::new(),
        }
    }

    /// Follows `tab`'s results pane and model, first staging the old form's open field; a no-op
    /// when it already follows that tab.
    pub fn follow(
        &mut self,
        tab: Option<Entity<QueryTab>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab.as_ref().map(Entity::entity_id) == tab.as_ref().map(Entity::entity_id) {
            return;
        }
        // The old form goes with the swap, and a key or an unfocused window gives it no blur,
        // so its open field stages here as leaving the form would stage it.
        if let Some(old) = &self.form {
            old.update(cx, |f, cx| f.stage_open(window, cx));
        }
        // Replacing the list drops the old tab's subscriptions.
        (self._follows, self.form) = match &tab {
            Some(t) => {
                let (model, pane) = (
                    t.read(cx).model().clone(),
                    t.read(cx).results_pane().clone(),
                );
                let table = pane.read(cx).table().clone();
                let form = cx.new(|cx| RecordForm::inspector(model.clone(), window, cx));
                pane.update(cx, |p, cx| p.attach_form(&form, cx));
                let follows = vec![
                    cx.observe_in(&model, window, |this, _, window, cx| {
                        this.refresh(window, cx)
                    }),
                    cx.observe_in(&pane, window, |this, _, window, cx| {
                        this.refresh(window, cx)
                    }),
                    cx.observe_in(&table, window, |this, _, window, cx| {
                        this.refresh(window, cx)
                    }),
                ];
                (follows, Some(form))
            }
            None => (Vec::new(), None),
        };
        self.tab = tab;
        self.seen = None;
        self.refresh(window, cx);
    }

    /// The inspector's form, while it follows a tab.
    pub fn form(&self) -> Option<&Entity<RecordForm>> {
        self.form.as_ref()
    }

    /// Points the form at the followed row. A new run, result or inserted-row count first
    /// drops its open field unstaged, as the results pane does, since its row would address
    /// different data.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(tab), Some(form)) = (&self.tab, &self.form) else {
            return cx.notify();
        };
        let pane = tab.read(cx).results_pane().read(cx);
        let (stmt, row) = (pane.displayed(cx), pane.current_row(cx));
        let m = tab.read(cx).model().read(cx);
        let key = (m.run_id(), stmt, m.edits().inserted());
        if self.seen.replace(key).is_some_and(|seen| seen != key) {
            form.update(cx, |f, cx| f.finish_edit(false, window, cx));
        }
        if let (Some(stmt), Some(row)) = (stmt, row) {
            form.update(cx, |f, cx| f.show(stmt, row, window, cx));
        }
        cx.notify();
    }

    /// Whether Edit, Duplicate and Delete take clicks: they wait out a save, as the grid's
    /// menu does.
    pub fn actions_live(&self, cx: &App) -> bool {
        self.tab
            .as_ref()
            .is_some_and(|t| t.read(cx).model().read(cx).save_state() == SaveState::Idle)
    }

    /// Whether Back and Forward take clicks: the followed tab has a hop that way and holds no
    /// SQL the user typed, which a hop would replace.
    pub fn nav_live(&self, cx: &App) -> (bool, bool) {
        self.tab.as_ref().map_or((false, false), |t| {
            let t = t.read(cx);
            let walk = !t.edited(cx);
            (walk && t.nav().can_back(), walk && t.nav().can_forward())
        })
    }

    /// Whether any field or cell editor over the followed tab is open; history steps wait.
    fn editing(&self, cx: &App) -> bool {
        let form = self
            .form
            .as_ref()
            .is_some_and(|f| f.read(cx).editing().is_some());
        form || self.tab.as_ref().is_some_and(|t| {
            let pane = t.read(cx).results_pane().read(cx);
            pane.editing().is_some() || pane.record_form().read(cx).editing().is_some()
        })
    }

    /// Edit, Duplicate or Delete on the followed row.
    fn act(&mut self, action: Act, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(tab), Some(form)) = (self.tab.clone(), self.form.clone()) else {
            return;
        };
        let (model, pane) = (
            tab.read(cx).model().clone(),
            tab.read(cx).results_pane().clone(),
        );
        let Some(row) = pane.read(cx).current_row(cx) else {
            return;
        };
        let at = record_at(model.read(cx), 0, row);
        match action {
            Act::Edit => {
                let m = model.read(cx);
                let identity = m.browse().map(|b| b.target().identity().to_vec());
                let fields = record_fields(m, 0, row);
                let editable = fields.iter().filter(|f| f.editable);
                let outside = editable
                    .clone()
                    .find(|f| !identity.iter().flatten().any(|n| *n == f.name));
                if let Some(col) = outside.or(editable.clone().next()).map(|f| f.col) {
                    form.update(cx, |f, cx| f.begin_edit(col, window, cx));
                }
            }
            Act::Duplicate => {
                // Inserted rows lead the table, so the copy's index is its table row.
                if let Some(ix) = model.update(cx, |m, cx| m.duplicate_row(at, cx)) {
                    pane.update(cx, |p, cx| p.select_row(ix, window, cx));
                }
            }
            Act::Delete => {
                model.update(cx, |m, cx| m.delete_at(at, cx));
            }
        }
    }
}

/// An inspector button's action.
#[derive(Clone, Copy)]
enum Act {
    Edit,
    Duplicate,
    Delete,
}

// The header of the followed row: its position, and on a browse result its table and, for a
// loaded row, its identity from the loaded cells.
struct Header {
    title: String,
    table: Option<String>,
    identity: Option<String>,
}

fn header(tab: &QueryTab, cx: &App) -> Option<Header> {
    let pane = tab.results_pane().read(cx);
    let (stmt, row) = (pane.displayed(cx)?, pane.current_row(cx)?);
    let m = tab.model().read(cx);
    let at = record_at(m, stmt, row);
    let title = match at {
        GridRow::Loaded(i) => format!("Row {}", i + 1),
        GridRow::Inserted(j) => format!("New row {}", j + 1),
    };
    let browse = m.browse().filter(|_| stmt == 0 && m.browse_result());
    let table = browse.map(|b| {
        let rel = b.target().relation();
        format!("{}.{}", rel.schema(), rel.name())
    });
    let identity = match (browse, at, m.results().first()) {
        (Some(b), GridRow::Loaded(i), Some(result)) if b.target().has_identity() => {
            let store = &result.store;
            let key = b
                .target()
                .identity()
                .iter()
                .map(|name| {
                    let col = store.columns().iter().position(|c| c.name() == name)?;
                    Some((name.clone(), edit_value(store.cell(i, col)?)?))
                })
                .collect::<Option<Vec<_>>>();
            key.map(|k| key_text(&k))
        }
        _ => None,
    };
    Some(Header {
        title,
        table,
        identity,
    })
}

// One References row: its direction, its label "<far table> <key text>" split in two, and
// the Follow it emits, None for a NULL or unsupported key.
struct Ref {
    direction: FkDirection,
    table: String,
    key: String,
    follow: Option<InspectorEvent>,
}

fn refs(tab: &QueryTab, cx: &App) -> Vec<Ref> {
    let pane = tab.results_pane().read(cx);
    let (Some(0), Some(row)) = (pane.displayed(cx), pane.current_row(cx)) else {
        return Vec::new();
    };
    let m = tab.model().read(cx);
    let Some(home) = m
        .browse()
        .map(|b| b.target().relation().schema().to_owned())
    else {
        return Vec::new();
    };
    references(m, record_at(m, 0, row))
        .into_iter()
        .map(|link| {
            let rel = &link.relation;
            let table = if rel.schema() == home {
                rel.name().to_owned()
            } else {
                format!("{}.{}", rel.schema(), rel.name())
            };
            let follow = link.filter.map(|filter| InspectorEvent::Follow {
                connection: m.spec().connection.clone(),
                relation: link.relation.clone(),
                filter,
            });
            Ref {
                direction: link.direction,
                table,
                key: key_text(&link.key),
                follow,
            }
        })
        .collect()
}

impl Render for Inspector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head = self.tab.as_ref().and_then(|t| header(t.read(cx), cx));
        let links = self
            .tab
            .as_ref()
            .map(|t| refs(t.read(cx), cx))
            .unwrap_or_default();
        let dim = theme::dim(cx);
        let editable = head.is_some()
            && self
                .tab
                .as_ref()
                .is_some_and(|t| t.read(cx).model().read(cx).editable());
        let live = self.actions_live(cx);
        let theme = cx.theme();
        let (border, hover, fill) = (theme.border, theme.list_hover, theme.background);
        let (fg, muted, mono, mono_size) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.mono_font_family.clone(),
            theme.mono_font_size,
        );
        let close = Button::new("inspector-close")
            .ghost()
            .xsmall()
            .child(ui::icon(Icons::X, px(12.), muted))
            .tooltip("Close")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(InspectorEvent::Close)));
        let (back, forward) = self.nav_live(cx);
        // Each tooltip names the effective key, or only the verb when the action is unbound.
        let tip = |verb: &str, keys: Option<SharedString>| match keys {
            Some(k) => format!("{verb} ({})", plus_keys(&k)),
            None => verb.to_owned(),
        };
        let (back_tip, forward_tip) = (
            tip("Back", keys_for(&NavBack, cx)),
            tip("Forward", keys_for(&NavForward, cx)),
        );
        let nav_button = |id: &'static str, icon, tip: String, live: bool, back: bool| {
            Button::new(id)
                .ghost()
                .xsmall()
                .child(ui::icon(icon, px(12.), if live { fg } else { muted }))
                .tooltip(tip)
                .disabled(!live)
                .on_click(move |_, window, cx| match back {
                    true => window.dispatch_action(Box::new(NavBack), cx),
                    false => window.dispatch_action(Box::new(NavForward), cx),
                })
        };
        let nav = self.tab.is_some().then(|| {
            h_flex()
                .flex_none()
                .child(nav_button(
                    "inspector-back",
                    Icons::ChevronLeft,
                    back_tip,
                    back,
                    true,
                ))
                .child(nav_button(
                    "inspector-forward",
                    Icons::ChevronRight,
                    forward_tip,
                    forward,
                    false,
                ))
        });
        let top = h_flex().flex_none().h(px(36.)).pl_3().pr_2().gap_2();
        // The relation and identity are dim and mono like the grid's text: the identity
        // beside the title, the relation on the line under it.
        let mono_line = |id: &'static str, text: String| {
            div()
                .id(id)
                .aria_label(text.clone())
                .test_support()
                .min_w_0()
                .truncate()
                .font_family(mono.clone())
                .text_size(mono_size)
                .text_color(dim)
                .child(text)
        };
        let top = match &head {
            Some(h) => top
                .child(ui::icon(Icons::Table2, px(14.), muted))
                .child(
                    div()
                        .id("inspector-title")
                        .aria_label(h.title.clone())
                        .test_support()
                        .flex_none()
                        .font_weight(FontWeight::BOLD)
                        .text_sm()
                        .child(h.title.clone()),
                )
                .children(
                    h.identity
                        .clone()
                        .map(|i| mono_line("inspector-identity", i)),
                ),
            None => top.child(ui::section_label("Row inspector", cx)),
        }
        .child(div().flex_1())
        .children(nav)
        .child(close);
        let detail = head.as_ref().filter(|h| h.table.is_some()).map(|h| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .px_3()
                .pb_2()
                .children(h.table.clone().map(|t| mono_line("inspector-table", t)))
        });
        let empty = head.is_none().then(|| {
            div()
                .id("inspector-empty")
                .test_support()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("Select a row to inspect it")
        });
        // The form takes only the height its fields need, so References follows the last
        // field; a long form shrinks and scrolls.
        let form = head.as_ref().and(self.form.clone()).map(|f| {
            div()
                .flex()
                .flex_col()
                .flex_initial()
                .min_h_0()
                .border_t_1()
                .border_color(border)
                .child(f)
        });
        // Each link reads "<far table> <key>", the key dim and right-aligned; a NULL key's row
        // is dim throughout, has no chevron and takes no clicks.
        let references = (!links.is_empty()).then(|| {
            let rows = links.into_iter().enumerate().map(|(i, link)| {
                let (id, icon) = match link.direction {
                    FkDirection::Incoming => ("ref-in", Icons::Table2),
                    FkDirection::Outgoing | _ => ("ref-out", Icons::Link),
                };
                let live = link.follow.is_some();
                let row = h_flex()
                    .id((id, i))
                    .aria_label(format!("{} {}", link.table, link.key))
                    .test_support()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(fill)
                    .text_sm()
                    .child(ui::icon(icon, px(12.), muted))
                    .child(
                        div()
                            .flex_none()
                            .when(!live, |d| d.text_color(dim))
                            .child(link.table),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .text_color(dim)
                            .child(link.key),
                    )
                    .when(live, |r| {
                        r.child(div().id(("ref-chevron", i)).test_support().child(ui::icon(
                            Icons::ChevronRight,
                            px(12.),
                            muted,
                        )))
                    });
                match link.follow {
                    Some(event) => row
                        .cursor_pointer()
                        .hover(|s| s.bg(hover))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(event.clone()))),
                    None => row,
                }
            });
            let rows: Vec<_> = rows.collect();
            // Many keys scroll inside a capped share of the panel, so the form keeps its room.
            div()
                .id("inspector-references")
                .test_support()
                .flex()
                .flex_col()
                .flex_none()
                .max_h(relative(0.4))
                .py_2()
                .border_t_1()
                .border_color(border)
                .child(div().flex().px_3().pb_1().child(ui::tracked_label(
                    "inspector-references-heading",
                    "References",
                    cx,
                )))
                .child(
                    div()
                        .id("inspector-references-list")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .gap_1()
                        .children(rows),
                )
        });
        let button = |id: &'static str, icon, label: &'static str, act: Act| {
            Button::new(id)
                .outline()
                .small()
                .icon(icon)
                .label(label)
                .disabled(!live)
                .on_click(cx.listener(move |this, _, window, cx| this.act(act, window, cx)))
        };
        let buttons = editable.then(|| {
            h_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(border)
                .child(button("inspector-edit", Icons::Pencil, "Edit", Act::Edit))
                .child(button(
                    "inspector-duplicate",
                    Icons::CopyPlus,
                    "Duplicate",
                    Act::Duplicate,
                ))
                .child(div().flex_1())
                .child(button("inspector-delete", Icons::Trash, "Delete", Act::Delete).danger())
        });
        panel_box(cx)
            .id("inspector-panel")
            .test_support()
            .key_context("Inspector")
            .track_focus(&self.focus)
            // A press that no field inside took keeps the keys with the panel.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        this.focus.focus(window, cx);
                    }
                }),
            )
            // An open field's or cell's own Ctrl+Z undoes typing; a history step waits for it.
            .on_action(cx.listener(|this, _: &UndoEdit, _, cx| {
                if let Some(t) = this.tab.as_ref().filter(|_| !this.editing(cx)) {
                    t.read(cx).model().clone().update(cx, |m, cx| m.undo(cx));
                }
            }))
            // Ctrl+S saves as the results pane does, which stages this form's open field.
            .on_action(cx.listener(|this, _: &SaveEdits, window, cx| {
                if let Some(t) = this.tab.clone() {
                    let pane = t.read(cx).results_pane().clone();
                    pane.update(cx, |p, cx| p.save_edits(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &RedoEdit, _, cx| {
                if let Some(t) = this.tab.as_ref().filter(|_| !this.editing(cx)) {
                    t.read(cx).model().clone().update(cx, |m, cx| m.redo(cx));
                }
            }))
            .flex()
            .flex_col()
            .text_color(fg)
            .child(top)
            .children(detail)
            .children(empty)
            .children(form)
            .children(references)
            .child(div().flex_1())
            .children(buttons)
    }
}
