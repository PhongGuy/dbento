//! The editor's completion provider: asks the pure `completion` model at the caret against the
//! sidebar's catalog cache, shows its answer in the app's [`CompletionPopup`], and asks the
//! sidebar to load whatever level the answer found missing. It never waits on a load.

use std::cell::RefCell;

use dbento_core::Engine;
use gpui_kit::component::input::{CompletionProvider, Rope};
use gpui_kit::{App, Task, WeakEntity, Window};
use lsp_types::{CompletionContext, CompletionResponse};

use crate::completion::complete_in;
use crate::completion_popup::CompletionPopup;
use crate::sidebar::Sidebar;
use crate::statements::StatementIndex;

/// Completes SQL for one connection from the sidebar's cached catalog.
pub struct SqlCompletionProvider {
    sidebar: WeakEntity<Sidebar>,
    popup: WeakEntity<CompletionPopup>,
    conn: String,
    engine: Engine,
    // The schema unqualified names resolve in; None completes no unqualified relations.
    default_schema: Option<String>,
    // Statement ends found on earlier keystrokes, so a keystroke splits only near the caret.
    statements: RefCell<StatementIndex>,
}

impl SqlCompletionProvider {
    /// A provider for connection `conn` on `engine`, reading `sidebar`'s catalog tree and
    /// showing its answers in `popup`; unqualified relations complete from `default_schema`.
    pub fn new(
        sidebar: WeakEntity<Sidebar>,
        popup: WeakEntity<CompletionPopup>,
        conn: String,
        engine: Engine,
        default_schema: Option<String>,
    ) -> Self {
        SqlCompletionProvider {
            sidebar,
            popup,
            conn,
            engine,
            default_schema,
            statements: RefCell::new(StatementIndex::new(engine)),
        }
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        // The kit gets no items, so its own menu stays shut; the popup draws the answer.
        let none = Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        let (Some(sidebar), Some(popup)) = (self.sidebar.upgrade(), self.popup.upgrade()) else {
            return none;
        };
        let base = self.statements.borrow_mut().around(text, offset);
        let stmt = text.slice(base.clone()).to_string();
        let tree = sidebar.read(cx).tree();
        let answer = complete_in(
            self.engine,
            &stmt,
            base.start,
            offset,
            tree,
            &self.conn,
            self.default_schema.as_deref(),
        );
        // The provider runs inside the editor's update; the sidebar is another entity, so
        // starting its loads here is safe, and touching it only when needed keeps typing cheap.
        if !answer.wants.is_empty() {
            sidebar.update(cx, |s, cx| s.ensure_loaded(&self.conn, &answer.wants, cx));
        }
        let typed = text.slice(answer.replace.clone()).to_string();
        let len = text.len();
        popup.update(cx, |p, cx| p.show(answer, &typed, offset, len, cx));
        none
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        !new_text.is_empty()
    }
}
