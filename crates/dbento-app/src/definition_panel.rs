//! The definition dock: one schema object's current definition, read-only in a SQL editor,
//! with Reload, Copy, Open in query tab and Close. It names a connection and object while it
//! loads and takes only the reply for that pair.

use std::cell::RefCell;
use std::rc::Rc;

use dbento_core::{DEFINITION_CAP, Definition, ObjectRef, RelationRef};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement, IntoElement, ParentElement, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled, TestSupportExt, Window, div, px,
};

use crate::confirm::sql_preview;
use crate::schema_tree::constraint_words;
use crate::smooth_scroll::{self, Animator};
use crate::status_bar::first_line;
use crate::theme;
use crate::ui::{self, Icons};

// The editor's accessibility name is its first line cut to this many chars, never the text.
const EDITOR_LABEL_CHARS: usize = 200;

/// What the panel asks its owner to do.
#[derive(Debug, Clone, PartialEq)]
pub enum DefinitionPanelEvent {
    /// Load the shown object again.
    Reload {
        /// The connection's name.
        connection: String,
        /// The shown object.
        object: ObjectRef,
    },
    /// Open a new query tab holding `sql`, unrun.
    OpenInTab {
        /// The connection's name.
        connection: String,
        /// The shown text.
        sql: String,
    },
    /// Close the dock.
    Close,
}

enum State {
    Empty,
    Loading,
    Ready {
        definition: Definition,
        editor: Entity<EditorState>,
        // The editor's accessible name, cut once from the text rather than on every frame.
        name: SharedString,
        // The editor's eased wheel motion, the grid's own.
        wheel: Rc<RefCell<Animator>>,
    },
    Failed(String),
}

/// The definition dock's view: the shown connection and object, and its loading, ready or failed state.
pub struct DefinitionPanel {
    target: Option<(String, ObjectRef)>,
    state: State,
    // The editor a reload replaced, so its reply can take focus over when it held it.
    replaced: Option<FocusHandle>,
}

impl EventEmitter<DefinitionPanelEvent> for DefinitionPanel {}

impl DefinitionPanel {
    /// An empty panel naming nothing.
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            target: None,
            state: State::Empty,
            replaced: None,
        }
    }

    /// Names `object` on `connection` and shows the loading line.
    pub fn loading(&mut self, connection: &str, object: ObjectRef, cx: &mut Context<Self>) {
        self.target = Some((connection.to_owned(), object));
        if let State::Ready { editor, .. } = &self.state {
            self.replaced = Some(editor.read(cx).focus_handle(cx));
        }
        self.state = State::Loading;
        cx.notify();
    }

    /// Names nothing again, so any later reply is dropped.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.target = None;
        self.state = State::Empty;
        self.replaced = None;
        cx.notify();
    }

    /// Shows `result` when the panel names this connection and object; false when dropped.
    pub fn show(
        &mut self,
        connection: &str,
        object: &ObjectRef,
        result: Result<Definition, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let named = self
            .target
            .as_ref()
            .is_some_and(|(c, o)| c == connection && o == object);
        if !named {
            return false;
        }
        // A fresh editor per reply, so no highlight or scroll from an older text survives;
        // it takes focus when the editor it replaces held it.
        let refocus = self.replaced.take().is_some_and(|h| h.is_focused(window));
        self.state = match result {
            Ok(definition) => {
                let editor = sql_preview(definition.text(), window, cx);
                if refocus {
                    editor.read(cx).focus_handle(cx).focus(window, cx);
                }
                let name = first_line(definition.text())
                    .unwrap_or_default()
                    .chars()
                    .take(EDITOR_LABEL_CHARS)
                    .collect::<String>()
                    .into();
                State::Ready {
                    editor,
                    definition,
                    name,
                    wheel: Rc::default(),
                }
            }
            Err(message) => State::Failed(message),
        };
        cx.notify();
        true
    }

    /// The shown definition's text.
    pub fn text(&self) -> Option<&str> {
        match &self.state {
            State::Ready { definition, .. } => Some(definition.text()),
            _ => None,
        }
    }

    /// The read-only editor holding it.
    pub fn editor(&self) -> Option<&Entity<EditorState>> {
        match &self.state {
            State::Ready { editor, .. } => Some(editor),
            _ => None,
        }
    }
}

/// `"{Kind} {qualified}"`, names unquoted: `Index t_idx on public.t`, `Type public.mood`.
pub fn definition_title(object: &ObjectRef) -> String {
    let on = |rel: &RelationRef| format!("{}.{}", rel.schema(), rel.name());
    match object {
        // A non-exhaustive tuple variant matches only in braced form outside its crate.
        ObjectRef::Relation { 0: rel, .. } => format!("Relation {}", on(rel)),
        ObjectRef::Index { rel, name, .. } => format!("Index {name} on {}", on(rel)),
        ObjectRef::Constraint {
            rel, name, kind, ..
        } => {
            let name = name.as_deref().unwrap_or(constraint_words(*kind));
            format!("Constraint {name} on {}", on(rel))
        }
        ObjectRef::Trigger { rel, name, .. } => format!("Trigger {name} on {}", on(rel)),
        ObjectRef::Type { schema, name, .. } => format!("Type {schema}.{name}"),
        ObjectRef::Routine {
            schema,
            name,
            arguments,
            ..
        } => format!("Routine {schema}.{name}({arguments})"),
        _ => "Object".to_owned(),
    }
}

/// Says the text is composed from the catalog, cut at the cap, or both; None for a whole
/// engine-stored text.
pub fn definition_note(definition: &Definition) -> Option<String> {
    let cut = format!("First {} shown", cap_words(DEFINITION_CAP));
    let parts: Vec<String> = [
        definition
            .composed()
            .then(|| "Reconstructed from the catalog; may not run as is".to_owned()),
        definition.truncated().then_some(cut),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// "1 MiB" for 1 << 20, "256 KiB" for 1 << 18.
pub fn cap_words(bytes: usize) -> String {
    const MIB: usize = 1 << 20;
    match bytes {
        b if b >= MIB && b % MIB == 0 => format!("{} MiB", b / MIB),
        b if b % 1024 == 0 => format!("{} KiB", b / 1024),
        b => format!("{b} bytes"),
    }
}

impl Render for DefinitionPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        // Labels read by tests and assistive tech carry the shown text, as `footer-rows` does.
        let label = |id: &'static str, text: String| {
            div()
                .id(id)
                .role(Role::Label)
                .accessibility_id(id)
                .aria_value(text.clone())
                .aria_label(text.clone())
                .test_support()
                .child(text)
        };
        let button = |id: &'static str, text: &'static str| {
            ui::accessibility_id(Button::new(id), id)
                .ghost()
                .xsmall()
                .label(text)
        };
        let ready = matches!(self.state, State::Ready { .. });
        // Offered while loading too: a reply lost to a disconnect would otherwise strand it.
        let reload = self.target.is_some().then(|| {
            button("definition-reload", "Reload").on_click(cx.listener(|this, _, _, cx| {
                if let Some((connection, object)) = this.target.clone() {
                    cx.emit(DefinitionPanelEvent::Reload { connection, object });
                }
            }))
        });
        let close = ui::accessibility_id(Button::new("definition-close"), "definition-close")
            .ghost()
            .xsmall()
            .child(ui::icon(Icons::X, px(12.), muted))
            .tooltip("Close")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DefinitionPanelEvent::Close)));
        let header = ui::panel_header("Definition", cx)
            .gap_1()
            .children(reload)
            .child(close);
        let title = self
            .target
            .as_ref()
            .map(|(_, object)| label("definition-title", definition_title(object)));
        let note = match &self.state {
            State::Ready { definition, .. } => definition_note(definition)
                .map(|n| label("definition-note", n).text_xs().text_color(dim)),
            _ => None,
        };
        let body: AnyElement = match &self.state {
            State::Empty => div().into_any_element(),
            State::Loading => label("definition-loading", "Loading definition\u{2026}".into())
                .px_3()
                .text_sm()
                .text_color(muted)
                .into_any_element(),
            State::Failed(message) => label("definition-error", message.clone())
                .px_3()
                .text_sm()
                .text_color(danger)
                .into_any_element(),
            State::Ready {
                editor,
                name,
                wheel,
                ..
            } => div()
                .id("definition-editor")
                .role(Role::Group)
                .accessibility_id("definition-editor")
                .aria_label(name.clone())
                .test_support()
                .flex_1()
                .min_h_0()
                .relative()
                .child(smooth_scroll::layer(editor.clone(), Rc::clone(wheel)))
                .child(Editor::new(editor).readonly(true).bordered(false).h_full())
                .into_any_element(),
        };
        let actions =
            ready.then(|| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(button("definition-copy", "Copy").on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(text) = this.text() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                            }
                        },
                    )))
                    .child(button("definition-open-tab", "Open in query tab").on_click(
                        cx.listener(|this, _, _, cx| {
                            if let (Some((connection, _)), Some(sql)) = (&this.target, this.text())
                            {
                                let event = DefinitionPanelEvent::OpenInTab {
                                    connection: connection.clone(),
                                    sql: sql.to_owned(),
                                };
                                cx.emit(event);
                            }
                        }),
                    ))
            });
        div()
            .id("definition-panel")
            .role(Role::Group)
            .accessibility_id("definition-panel")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_hidden()
            .child(header)
            .children(title.map(|t| t.px_3().text_sm().truncate()))
            .children(note.map(|n| n.px_3()))
            .child(body)
            .children(actions)
    }
}
