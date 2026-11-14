//! A themed in-app confirmation dialog that replaces the platform prompt: Enter answers the
//! focused button where the default holds the first focus, else the default; Escape the cancel
//! choice. Each button carries an id the pilot can see.
// Copied from Apinata (crates/apinata-app/src/confirm.rs), MIT OR Apache-2.0, without its
// dialog placement helper.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Size, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    App, AppContext as _, Div, Entity, FocusHandle, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt, Window, div, px,
};

use crate::staged::PREVIEW_LINES;
use crate::{theme, ui};

/// The kit editor's row height as a multiple of its font size (gpui-component `editor.rs`).
const EDITOR_LINE_HEIGHT: f32 = 1.5;

/// The highest a dialog sits: under the 40 px title bar, the 6 px gutter, the tab strip and
/// the 40 px toolbar holding the production chip, with room to spare.
pub const DIALOG_TOP: Pixels = px(120.);
/// The width of the confirm, name and approvals dialogs.
pub const DIALOG_WIDTH: Pixels = px(480.);
/// The text width inside a dialog's SQL box: the dialog less its 16 px sides, the box's border
/// and right padding, and the editor's own 12 px inset, plus the 4 px the box pulls it left.
const PREVIEW_TEXT_WIDTH: Pixels = px(480. - 32. - 2. - 8. - 12. - 12. + 4.);

/// One answer button; `danger` paints it destructive.
#[derive(Clone)]
pub struct Choice {
    /// Element id of the button.
    pub id: &'static str,
    /// Text shown on the button.
    pub label: SharedString,
    /// Paints the button destructive.
    pub danger: bool,
}

/// `default` is the index Enter answers; when it is the first choice, which starts focused,
/// Enter follows the focus instead. `cancel` is the index Escape answers. Buttons are painted
/// by [`choice_style`], so beside a `danger` choice the destructive one is the only red button.
#[derive(Clone)]
pub struct Ask {
    /// The question, shown as the dialog title.
    pub title: SharedString,
    /// Optional explanation shown under the title.
    pub detail: Option<SharedString>,
    /// SQL shown boxed in mono under the explanation.
    pub sql: Option<SharedString>,
    /// The grammar id the boxed text is highlighted with ("sql", "javascript").
    pub language: &'static str,
    /// The buttons, left to right.
    pub choices: Vec<Choice>,
    /// Index into `choices` that Enter answers; at 0, Enter follows the focus instead.
    pub default: usize,
    /// Index into `choices` that Escape answers.
    pub cancel: usize,
}

/// How a choice's button is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceStyle {
    /// The raised fill with the text colour.
    Neutral,
    /// The accent fill.
    Primary,
    /// The destructive fill.
    Danger,
}

/// A danger choice is Danger; beside one, every other choice is Neutral; without one, the
/// default is Primary and the rest Neutral.
pub fn choice_style(spec: &Ask, ix: usize) -> ChoiceStyle {
    let has_danger = spec.choices.iter().any(|c| c.danger);
    match spec.choices.get(ix) {
        Some(c) if c.danger => ChoiceStyle::Danger,
        _ if !has_danger && ix == spec.default => ChoiceStyle::Primary,
        _ => ChoiceStyle::Neutral,
    }
}

/// The key that answers `spec`'s choice `ix`, for its chip: `Esc` on the cancel choice, and
/// Enter on the default when that differs from cancel. Enter answering cancel shows no chip.
pub fn choice_keys(spec: &Ask, ix: usize) -> Option<&'static str> {
    if ix == spec.cancel {
        Some("Esc")
    } else if ix == spec.default {
        Some("\u{23CE}")
    } else {
        None
    }
}

/// Dresses `dialog` as a dbento surface: `width` wide, raised fill, large radius, no kit
/// padding and no close button, so callers lay out their own [`title_row`] and [`footer`].
/// With `edge`, a 2 px top border in that colour replaces the kit's hairline. Callers set
/// `margin_top`.
pub fn frame(dialog: Dialog, width: Pixels, edge: Option<Hsla>, cx: &App) -> Dialog {
    let theme = cx.theme();
    let dialog = dialog
        .w(width)
        .p_0()
        .close_button(false)
        .bg(theme.popover)
        .rounded(theme.radius_lg);
    match edge {
        Some(color) => dialog.border_0().border_t_2().border_color(color),
        None => dialog,
    }
}

/// A dialog's title row: an optional 16 px glyph, then the title in SemiBold text.
pub fn title_row(icon: Option<(IconName, Hsla)>, title: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .gap_2()
        .px_4()
        .pt_4()
        .when_some(icon, |el, (name, color)| {
            el.child(ui::icon(name, px(16.), color))
        })
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(title.into()),
        )
}

/// A dialog's 48 px footer bar under a hairline; callers add a spacer and 32 px buttons.
pub fn footer(cx: &App) -> Div {
    h_flex()
        .flex_none()
        .h(px(48.))
        .px_4()
        .gap_2()
        .items_center()
        .border_t_1()
        .border_color(cx.theme().border)
}

/// The key text and box edge colours for a chip on a button of `style`: a filled button's
/// text with its boxes at 40 %, like Run's chip, or the plain dim chip on a neutral one.
pub fn chip_colours(style: ChoiceStyle, cx: &App) -> (Hsla, Hsla) {
    let fg = match style {
        ChoiceStyle::Neutral => return (crate::theme::dim(cx), cx.theme().border),
        ChoiceStyle::Primary => cx.theme().primary_foreground,
        ChoiceStyle::Danger => cx.theme().danger_foreground,
    };
    (fg, fg.opacity(0.4))
}

/// Gives a dialog button its key chip, each key boxed in [`chip_colours`] for `style`.
pub fn with_keys(button: Button, keys: Option<&str>, style: ChoiceStyle, cx: &App) -> Button {
    let Some(keys) = keys else {
        return button;
    };
    let (text, edge) = chip_colours(style, cx);
    button.child(ui::key_chip_colored(keys.to_owned(), text, edge, cx))
}

/// Where a dialog's surface starts: the kit's tenth of the height, never above DIALOG_TOP.
pub fn dialog_top(window: &Window) -> Pixels {
    (window.viewport_size().height / 10.).max(DIALOG_TOP)
}

/// A read-only SQL editor holding `sql`: highlighted, no line numbers, folding or soft wrap.
pub fn sql_preview(sql: &str, window: &mut Window, cx: &mut App) -> Entity<EditorState> {
    preview_editor(sql, "sql", false, window, cx)
}

/// A read-only editor holding `text` highlighted as `language`, with no line numbers or
/// folding, wrapping long lines at the confirm dialog's width.
pub fn code_preview(
    text: &str,
    language: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    preview_editor(text, language, true, window, cx)
}

fn preview_editor(
    text: &str,
    language: &'static str,
    wrap: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    let text = text.to_owned();
    let (font, size) = (
        cx.theme().mono_font_family.clone(),
        cx.theme().mono_font_size,
    );
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx)
            .language(language)
            .line_number(false)
            .folding(false)
            .soft_wrap(wrap)
            .default_value(text);
        state.set_readonly(true, cx);
        // Wrapped before the first layout, so the box is as tall as its rows from the start.
        if wrap {
            state.prewrap(gpui_kit::font(font), size, PREVIEW_TEXT_WIDTH, cx);
        }
        state
    })
}

/// The prompt's boxed SQL, `confirm-sql` labelled by `sql`, holding the read-only `state` in
/// `confirm-sql-editor`: as tall as its statements (one per line), capped at the preview's
/// lines plus the "and N more" line.
pub fn sql_box(
    sql: SharedString,
    state: &Entity<EditorState>,
    cx: &App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    // A wrapping editor counts its wrapped rows; before its first wrap it has none.
    let rows = state.read(cx).display_map().wrap_row_count();
    let lines = rows.max(sql.lines().count()).clamp(1, PREVIEW_LINES + 1);
    // The kit's multi-line editor pads its text by the Medium input padding above and below.
    let height =
        theme.mono_font_size * EDITOR_LINE_HEIGHT * lines as f32 + Size::Medium.input_py() * 2.;
    div()
        .id("confirm-sql")
        .aria_label(sql)
        .test_support()
        .py_2()
        .pr_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme::code_edge(cx))
        .bg(theme::editor_fill(cx))
        .child(
            // The kit Editor ignores its tab index, so a tab group at index 1 puts it after
            // the dialog's buttons and Cancel keeps the first focus. The kit sets code 12 px
            // into the editor (padding plus an empty gutter), so the editor starts 4 px left
            // inside this clip to set it 9 px in, like the approval dialog's plain box.
            div()
                .id("confirm-sql-editor")
                .test_support()
                .tab_group()
                .tab_index(1)
                .h(height)
                .overflow_hidden()
                .child(
                    div()
                        .ml(px(-4.))
                        .h_full()
                        .child(Editor::new(state).readonly(true).appearance(false).h_full()),
                ),
        )
}

/// An open [`ask_closable`] question: its answer, and a way to take it off screen unanswered.
pub struct Asked {
    /// Yields the chosen index once; errs when the dialog goes away unanswered.
    pub answer: async_channel::Receiver<usize>,
    buttons: Rc<Vec<FocusHandle>>,
}

impl Asked {
    /// Closes the dialog while one of its buttons holds focus. Focus elsewhere means another
    /// dialog sits on top, and closing then would take the wrong one.
    pub fn close(&self, window: &mut Window, cx: &mut App) {
        if self.buttons.iter().any(|h| h.is_focused(window)) {
            window.close_dialog(cx);
        }
    }
}

/// Opens the modal over `window` on the next effect cycle, so a popup menu that restores focus
/// after its click handler cannot pull focus out of it. The receiver yields the chosen index
/// once; it errs when the dialog goes away unanswered (window closed).
pub fn ask(spec: Ask, window: &mut Window, cx: &mut App) -> async_channel::Receiver<usize> {
    ask_closable(spec, window, cx).answer
}

/// [`ask`], also returning a way to close the dialog unanswered.
pub fn ask_closable(spec: Ask, window: &mut Window, cx: &mut App) -> Asked {
    // A one-slot channel and try_send: whichever answer lands first wins, later ones drop.
    let (tx, rx) = async_channel::bounded(1);
    let focused = Rc::new(Cell::new(false));
    // One focus handle per button for the dialog's life, so Enter can tell which one holds focus.
    let handles: Rc<Vec<FocusHandle>> =
        Rc::new(spec.choices.iter().map(|_| cx.focus_handle()).collect());
    let asked = Asked {
        answer: rx,
        buttons: Rc::clone(&handles),
    };
    window.defer(cx, move |window, cx| {
        // The builder runs every frame, so the preview's editor is made once, here, and each
        // handler clones what it captures.
        let language = spec.language;
        let preview = spec
            .sql
            .as_ref()
            .map(|sql| code_preview(sql, language, window, cx));
        window.open_dialog(cx, move |dialog, window, cx| {
            // Once the first frame holding the buttons is drawn, focus the first button (Cancel
            // beside a danger choice), so Space never answers Run.
            if !focused.replace(true) {
                let first = handles.first().cloned();
                window.defer(cx, move |window, cx| {
                    if let Some(handle) = first {
                        handle.focus(window, cx);
                    }
                });
            }
            let key = |ix: usize| {
                let tx = tx.clone();
                move |_: &_, _: &mut Window, _: &mut App| {
                    let _ = tx.try_send(ix);
                    true
                }
            };
            // Where the default is the first focus, Enter answers whichever button holds focus,
            // so the ring is always on what Enter presses. A later default (Commit beside a
            // focused Roll back) keeps Enter on itself.
            let ok = {
                let (tx, handles, default) = (tx.clone(), Rc::clone(&handles), spec.default);
                move |_: &_, window: &mut Window, _: &mut App| {
                    let focused = handles.iter().position(|h| h.is_focused(window));
                    let ix = match focused {
                        Some(ix) if default == 0 => ix,
                        _ => default,
                    };
                    let _ = tx.try_send(ix);
                    true
                }
            };
            let buttons: Vec<_> = spec
                .choices
                .iter()
                .enumerate()
                .map(|(ix, c)| {
                    let tx = tx.clone();
                    let style = choice_style(&spec, ix);
                    let button = Button::new(c.id)
                        .label(c.label.clone())
                        .h(px(32.))
                        .track_focus(&handles[ix]);
                    let button = match style {
                        ChoiceStyle::Neutral => ui::secondary(button),
                        ChoiceStyle::Primary => button.primary(),
                        ChoiceStyle::Danger => button.danger(),
                    };
                    with_keys(button, choice_keys(&spec, ix), style, cx).on_click(
                        move |_, window, cx| {
                            let _ = tx.try_send(ix);
                            window.close_dialog(cx);
                        },
                    )
                })
                .collect();
            let theme = cx.theme();
            let danger = theme.danger;
            // The top never rises above DIALOG_TOP, so the danger edge clears the title bar,
            // tab strip and the toolbar's production chip instead of covering them.
            frame(dialog, DIALOG_WIDTH, Some(danger), cx)
                .margin_top(dialog_top(window))
                .on_ok(ok)
                .on_cancel(key(spec.cancel))
                .child(
                    v_flex()
                        .id("confirm-dialog")
                        .test_support()
                        .child(title_row(
                            Some((ui::Icons::TriangleAlert, danger)),
                            spec.title.clone(),
                            cx,
                        ))
                        .child(
                            v_flex()
                                .gap_3()
                                .px_4()
                                .pt_2()
                                .pb_4()
                                .when_some(spec.detail.clone(), |el, detail| {
                                    el.child(
                                        div()
                                            .id("confirm-detail")
                                            .aria_label(detail.clone())
                                            .test_support()
                                            .text_sm()
                                            .text_color(theme::dim(cx))
                                            .child(detail),
                                    )
                                })
                                .when_some(
                                    spec.sql.clone().zip(preview.clone()),
                                    |el, (sql, state)| el.child(sql_box(sql, &state, cx)),
                                ),
                        )
                        // Cancel stays the first focusable element, after the spacer.
                        .child(footer(cx).child(div().flex_1()).children(buttons)),
                )
        });
    });
    asked
}

/// What the connection picker shows: a title, a dim detail, the SQL boxed, and one row per
/// connection as `(name, engine label)`.
pub struct PickAsk {
    /// The dialog title.
    pub title: SharedString,
    /// A dim line under the title.
    pub detail: SharedString,
    /// The SQL the pick will open, boxed in mono.
    pub sql: SharedString,
    /// The connections offered, in order.
    pub rows: Vec<(SharedString, &'static str)>,
}

/// The connection picker: `pick-connection-dialog` holding rows `("pick-connection", n)`, each
/// labelled by its name, and `pick-cancel`. The receiver yields the picked row's index once;
/// Cancel or Escape closes it with nothing sent.
pub fn pick(spec: PickAsk, window: &mut Window, cx: &mut App) -> async_channel::Receiver<usize> {
    let (tx, rx) = async_channel::bounded(1);
    window.defer(cx, move |window, cx| {
        // Made once here: the builder runs every frame and must not rebuild the preview.
        let preview = sql_preview(&spec.sql, window, cx);
        window.open_dialog(cx, move |dialog, window, cx| {
            let dim = theme::dim(cx);
            let rows: Vec<_> = (spec.rows.iter().enumerate())
                .map(|(ix, (name, engine))| {
                    let tx = tx.clone();
                    Button::new(("pick-connection", ix))
                        .ghost()
                        .w_full()
                        .h(px(28.))
                        .accessibility_label(name.clone())
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(div().flex_1().child(name.clone()))
                                .child(div().text_sm().text_color(dim).child(*engine)),
                        )
                        .on_click(move |_, window, cx| {
                            let _ = tx.try_send(ix);
                            window.close_dialog(cx);
                        })
                })
                .collect();
            let cancel = ui::secondary(Button::new("pick-cancel"))
                .label("Cancel")
                .h(px(32.))
                .on_click(|_, window, cx| window.close_dialog(cx));
            let warning = cx.theme().warning;
            frame(dialog, DIALOG_WIDTH, Some(warning), cx)
                .margin_top(dialog_top(window))
                .child(
                    v_flex()
                        .id("pick-connection-dialog")
                        .test_support()
                        .child(title_row(
                            Some((ui::Icons::TriangleAlert, warning)),
                            spec.title.clone(),
                            cx,
                        ))
                        .child(
                            v_flex()
                                .gap_3()
                                .px_4()
                                .pt_2()
                                .pb_4()
                                .child(div().text_sm().text_color(dim).child(spec.detail.clone()))
                                .child(sql_box(spec.sql.clone(), &preview, cx))
                                .child(
                                    v_flex()
                                        .id("pick-connection-list")
                                        .max_h(px(240.))
                                        .overflow_y_scroll()
                                        .children(rows),
                                ),
                        )
                        .child(footer(cx).child(div().flex_1()).child(cancel)),
                )
        });
    });
    rx
}

/// Why a name dialog's trimmed text cannot be used; None accepts it.
pub type TextCheck = Rc<dyn Fn(&str) -> Option<SharedString>>;

/// The text a name dialog asks for and how it judges it.
pub struct TextAsk {
    /// The dialog title.
    pub title: SharedString,
    /// A dim line above the input.
    pub hint: SharedString,
    /// The OK button's label.
    pub ok: SharedString,
    /// The input's starting text, selected.
    pub initial: String,
    /// Why the trimmed text cannot be used; None accepts it.
    pub check: TextCheck,
}

/// The name dialog: `save-name` input, `save-name-error`, `save-cancel`, `save-ok` (Enter).
/// OK on text `check` refuses keeps the dialog open with the reason in `save-name-error`;
/// else the receiver yields the trimmed text once.
pub fn ask_text(
    spec: TextAsk,
    window: &mut Window,
    cx: &mut App,
) -> async_channel::Receiver<String> {
    let (tx, rx) = async_channel::bounded(1);
    // One input for the dialog's life: the builder runs every frame and must not reset it.
    let input = cx.new(|cx| InputState::new(window, cx).default_value(spec.initial.clone()));
    let focused = Rc::new(Cell::new(false));
    let problem: Rc<RefCell<Option<SharedString>>> = Rc::default();
    window.defer(cx, move |window, cx| {
        window.open_dialog(cx, move |dialog, window, cx| {
            if !focused.replace(true) {
                input.update(cx, |i, cx| {
                    i.focus(window, cx);
                    i.select_all(window, cx);
                });
            }
            // Enter reaches the dialog's OK through the single-line input, as does the button.
            // A refused text stays in the dialog with its reason, and the input keeps the keys.
            let send = {
                let (tx, input, problem) = (tx.clone(), input.clone(), Rc::clone(&problem));
                let check = Rc::clone(&spec.check);
                move |window: &mut Window, cx: &mut App| -> bool {
                    let text = input.read(cx).value().trim().to_owned();
                    let says = check(&text);
                    let refused = says.is_some();
                    *problem.borrow_mut() = says;
                    if refused {
                        input.update(cx, |i, cx| i.focus(window, cx));
                        window.refresh();
                        return false;
                    }
                    let _ = tx.try_send(text);
                    true
                }
            };
            let ok = {
                let send = send.clone();
                move |_: &_, window: &mut Window, cx: &mut App| send(window, cx)
            };
            let dim = theme::dim(cx);
            let danger = cx.theme().danger;
            let error = problem.borrow().clone().map(|says| {
                div()
                    .id("save-name-error")
                    .aria_label(says.clone())
                    .test_support()
                    .text_sm()
                    .text_color(danger)
                    .child(says)
            });
            let cancel = ui::secondary(Button::new("save-cancel"))
                .label("Cancel")
                .h(px(32.))
                .on_click(|_, window, cx| window.close_dialog(cx));
            let ok_button = Button::new("save-ok")
                .primary()
                .label(spec.ok.clone())
                .h(px(32.))
                .on_click(move |_, window, cx| {
                    if send(window, cx) {
                        window.close_dialog(cx);
                    }
                });
            frame(dialog, DIALOG_WIDTH, Some(danger), cx)
                .margin_top(dialog_top(window))
                .on_ok(ok)
                .child(
                    v_flex()
                        .child(title_row(None, spec.title.clone(), cx))
                        .child(
                            v_flex()
                                .gap_3()
                                .px_4()
                                .pt_2()
                                .pb_4()
                                .child(
                                    div()
                                        .id("save-hint")
                                        .aria_label(spec.hint.clone())
                                        .test_support()
                                        .text_sm()
                                        .text_color(dim)
                                        .child(spec.hint.clone()),
                                )
                                .child(
                                    div()
                                        .id("save-name")
                                        .test_support()
                                        .child(Input::new(&input)),
                                )
                                .children(error),
                        )
                        .child(
                            footer(cx)
                                .child(div().flex_1())
                                .child(with_keys(cancel, Some("Esc"), ChoiceStyle::Neutral, cx))
                                .child(with_keys(
                                    ok_button,
                                    Some("\u{23CE}"),
                                    ChoiceStyle::Primary,
                                    cx,
                                )),
                        ),
                )
        });
    });
    rx
}
