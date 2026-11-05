//! The record view: one row of a result as a Name / Value form, one line per column, whose
//! editable values open one at a time in a wrapping textarea.

use dbento_core::EditValue;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, RopeExt as _, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, TestSupportExt, Window, div, px,
};

use crate::keymap::{CancelRecordEdit, CommitRecordEdit};
use crate::record::{
    Marker, RecordField, Was, display_text, edit_text, edits_as_bytes, parse_edit, record_at,
    record_fields,
};
use crate::staged::{self, GridRow};
use crate::tab_model::{SaveState, TabModel};
use crate::{theme, ui};

/// Whether Enter types a newline in a field of `type_name` holding `text`: long-text types
/// (JSON, JSONB, XML, any case) and values that already span lines; elsewhere Enter stages.
pub fn enter_inserts_newline(type_name: &str, text: &str) -> bool {
    ["json", "jsonb", "xml"]
        .iter()
        .any(|t| type_name.eq_ignore_ascii_case(t))
        || text.contains('\n')
}

/// The key hint under an open field, for a field where Enter stages (false) or types a
/// newline (true).
pub fn edit_hint(multiline: bool) -> &'static str {
    #[cfg(target_os = "macos")]
    const MULTI: &str = "⌘+Enter save · Enter new line · Esc cancel";
    #[cfg(not(target_os = "macos"))]
    const MULTI: &str = "Ctrl+Enter save · Enter new line · Esc cancel";
    if multiline {
        MULTI
    } else {
        "Enter save · Shift+Enter new line · Esc cancel"
    }
}

/// The text size of a stacked field's name.
pub const STACKED_NAME_SIZE: gpui_kit::Pixels = px(13.);
/// The text size of a stacked field's type.
pub const STACKED_TYPE_SIZE: gpui_kit::Pixels = px(11.);

/// The border of a stacked field's value box: the theme's input border.
pub fn box_border(cx: &App) -> gpui_kit::Hsla {
    cx.theme().input
}

/// The open editor's padding at the kit's small input size, which a stacked value box shares
/// so the editor's border can cover the box's.
const EDIT_PX: gpui_kit::Pixels = px(8.);
const EDIT_PY: gpui_kit::Pixels = px(2.);

/// The leading key-marker slot and its gap to the name; stacked boxes inset by both.
const MARKER_SLOT: gpui_kit::Pixels = px(12.);
const MARKER_GAP: gpui_kit::Pixels = px(4.);

/// The form over one table row of one result of a tab.
pub struct RecordForm {
    model: Entity<TabModel>,
    stmt: usize,
    row: usize,
    focus: FocusHandle,
    editor: Entity<TextareaState>,
    /// The result column whose value the editor holds.
    editing: Option<usize>,
    /// Whether Enter types a newline in the open field rather than staging it.
    multiline: bool,
    /// Why the open edit could not stage.
    error: Option<String>,
    /// The first word of every element id: `record` or `inspector`.
    prefix: &'static str,
    /// Each field as a name line over a filled value box, for a narrow dock.
    stacked: bool,
    /// Whether a row with a stored document shows it whole instead of its fields.
    document_mode: bool,
    _observe: Subscription,
    _leave: Subscription,
    _typed: Subscription,
}

impl RecordForm {
    /// A form over `model`, showing table row 0 of result 0 until told otherwise.
    pub fn new(model: Entity<TabModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(model, "record", false, window, cx)
    }

    /// A form whose ids start `inspector-` instead of `record-`, each field stacked.
    pub fn inspector(model: Entity<TabModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(model, "inspector", true, window, cx)
    }

    fn build(
        model: Entity<TabModel>,
        prefix: &'static str,
        stacked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Staged edits and arriving rows change what the shown row reads.
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        // The keymap decides Enter: it stages unless the editor's context says `multiline`.
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .soft_wrap(true)
        });
        // A one-line value that gains a line break edits as multi-line from then on.
        let typed = cx.subscribe(&editor, |this: &mut Self, editor, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change)
                && this.editing.is_some()
                && !this.multiline
                && editor.read(cx).text().lines_len() > 1
            {
                this.multiline = true;
                cx.notify();
            }
        });
        let focus = cx.focus_handle();
        // Leaving the form stages its open field, so two forms over one tab never both hold
        // one; the window losing focus (switching apps) leaves the field open.
        let leave = cx.on_focus_out(&focus, window, |this, _, window, cx| {
            if window.is_window_active() {
                this.stage_open(window, cx);
            }
        });
        RecordForm {
            model,
            stmt: 0,
            row: 0,
            focus,
            editor,
            editing: None,
            multiline: false,
            error: None,
            prefix,
            stacked,
            document_mode: false,
            _observe: observe,
            _leave: leave,
            _typed: typed,
        }
    }

    /// Shows table row `row` of result `stmt`, staging an open edit first (a value that does
    /// not parse is dropped).
    pub fn show(&mut self, stmt: usize, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if (self.stmt, self.row) != (stmt, row) {
            self.end_edit(true, false, window, cx);
            self.stmt = stmt;
            self.row = row;
            cx.notify();
        }
    }

    /// Whether a row with a stored document shows it instead of its fields.
    pub fn document_mode(&self) -> bool {
        self.document_mode
    }

    /// Shows (true) or leaves (false) the document of each row that has one; rows without a
    /// document keep showing their fields.
    pub fn set_document_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.document_mode != on {
            self.document_mode = on;
            cx.notify();
        }
    }

    /// (stmt, row) shown.
    pub fn shown(&self) -> (usize, usize) {
        (self.stmt, self.row)
    }

    /// Opens the editor on result column `col` (staging an open one first); false when the
    /// field is not editable.
    pub fn begin_edit(&mut self, col: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.field_at(col, cx).filter(|f| f.editable) else {
            return false;
        };
        self.end_edit(true, false, window, cx);
        let text = edit_text(&field.value);
        self.multiline = enter_inserts_newline(&field.type_name, &text);
        self.editor.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.editing = Some(col);
        self.error = None;
        cx.notify();
        true
    }

    /// Stages (true) or drops (false) the open edit. A bad value keeps the editor open with
    /// its error when staging was asked by Ctrl+Enter; returns whether the editor closed.
    pub fn finish_edit(
        &mut self,
        stage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.end_edit(stage, true, window, cx)
    }

    /// Stages the open edit, dropping a value that does not parse; for leaving the form.
    pub fn stage_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edit(true, false, window, cx);
    }

    /// The result column being edited.
    pub fn editing(&self) -> Option<usize> {
        self.editing
    }

    /// Why the open edit could not stage.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether a field's Revert takes clicks: no save is in flight.
    pub fn revert_live(&self, cx: &App) -> bool {
        self.model.read(cx).save_state() == SaveState::Idle
    }

    /// The field editor's textarea.
    pub fn editor(&self) -> &Entity<TextareaState> {
        &self.editor
    }

    /// The element id `<prefix>-<name>`.
    fn id(&self, name: &str) -> SharedString {
        format!("{}-{name}", self.prefix).into()
    }

    /// The shown row's field for result column `col`.
    fn field_at(&self, col: usize, cx: &App) -> Option<RecordField> {
        record_fields(self.model.read(cx), self.stmt, self.row)
            .into_iter()
            .find(|f| f.col == col)
    }

    /// Closes the editor, staging its text through `set_value` (one undo step) when `stage`.
    /// With `keep_bad`, text that does not parse keeps it open with the error instead.
    fn end_edit(
        &mut self,
        stage: bool,
        keep_bad: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(col) = self.editing else {
            return true;
        };
        // A field gone with its row has nothing to stage into.
        if stage && let Some(field) = self.field_at(col, cx) {
            let text = self.editor.read(cx).value().to_string();
            match parse_edit(&text, &field.value, edits_as_bytes(&field)) {
                Ok(Some(value)) => {
                    let (stmt, row) = (self.stmt, self.row);
                    self.model.update(cx, |m, cx| {
                        let at = record_at(m, stmt, row);
                        m.set_value(at, col, value, cx)
                    });
                }
                Ok(None) => {}
                Err(e) if keep_bad => {
                    self.error = Some(e);
                    cx.notify();
                    return false;
                }
                Err(_) => {}
            }
        }
        // The form takes focus back only from its own input, never from the SQL editor.
        if self.editor.read(cx).focus_handle(cx).is_focused(window) {
            self.focus.focus(window, cx);
        }
        self.editing = None;
        self.error = None;
        cx.notify();
        true
    }

    /// The dim "was" line under a staged field, with Revert and, for a capped original,
    /// Copy original.
    fn was_line(&self, col: usize, was: Was, cx: &Context<Self>) -> gpui_kit::Div {
        let (stmt, row) = (self.stmt, self.row);
        let revert = Button::new((self.id("revert"), col))
            .ghost()
            .xsmall()
            .label("Revert")
            .disabled(!self.revert_live(cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                // An open draft of this field would restage the value once committed.
                if this.editing == Some(col) {
                    this.end_edit(false, false, window, cx);
                }
                this.model.update(cx, |m, cx| {
                    let at = record_at(m, stmt, row);
                    m.revert_cell(at, col, cx)
                });
            }));
        let copy = was.copy.map(|text| {
            Button::new((self.id("was-copy"), col))
                .ghost()
                .xsmall()
                .label("Copy original")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                })
        });
        div()
            .flex()
            .items_center()
            .gap_1()
            .pt_1()
            .text_xs()
            .child(
                div()
                    .id((self.id("was"), col))
                    .aria_label(was.text.clone())
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::dim(cx))
                    .child(was.text),
            )
            .child(revert)
            .children(copy)
    }

    /// The dim line under the was line naming what a reload found in the database now.
    fn fresh_line(&self, col: usize, text: String, cx: &Context<Self>) -> gpui_kit::Div {
        div().pt_1().text_xs().child(
            div()
                .id((self.id("fresh"), col))
                .aria_label(text.clone())
                .test_support()
                .truncate()
                .text_color(theme::dim(cx))
                .child(text),
        )
    }

    /// One Name / Value line: the 220 px name cell with its key marker and type, then the
    /// wrapping value.
    fn field(&self, f: RecordField, cx: &Context<Self>) -> AnyElement {
        let (dim, modified_fill) = (theme::dim(cx), theme::edit_pending(cx).fill);
        let theme = cx.theme();
        let (border, fg, danger) = (theme.border, theme.foreground, theme.danger);
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let col = f.col;
        let was = f.was.map(|was| self.was_line(col, was, cx));
        let fresh = f.fresh.map(|text| self.fresh_line(col, text, cx));
        let marker = f.marker.map(|m| {
            let (id, icon) = match m {
                Marker::PrimaryKey => ("pk", ui::Icons::KeyRound),
                Marker::ForeignKey => ("fk", ui::Icons::Link),
                Marker::Locator => ("lock", ui::Icons::Lock),
            };
            div()
                .id((self.id(id), col))
                .test_support()
                .flex()
                .flex_none()
                .items_center()
                .h(LINE)
                .child(ui::icon(icon, px(12.), dim))
        });
        let type_text = match &f.references {
            Some(r) => format!("{} → {r}", f.type_name),
            None => f.type_name.clone(),
        };
        let value = display_text(&f.value, &f.type_name);
        let placeholder = matches!(f.value, EditValue::Null | EditValue::Default);
        let name = div()
            .flex()
            .items_start()
            .gap(MARKER_GAP)
            // A fixed slot, empty without a marker, keeps every name on one left edge.
            .child(div().flex_none().w(MARKER_SLOT).children(marker))
            .child(
                div()
                    .id((self.id("name"), col))
                    .aria_label(f.name.clone())
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .when(self.stacked, |d| d.text_size(STACKED_NAME_SIZE))
                    .text_color(fg)
                    .child(f.name),
            )
            .child(
                div()
                    .id((self.id("type"), col))
                    .aria_label(type_text.clone())
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    // Stacked types are small sans labels; the wide form keeps them mono.
                    .when(self.stacked, |d| d.text_size(STACKED_TYPE_SIZE))
                    .when(!self.stacked, |d| {
                        d.font_family(mono.clone()).text_size(mono_size)
                    })
                    .text_color(dim)
                    .child(type_text),
            );
        let value = if self.editing == Some(col) {
            // Stopping the click keeps a click inside the editor from re-opening it.
            let error = self.error.clone().map(|e| {
                div()
                    .id(self.id("error"))
                    .aria_label(e.clone())
                    .test_support()
                    .pt_1()
                    .text_xs()
                    .text_color(danger)
                    .child(e)
            });
            let hint = edit_hint(self.multiline);
            let hint = div()
                .id(self.id("hint"))
                .aria_label(hint)
                .test_support()
                .pt_1()
                .text_xs()
                .text_color(dim)
                .child(hint);
            div()
                .id(self.id("editor"))
                // `multiline` turns off the keymap's Enter-stages binding for this field.
                .key_context(if self.multiline {
                    "RecordEditor multiline"
                } else {
                    "RecordEditor"
                })
                .test_support()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    // The box grows outward by its padding and border, so the text stays
                    // where the read-only value drew it; in a stacked box the border lies on
                    // the box's own. A wrapper takes the margins so the width stretches too.
                    div().mx(-(EDIT_PX + px(1.))).my(-(EDIT_PY + px(1.))).child(
                        Textarea::new(&self.editor)
                            .font_family(mono)
                            .text_size(mono_size)
                            .small(),
                    ),
                )
                .child(hint)
                .children(error)
                .into_any_element()
        } else {
            div()
                .id((self.id("value"), col))
                .aria_label(value.clone())
                .test_support()
                .font_family(mono)
                .text_size(mono_size)
                .text_color(if placeholder { dim } else { fg })
                .when(f.editable, |d| {
                    d.cursor_text()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_edit(col, window, cx);
                        }))
                })
                .child(value)
                .into_any_element()
        };
        let row = if self.stacked {
            // The app canvas recesses the box from the panel in both themes; the editor
            // fill is the panel's own white in the light theme.
            let boxed = (self.id("box"), col).into();
            stacked_line(name, value, boxed, border, theme.background, box_border(cx))
                .children(was.map(|w| w.ml(MARKER_SLOT + MARKER_GAP)))
                .children(fresh.map(|f| f.ml(MARKER_SLOT + MARKER_GAP)))
        } else {
            line(
                name,
                div().child(value).children(was).children(fresh),
                border,
            )
        }
        .when(f.modified, |d| d.bg(modified_fill));
        if f.modified {
            div()
                .id((self.id("modified"), col))
                .test_support()
                .child(row)
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

/// The shared line height, so the sans name and the mono type and value sit on one baseline.
const LINE: gpui_kit::Pixels = px(18.);

/// A form line: the 220 px name cell, a hairline divider, then the value cell; at least 30 px
/// tall with a hairline under it.
fn line(name: impl IntoElement, value: impl IntoElement, border: gpui_kit::Hsla) -> gpui_kit::Div {
    let cell = || div().py(px(6.)).px_3().min_w_0();
    div()
        .flex()
        .min_h(px(30.))
        .border_b_1()
        .border_color(border)
        .text_sm()
        .line_height(LINE)
        .child(
            cell()
                .flex_none()
                .w(px(220.))
                .border_r_1()
                .border_color(border)
                .child(name),
        )
        .child(cell().flex_1().child(value))
}

/// A stacked form line for a narrow dock: the name line, then the value in a filled, bordered
/// mono box `box_id` at least 22 px tall, with a hairline under both. The box is inset past
/// the marker slot so it starts where the name does.
fn stacked_line(
    name: impl IntoElement,
    value: impl IntoElement,
    box_id: gpui_kit::ElementId,
    border: gpui_kit::Hsla,
    fill: gpui_kit::Hsla,
    box_border: gpui_kit::Hsla,
) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py(px(6.))
        .border_b_1()
        .border_color(border)
        .text_sm()
        .line_height(LINE)
        .child(name)
        .child(
            div()
                .id(box_id)
                .test_support()
                .ml(MARKER_SLOT + MARKER_GAP)
                .min_h(px(22.))
                .px(EDIT_PX)
                .py(EDIT_PY)
                .rounded_sm()
                .border_1()
                .border_color(box_border)
                .bg(fill)
                .child(value),
        )
}

impl Focusable for RecordForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RecordForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let fields = record_fields(m, self.stmt, self.row);
        let at = record_at(m, self.stmt, self.row);
        // Only a loaded row of an engine with documents offers the mode; the shell text is
        // built only while it is shown.
        let document = match at {
            GridRow::Loaded(r) => m
                .results()
                .get(self.stmt)
                .and_then(|res| res.store.document(r)),
            GridRow::Inserted(_) => None,
        };
        let has_document = document.is_some();
        let document_text = document
            .filter(|_| self.document_mode)
            .map(|d| d.to_shell(true));
        let deleted = self.stmt == 0
            && m.browse_result()
            && matches!(at, GridRow::Loaded(r) if m.edits().is_deleted(r));
        // A conflicting delete has no staged cell for a fresh line, so the mark carries it.
        let gone = (deleted && m.row_gone(at)).then(|| staged::fresh_text(staged::Fresh::Gone));
        let dim = theme::dim(cx);
        let (border, danger) = (cx.theme().border, cx.theme().danger);
        let root = div()
            .id(self.id("view"))
            .test_support()
            .key_context("RecordView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CommitRecordEdit, window, cx| {
                this.finish_edit(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CancelRecordEdit, window, cx| {
                this.finish_edit(false, window, cx);
            }))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0();
        if fields.is_empty() {
            return root.child(
                div()
                    // The inspector's panel owns `inspector-empty`.
                    .id(self.id(if self.stacked { "form-empty" } else { "empty" }))
                    .aria_label("No rows")
                    .test_support()
                    .p_3()
                    .text_sm()
                    .text_color(dim)
                    .child("No rows"),
            );
        }
        let mode = has_document.then(|| {
            div().flex_none().px_3().py_1().child(ui::segmented(
                self.id("mode"),
                &["Fields", "Document"],
                usize::from(self.document_mode),
                cx.listener(|this, ix: &usize, _, cx| this.set_document_mode(*ix == 1, cx)),
                window,
                cx,
            ))
        });
        if let Some(text) = document_text {
            let mono = cx.theme().mono_font_family.clone();
            let mono_size = cx.theme().mono_font_size;
            return root.children(mode).child(
                div()
                    .id(self.id("document"))
                    .aria_label(text.clone())
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .py(px(6.))
                    .font_family(mono)
                    .text_size(mono_size)
                    .child(text),
            );
        }
        let lines: Vec<_> = fields.into_iter().map(|f| self.field(f, cx)).collect();
        let heading = |text: &'static str| div().text_xs().text_color(dim).child(text);
        let deleted_mark = deleted.then(|| {
            let fresh = gone.map(|text| {
                div()
                    .id(self.id("fresh-row"))
                    .aria_label(text.clone())
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .text_color(dim)
                    .child(text)
            });
            div()
                .flex()
                .gap_2()
                .min_w_0()
                .text_xs()
                .child(
                    div()
                        .id(self.id("deleted"))
                        .aria_label("Deleted")
                        .test_support()
                        .flex_none()
                        .text_color(danger)
                        .child("Deleted"),
                )
                .children(fresh)
        });
        // Stacked fields name themselves, so the narrow form heads only a deleted row.
        let header = if self.stacked {
            div().children(deleted_mark.map(|d| d.px_3().py_1()))
        } else {
            // The Name heading sits past the same empty marker slot as the names below it.
            let name_heading = div()
                .flex()
                .gap(MARKER_GAP)
                .child(div().flex_none().w(MARKER_SLOT))
                .child(heading("Name").id(self.id("heading-name")).test_support());
            let value_heading = div()
                .flex()
                .justify_between()
                .child(heading("Value"))
                .children(deleted_mark);
            line(name_heading, value_heading, border)
        };
        root.children(mode)
            .child(
                div()
                    .id(self.id("header"))
                    .test_support()
                    .flex_none()
                    .child(header),
            )
            .child(
                div()
                    .id(self.id("fields"))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(deleted, |d| d.bg(theme::edit_deleted(cx)).line_through())
                    .children(lines),
            )
    }
}
