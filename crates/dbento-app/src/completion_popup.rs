//! The editor's completion popup, drawn by the app: the kit's own menu latches its width at
//! first open, selects in the global accent and draws no kind glyphs. The kit still inserts
//! the accepted item; the provider hands the kit no items, so its menu never opens.

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::{
    AnyElement, App, Context, Entity, Focusable as _, HighlightStyle, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, TestSupportExt as _,
    Window, anchored, deferred, div, point, prelude::FluentBuilder as _, px,
};
use lsp_types::{CompletionItem, CompletionTextEdit, TextEdit};

use crate::completion::{Candidate, CandidateKind, Completion};
use crate::theme;
use crate::ui::{self, Icons};

/// The widest the popup grows; a longer row truncates its label.
pub const MAX_WIDTH: Pixels = px(480.);
const MIN_WIDTH: Pixels = px(160.);
const MAX_HEIGHT: Pixels = px(240.);
const ROW_HEIGHT: Pixels = px(22.);

/// The answer the editor shows, tied to the buffer state it was computed for.
pub struct CompletionPopup {
    editor: Entity<EditorState>,
    items: Vec<Candidate>,
    // Each label's lit prefix, in bytes of the label.
    lit: Vec<usize>,
    replace: Range<usize>,
    // The caret and buffer length the answer belongs to; any other state closes it.
    caret: usize,
    len: usize,
    selected: usize,
    scroll: ScrollHandle,
    _editor: Subscription,
}

impl CompletionPopup {
    /// A closed popup over `editor`, redrawn whenever the editor changes.
    pub fn new(editor: &Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        CompletionPopup {
            editor: editor.clone(),
            items: Vec::new(),
            lit: Vec::new(),
            replace: 0..0,
            caret: 0,
            len: 0,
            selected: 0,
            scroll: ScrollHandle::new(),
            _editor: cx.observe(editor, |_, _, cx| cx.notify()),
        }
    }

    /// Shows `answer` for a caret at `caret` of a `len`-byte buffer whose replaced prefix
    /// reads `typed`; an answer without items closes the popup.
    pub fn show(
        &mut self,
        answer: Completion,
        typed: &str,
        caret: usize,
        len: usize,
        cx: &mut Context<Self>,
    ) {
        self.lit = (answer.items.iter())
            .map(|c| label_prefix(&c.label, typed).len())
            .collect();
        self.items = answer.items;
        self.replace = answer.replace;
        (self.caret, self.len, self.selected) = (caret, len, 0);
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Closes the popup.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if !self.items.is_empty() {
            self.items.clear();
            cx.notify();
        }
    }

    /// The candidates shown, empty when closed.
    pub fn items(&self) -> &[Candidate] {
        &self.items
    }

    /// The highlighted row.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The byte length of item `ix`'s label lit as typed.
    pub fn highlight(&self, ix: usize) -> usize {
        self.lit.get(ix).copied().unwrap_or(0)
    }

    /// Whether the popup is open for the editor as it stands: focused, with the caret and
    /// text the answer was computed for.
    pub fn is_live(&self, window: &Window, cx: &App) -> bool {
        let e = self.editor.read(cx);
        !self.items.is_empty()
            && e.focus_handle(cx).is_focused(window)
            && e.cursor() == self.caret
            && e.text().len() == self.len
    }

    /// Moves the highlight `step` rows, wrapping, and scrolls it into view.
    pub fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let n = self.items.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + step).rem_euclid(n) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    /// Replaces the typed prefix with row `ix`'s insert text, or the highlighted row's.
    pub fn accept(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.items.get(ix.unwrap_or(self.selected)).cloned() else {
            return;
        };
        let range = self.replace.clone();
        self.close(cx);
        self.editor.update(cx, |e, cx| {
            // LSP positions count characters, as the editor's own `position_to_offset` reads them.
            let lsp = lsp_types::Range::new(
                e.text().offset_to_position(range.start),
                e.text().offset_to_position(range.end),
            );
            let item = CompletionItem {
                label: c.label,
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(lsp, c.insert))),
                ..Default::default()
            };
            e.insert_completion(&item, range, window, cx);
        });
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (selected, hover, accent) = (theme.list_active, theme.list_hover, theme.blue);
        let (muted, dim) = (theme.muted_foreground, theme::dim(cx));
        let c = &self.items[ix];
        let lit = HighlightStyle {
            color: Some(accent),
            ..Default::default()
        };
        let label = StyledText::new(SharedString::from(c.label.clone()))
            .with_highlights([(0..self.highlight(ix), lit)]);
        div()
            .id(("completion-row", ix))
            .aria_label(SharedString::from(c.label.clone()))
            .flex()
            .items_center()
            .gap_2()
            .h(ROW_HEIGHT)
            .px_2()
            .rounded(cx.theme().radius)
            .whitespace_nowrap()
            .when(ix == self.selected, |r| r.bg(selected))
            .when(ix != self.selected, |r| r.hover(|r| r.bg(hover)))
            .child(ui::icon(kind_icon(c.kind), px(14.), muted).flex_none())
            .child(div().min_w_0().flex_1().truncate().child(label))
            .child(
                div()
                    .flex_none()
                    .pl_2()
                    .text_color(dim)
                    .child(c.detail.clone()),
            )
            .on_click(cx.listener(move |p, _, window, cx| p.accept(Some(ix), window, cx)))
            .test_support()
            .into_any_element()
    }
}

/// The explorer glyph for a candidate of `kind`.
pub fn kind_icon(kind: CandidateKind) -> IconName {
    match kind {
        CandidateKind::Column => Icons::Columns3,
        CandidateKind::Relation => Icons::Table2,
        CandidateKind::Schema => Icons::Layers,
        CandidateKind::Keyword => Icons::FileCode,
    }
}

/// The prefix of `label` that matches `typed` ignoring case, or "" when none does. Case
/// folding can change a character's length, so the lit part is measured on the label itself.
fn label_prefix<'l>(label: &'l str, typed: &str) -> &'l str {
    let want = typed.to_lowercase();
    let ends = label.char_indices().map(|(i, _)| i).chain([label.len()]);
    ends.map(|end| &label[..end])
        .find(|p| p.to_lowercase() == want)
        .unwrap_or("")
}

impl Render for CompletionPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // An answer the editor has moved past (a caret move, a deletion, a blur) closes for good.
        if !self.is_live(window, cx) {
            self.items.clear();
            return div().into_any_element();
        }
        let e = self.editor.read(cx);
        let Some((caret, line_height)) = e.cursor_layout() else {
            return div().into_any_element();
        };
        // Just under the caret, as the kit placed its menu; the scroll offset maps the
        // caret's text position to the window.
        let at = e.scroll_offset() + caret.origin + point(px(-4.), line_height + px(4.));
        let rows: Vec<AnyElement> = (0..self.items.len()).map(|ix| self.row(ix, cx)).collect();
        let theme = cx.theme();
        deferred(
            anchored()
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("completion-popup")
                        .occlude()
                        .min_w(MIN_WIDTH)
                        .max_w(MAX_WIDTH)
                        .max_h(MAX_HEIGHT)
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .p_1()
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .shadow_md()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(theme.mono_font_size)
                        .children(rows)
                        .on_mouse_down_out(cx.listener(|p, _, _, cx| p.close(cx)))
                        .test_support(),
                ),
        )
        .with_priority(1)
        .into_any_element()
    }
}
