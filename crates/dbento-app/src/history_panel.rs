//! The history panel: this workspace's recent statements, searchable and filtered by
//! connection, each opening its SQL in a new tab without running it.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dbento_core::{
    Color, Engine, HistoryEntry, HistoryFilter, HistoryOutcome, HistorySource, Workspace,
    line_comment_starts,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, TestSupportExt, WeakEntity, Window, div, px, uniform_list,
};

use crate::history::HistoryLog;
use crate::results_pane::format_rows;
use crate::sidebar::{color_name, tag_color};
use crate::status_bar::{first_line, format_elapsed};
use crate::theme;
use crate::ui::{self, Icons};

// The filter's label and first menu row when no connection is chosen.
const ALL_CONNECTIONS: &str = "all connections";

/// The most entries the panel lists.
pub const HISTORY_ROWS: usize = 200;

/// How often a shown panel redraws so its relative times ("2 min ago") stay current.
pub const TIME_REFRESH: Duration = Duration::from_secs(30);

// Two text lines, the SQL over its detail, as dense as the explorer's 24 px rows allow.
const ROW_HEIGHT: f32 = 40.;

/// What the panel asks its owner to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryPanelEvent {
    /// Open `sql` in a new tab on `connection`.
    Open {
        /// The connection the statement ran on.
        connection: String,
        /// The statement's SQL.
        sql: String,
    },
}

/// The history dock: entries of one workspace, newest first, narrowed by text and connection.
pub struct HistoryPanel {
    log: Entity<HistoryLog>,
    workspace: Arc<Workspace>,
    search: Entity<InputState>,
    connection: Option<String>,
    rows: Vec<HistoryEntry>,
    reads: u64,
    // One read at a time: a change while one runs, or while hidden, only marks the rows
    // stale, and they are read once when that read ends or the panel shows.
    visible: bool,
    loading: bool,
    stale: bool,
    _load: Task<()>,
    // Redraws for the relative times; only a shown panel holds one, so a hidden one never wakes.
    _tick: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<HistoryPanelEvent> for HistoryPanel {}

impl HistoryPanel {
    /// A hidden panel over `log` for `workspace`'s entries; it reloads whenever the log writes.
    pub fn new(
        log: Entity<HistoryLog>,
        workspace: Arc<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search SQL"));
        let changes = cx.subscribe_in(&search, window, |this, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.reload(cx);
            }
        });
        let written = cx.observe(&log, |this, _, cx| this.reload(cx));
        HistoryPanel {
            log,
            workspace,
            search,
            connection: None,
            rows: Vec::new(),
            reads: 0,
            visible: false,
            loading: false,
            stale: true,
            _load: Task::ready(()),
            _tick: None,
            _subscriptions: [changes, written],
        }
    }

    /// Shows or hides the panel; a hidden panel reads nothing, and reads once on showing if
    /// anything changed meanwhile.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if !visible {
            self._tick = None;
        } else if self._tick.is_none() {
            self._tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TIME_REFRESH).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
        if visible && self.stale {
            self.reload(cx);
        }
    }

    /// The workspace whose entries are listed.
    pub fn workspace(&self) -> &Arc<Workspace> {
        &self.workspace
    }

    /// Lists `ws`'s entries; a connection filter on a connection `ws` lacks goes back to all.
    pub fn set_workspace(&mut self, ws: Arc<Workspace>, cx: &mut Context<Self>) {
        if let Some(name) = &self.connection
            && ws.connection(name).is_none()
        {
            self.connection = None;
        }
        self.workspace = ws;
        self.reload(cx);
        cx.notify();
    }

    /// The entries listed, newest first.
    pub fn rows(&self) -> &[HistoryEntry] {
        &self.rows
    }

    /// How many reads of the history file this panel has started.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// The search box's state.
    pub fn search_state(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The connection the list is narrowed to; None lists every connection.
    pub fn connection_filter(&self) -> Option<&str> {
        self.connection.as_deref()
    }

    /// Narrows the list to `connection`, or lists every connection for None, and rereads.
    pub fn set_connection_filter(&mut self, connection: Option<String>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.reload(cx);
        cx.notify();
    }

    // Reads the matching entries off the main thread and shows them when the read ends.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        if !self.visible || self.loading {
            return;
        }
        self.stale = false;
        self.loading = true;
        let mut filter = HistoryFilter::default().workspace(self.workspace.dir());
        if let Some(conn) = &self.connection {
            filter = filter.connection(conn);
        }
        let text = self.search.read(cx).value();
        if !text.trim().is_empty() {
            filter = filter.text(text.trim());
        }
        self.reads += 1;
        let read = self.log.read(cx).recent(filter, HISTORY_ROWS, cx);
        self._load = cx.spawn(async move |this, cx| {
            let rows = read.await;
            let _ = this.update(cx, |panel, cx| {
                panel.loading = false;
                // Rows read before a change are already out of date, so they wait for the next.
                if panel.stale {
                    panel.reload(cx);
                } else {
                    panel.rows = rows;
                    cx.notify();
                }
            });
        });
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (mono, mono_size, hover, success, danger) = (
            theme.mono_font_family.clone(),
            theme.mono_font_size,
            theme.list_hover,
            theme.success,
            theme.danger,
        );
        let now = SystemTime::now();
        let end = range.end.min(self.rows.len());
        (range.start.min(end)..end)
            .map(|ix| {
                let entry = &self.rows[ix];
                let conn = self.workspace.connection(entry.connection());
                // A connection since removed lexes as Postgres, the stricter of the two.
                let engine = conn.map_or(Engine::Postgres, |c| c.engine());
                let sql = one_line(engine, entry.sql());
                let tag = conn.and_then(|c| c.effective_color());
                let detail = format!(
                    "{} \u{b7} {} \u{b7} {}",
                    entry.connection(),
                    outcome_label(entry.outcome()),
                    format_elapsed(entry.elapsed())
                );
                let mcp = entry.source() == HistorySource::Mcp;
                let (icon, tint) = outcome_mark(entry.outcome(), success, danger, dim);
                let glyph = ui::icon(icon, px(14.), tint);
                let when = relative_time(entry.at(), now);
                let open = HistoryPanelEvent::Open {
                    connection: entry.connection().to_owned(),
                    sql: entry.sql().to_owned(),
                };
                div()
                    .id(("history-row", ix))
                    .aria_label(sql.clone())
                    .test_support()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px_3()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(|s| s.bg(hover))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(open.clone())))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(glyph)
                            .child(tag_dot(ix, tag))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_size(mono_size)
                                    .child(sql),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id(("history-detail", ix))
                                    .aria_label(detail.clone())
                                    .test_support()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(dim)
                                    .child(detail),
                            )
                            // A run the AI made through `dbento mcp` is marked so it
                            // stands apart from the user's own statements.
                            .when(mcp, |row| {
                                row.child(
                                    ui::chip_base(cx)
                                        .id(("history-mcp", ix))
                                        .aria_label("MCP")
                                        .test_support()
                                        .text_size(px(11.))
                                        .child(ui::icon(Icons::Bot, px(12.), dim))
                                        .child("MCP"),
                                )
                            })
                            .child(div().flex_1())
                            // The row's two gaps plus this pad keep at least 12 px before the
                            // time even when the detail truncates against it.
                            .child(
                                div()
                                    .flex_none()
                                    .pl_2()
                                    .font_family(mono.clone())
                                    .text_size(px(11.))
                                    .text_color(dim)
                                    .child(when),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// `sql` as one line: each line trimmed, empty lines and lines that start with a `--`
/// comment dropped, joined by one space. A `--` inside a literal, quoted identifier or block
/// comment, lexed as `engine` does, is text; SQL that does not lex drops every `--` line.
/// A statement made only of comments shows its first non-empty line instead.
pub fn one_line(engine: Engine, sql: &str) -> String {
    let comments = line_comment_starts(engine, sql);
    let is_comment = |at: usize, line: &str| match &comments {
        Some(starts) => starts.binary_search(&at).is_ok(),
        None => line.starts_with("--"),
    };
    // Each line's offset in `sql`, so a trimmed line's start can be checked against the lexer.
    let mut at = 0;
    let mut kept = Vec::new();
    for raw in sql.split_inclusive('\n') {
        let line = raw.trim();
        let start = at + (raw.len() - raw.trim_start().len());
        at += raw.len();
        if !line.is_empty() && !is_comment(start, line) {
            kept.push(line);
        }
    }
    let joined = kept.join(" ");
    if !joined.is_empty() {
        return joined;
    }
    sql.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

// The connection's tag before the SQL; an untagged row keeps the 8 px slot so the SQL aligns.
fn tag_dot(ix: usize, tag: Option<Color>) -> AnyElement {
    let dot = div().size(px(8.)).flex_none().rounded_full();
    match tag {
        Some(c) => dot
            .id(("history-dot", ix))
            .aria_label(color_name(c))
            .test_support()
            .bg(tag_color(c))
            .into_any_element(),
        None => dot.into_any_element(),
    }
}

// The filter menu: "all connections" at 0, then each connection in workspace order, the
// current choice checked.
fn filter_menu(
    menu: PopupMenu,
    panel: &WeakEntity<HistoryPanel>,
    names: &[Option<String>],
    current: Option<&str>,
) -> PopupMenu {
    names.iter().enumerate().fold(menu, |menu, (n, name)| {
        let label: SharedString = name.as_deref().unwrap_or(ALL_CONNECTIONS).to_owned().into();
        let (panel, pick) = (panel.clone(), name.clone());
        let row = PopupMenuItem::element(move |_, _| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .child(label.clone())
                .id(("history-filter", n))
                .aria_label(label.clone())
                .test_support()
        })
        .checked(name.as_deref() == current)
        .on_click(move |_, _, cx| {
            let _ = panel.update(cx, |p, cx| p.set_connection_filter(pick.clone(), cx));
        });
        menu.item(row)
    })
}

/// "3 rows", "3 rows (truncated)", "2 rows affected", "failed: <first line>", "cancelled",
/// "skipped".
pub fn outcome_label(o: &HistoryOutcome) -> String {
    match o {
        HistoryOutcome::Ok {
            rows_affected: Some(n),
            ..
        } => format!("{} affected", format_rows(*n)),
        HistoryOutcome::Ok {
            rows,
            truncated: true,
            ..
        } => format!("{} (truncated)", format_rows(*rows)),
        HistoryOutcome::Ok { rows, .. } => format_rows(*rows),
        HistoryOutcome::Failed { message } => {
            format!("failed: {}", first_line(message).unwrap_or_default())
        }
        HistoryOutcome::Cancelled => "cancelled".into(),
        HistoryOutcome::Skipped => "skipped".into(),
        // An outcome core adds later reads plainly until the panel learns it.
        _ => "ended".into(),
    }
}

/// How long before `now` the run at `at` was, coarsely: "just now" under a minute (or when
/// `at` is after `now`), then floored minutes, hours or days ("3 min ago", "2 h ago", "5 d ago").
pub fn relative_time(at: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(at).map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// The glyph a history row leads with and its tint: a `success` check for a run that
/// succeeded, a `danger` cross for a failure, else a `dim` cross.
pub fn outcome_mark(
    o: &HistoryOutcome,
    success: Hsla,
    danger: Hsla,
    dim: Hsla,
) -> (IconName, Hsla) {
    match o {
        HistoryOutcome::Ok { .. } => (Icons::CircleCheck, success),
        HistoryOutcome::Failed { .. } => (Icons::CircleX, danger),
        HistoryOutcome::Cancelled | HistoryOutcome::Skipped | _ => (Icons::CircleX, dim),
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, chrome, edge, radius) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.sidebar,
            theme.border,
            theme.radius,
        );
        let filter_label = self
            .connection
            .clone()
            .unwrap_or_else(|| ALL_CONNECTIONS.into());
        let names: Vec<Option<String>> = std::iter::once(None)
            .chain(
                self.workspace
                    .connections()
                    .iter()
                    .map(|c| Some(c.name().to_owned())),
            )
            .collect();
        let (panel, current) = (cx.entity().downgrade(), self.connection.clone());
        let header = ui::panel_header("History", cx).gap_2().child(
            Button::new("history-connection")
                .ghost()
                .xsmall()
                .label(filter_label)
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    filter_menu(menu, &panel, &names, current.as_deref())
                }),
        );
        let search = ui::search_input("history-search", &self.search, cx);
        let filtered = self.connection.is_some() || !self.search.read(cx).value().trim().is_empty();
        let body = if self.rows.is_empty() {
            div()
                .id("history-empty")
                .test_support()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child(if filtered {
                    "No matching statements."
                } else {
                    "No statements yet."
                })
                .into_any_element()
        } else {
            uniform_list(
                "history-list",
                self.rows.len(),
                cx.processor(|this, range: Range<usize>, _, cx| this.render_rows(range, cx)),
            )
            .test_support()
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div()
            .id("history-panel")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(radius)
            .border_1()
            .border_color(edge)
            .bg(chrome)
            .text_color(fg)
            .child(header)
            .child(search)
            .child(body)
    }
}
