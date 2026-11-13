//! The window's root view: the title bar over either the welcome view or the open
//! workspace's layout, with the status bar at the foot.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dbento_core::{
    ApprovalError, ApprovalRequest, ApprovalStore, Browse, Color, ConnectionSpec, Decision, Edit,
    EditPlan, Engine, FolderPath, GrantState, ObjectRef, RelationKind, RelationRef, SavedQuery,
    Secret, SecretError, SecretKey, SecretSource, SessionOptions, Sort, Workspace, WorkspaceItem,
    WriteError, connect, move_secret, plan_edit, resolve_key_passphrase, resolve_secret,
    secret_env_var, tls_key_is_encrypted,
};
use futures_util::future::{Either, join, join_all, select};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ObservedElement, ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, ResizableState, Sizable as _, TITLE_BAR_HEIGHT, TitleBar,
    WindowExt as _, h_resizable, resizable_panel, resize_handle_appearance, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action as _, AnyView, App, AppContext as _, BackgroundExecutor, ClickEvent, Context,
    DismissEvent, Div, ElementId, Entity, EntityId, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement, IntoElement, ModifiersChangedEvent, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Render, Role, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Subscription, Task, TestSupportExt, WeakEntity, Window, div, img, px,
    transparent_black,
};

use crate::app::{
    BrowseSaved, SavedState, Services, SortSaved, TabState, TableState, WindowState,
    WorkspaceSession,
};
use crate::approvals::{
    APPROVAL_POLL, Answer, answered_elsewhere_notice, approved_notice, ask_approval, next_pending,
    pending_requests, still_pending, verdict_of,
};
use crate::approvals_panel::{ApprovalsPanel, Decided, Outcome};
use crate::browse::BrowseEvent;
use crate::catalog::CatalogClosing;
use crate::confirm::{self, Ask, Choice, TextAsk, ask, ask_text};
use crate::connection_dialog::{ConnectionForm, ConnectionFormEvent, Field, TestState};
use crate::definition_panel::{DefinitionPanel, DefinitionPanelEvent};
use crate::drafts::{DraftSlot, Drafts};
use crate::history::HistoryLog;
use crate::history_panel::{HistoryPanel, HistoryPanelEvent};
use crate::inspector::{Inspector, InspectorEvent};
use crate::keymap::{
    CloseTab, GoToLastTab, GoToTab1, GoToTab2, GoToTab3, GoToTab4, GoToTab5, GoToTab6, GoToTab7,
    GoToTab8, MoveTabLeft, MoveTabRight, NavBack, NavForward, NewConnection, NewTab, OpenCommands,
    OpenPalette, OpenQuery, OpenSettings, RecentTabBack, RecentTabForward, ReloadWorkspace,
    SaveQuery, ToggleHistory, ToggleRowInspector, chip_keys, keys_for,
};
use crate::link::LinkState;
use crate::nav::{Hop, NavHistory};
use crate::palette::{self, Mode, PaletteCommand, PaletteEvent, PaletteInit, TableRef};
use crate::persist::ChangeQueue;
use crate::query_files::{ask_name, name_problem, query_file_name};
use crate::query_tab::{QueryTab, QueryTabEvent, TOOLBAR_HEIGHT};
use crate::recent_tabs::Recent;
use crate::restore::plan_restore;
use crate::results_pane::ResultView;
use crate::runner::RunnerSpec;
use crate::schema_tree::{NodeKey, select_text};
use crate::settings_dialog::{SettingsView, SettingsViewEvent};
use crate::settings_store::{self, SettingsEvent, SettingsStore};
use crate::sidebar::{Sidebar, SidebarEvent, color_name, engine_label, tag_color};
use crate::staged::discard_title;
use crate::status_bar::{STATUS_HEIGHT, status_bar};
use crate::tab_model::{Closing, TabModel};
use crate::theme::{self, Appearance, CycleAppearance};
use crate::ui::{self, Icons};

/// The database a Postgres Test opens when the form names none: every server has it.
const TEST_DATABASE_POSTGRES: &str = "postgres";
/// How long a passed Test waits for the server's database list before offering none.
const TEST_DATABASES_CAP: Duration = Duration::from_secs(5);

// The narrowest the tab area gets before the side panels give way, and their own minimum.
const MIN_TAB_AREA: Pixels = px(160.);
const PANEL_MIN: Pixels = px(100.);

// The list `list` answers within `cap`, or empty when it fails; `None` once the cap passes,
// so the caller abandons the session the list still holds.
async fn listed_within<E>(
    cap: Duration,
    list: impl Future<Output = Result<Vec<String>, E>>,
) -> Option<Vec<String>> {
    let listed = tokio::time::timeout(cap, list).await.ok()?;
    Some(listed.unwrap_or_default())
}

// A failed save of the app state is logged, never fatal.
fn save_state(file: &ChangeQueue<SavedState>) {
    if let Err(e) = file.write() {
        eprintln!("dbento: cannot save the app state: {e}");
    }
}

// Plans `edit` on `ws` and writes it for the connection dialog, which shows a refusal
// inline: its text, and whether the file changed on disk since load.
fn plan_and_write(ws: &Workspace, edit: &Edit) -> Result<EditPlan, (String, bool)> {
    let plan = plan_edit(ws, edit).map_err(|e| (e.to_string(), false))?;
    plan.apply()
        .map_err(|e| (e.to_string(), matches!(e, WriteError::Changed)))?;
    Ok(plan)
}

// Every APPROVAL_POLL, starting at once, refreshes the pending list and this session's grant
// readings. The refresh has its own task so the list and the rail badge stay live while
// `ask_approvals` waits on an open dialog.
fn poll_approvals(dir: PathBuf, window: &mut Window, cx: &mut Context<Shell>) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        loop {
            let Ok(recheck) = this.update(cx, |shell, _| shell.grants_to_recheck()) else {
                return;
            };
            let read = dir.clone();
            let (pending, states) = cx
                .background_executor()
                .spawn(async move {
                    let states: Vec<(usize, GrantState)> = ApprovalStore::open(&read)
                        .map(|store| {
                            recheck
                                .into_iter()
                                .map(|(ix, id)| (ix, store.grant_state(&id)))
                                .collect()
                        })
                        .unwrap_or_default();
                    (pending_requests(&read), states)
                })
                .await;
            let read = this.update(cx, |shell, cx| shell.approvals_read(pending, states, cx));
            if read.is_err() {
                return;
            }
            cx.background_executor().timer(APPROVAL_POLL).await;
        }
    })
}

// Every APPROVAL_POLL, starting at once, shows the oldest pending MCP request when no other
// dialog is open, waits for its answer and records it, then looks again without waiting; one
// request is on screen at a time.
// When another window answered first, the dialog closes or the answer loses, and the notice
// shows what won. Any other failed record goes to stderr and the request shows again.
fn ask_approvals(dir: PathBuf, window: &mut Window, cx: &mut Context<Shell>) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        let free = |cx: &mut gpui_kit::AsyncWindowContext| {
            this.update_in(cx, |_, window, cx| !window.has_active_dialog(cx))
        };
        loop {
            match free(cx) {
                // The window is gone.
                Err(_) => return,
                Ok(false) => {}
                Ok(true) => {
                    let read = dir.clone();
                    let shown = cx
                        .background_executor()
                        .spawn(async move { next_pending(&read) })
                        .await;
                    // Another dialog may have opened during the read; then the next poll asks.
                    if let (Some(shown), Ok(true)) = (shown, free(cx)) {
                        let request = shown.request.clone();
                        // The count reads the same list as the rail badge, so the two agree.
                        let Ok(asked) = this.update_in(cx, |_, window, cx| {
                            let shell = cx.weak_entity();
                            let queued = move |cx: &App| {
                                shell
                                    .upgrade()
                                    .map_or(0, |s| s.read(cx).approvals_pending())
                            };
                            ask_approval(shown, queued, window, cx)
                        }) else {
                            return;
                        };
                        let bg = cx.background_executor().clone();
                        let answer = std::pin::pin!(asked.answer.recv());
                        let gone = std::pin::pin!(answered_elsewhere(
                            dir.clone(),
                            request.id().to_owned(),
                            bg.clone()
                        ));
                        let answered = match select(answer, gone).await {
                            Either::Left((answer, _)) => answer.ok(),
                            Either::Right(_) => {
                                let verdict = read_verdict(&dir, &request, &bg).await;
                                let _ = this.update_in(cx, |_, window, cx| {
                                    asked.close(window, cx);
                                    ui::notice(window, cx, answered_elsewhere_notice(verdict));
                                });
                                None
                            }
                        };
                        if let Some(answered) = answered {
                            record_answer(&this, &dir, request, answered, cx).await;
                        }
                        continue;
                    }
                }
            }
            cx.background_executor().timer(APPROVAL_POLL).await;
        }
    })
}

// Records this window's answer. A recorded answer joins the list and only Approve asks for a
// retry; one that lost to another window shows the winner; any other failure goes to stderr.
async fn record_answer(
    this: &WeakEntity<Shell>,
    dir: &Path,
    request: ApprovalRequest,
    answered: Answer,
    cx: &mut gpui_kit::AsyncWindowContext,
) {
    let bg = cx.background_executor().clone();
    let (store_dir, id) = (dir.to_path_buf(), request.id().to_owned());
    let Answer { decision, reason } = answered;
    let recorded = bg
        .spawn(async move {
            ApprovalStore::open(&store_dir)
                .and_then(|s| s.decide_with_reason(&id, decision, reason.as_deref()))
        })
        .await;
    match recorded {
        Ok(()) => {
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.approvals_decided(request, decision, cx);
                if decision == Decision::Approve {
                    ui::notice(window, cx, approved_notice());
                }
            });
        }
        // The first answer stands; this window shows it instead of its own.
        Err(ApprovalError::NotPending { .. }) => {
            let verdict = read_verdict(dir, &request, &bg).await;
            let _ = this.update_in(cx, |_, window, cx| {
                ui::notice(window, cx, answered_elsewhere_notice(verdict));
            });
        }
        Err(e) => eprintln!("dbento: cannot record an approval: {e}"),
    }
}

// Resolves once a poll finds `id` no longer pending; an unreadable folder keeps waiting.
async fn answered_elsewhere(dir: PathBuf, id: String, bg: BackgroundExecutor) {
    loop {
        bg.timer(APPROVAL_POLL).await;
        let (dir, id) = (dir.clone(), id.clone());
        let pending = bg.spawn(async move { still_pending(&dir, &id) }).await;
        if pending == Some(false) {
            return;
        }
    }
}

// The stored winner of `request`, read off the main thread.
async fn read_verdict(
    dir: &Path,
    request: &ApprovalRequest,
    bg: &BackgroundExecutor,
) -> dbento_core::Verdict {
    let (dir, request) = (dir.to_path_buf(), request.clone());
    bg.spawn(async move { verdict_of(&dir, &request) }).await
}

// One answer of this session. `tentative` marks a first Expired reading not yet shown.
struct SessionDecision {
    decided: Decided,
    tentative: bool,
}

/// How long a window close waits for its sessions to close before closing anyway.
pub const QUIT_CAP: Duration = Duration::from_secs(12);

/// The most database sessions opening a workspace may start; the rest connect on first Run.
pub const PRECONNECT_CAP: usize = 4;

/// Waits for `sessions` or `cap`, whichever ends first; true when the cap fired first.
pub async fn wait_capped(
    sessions: impl Future<Output = ()>,
    cap: impl Future<Output = ()>,
) -> bool {
    let sessions = std::pin::pin!(sessions);
    let cap = std::pin::pin!(cap);
    matches!(select(sessions, cap).await, Either::Right(_))
}

/// The root view of the main window.
pub struct Shell {
    services: Services,
    // Every tab records its ended statements here.
    history_log: Entity<HistoryLog>,
    // The open workspace's history dock, shown on the right while `history_open`.
    history_panel: Option<Entity<HistoryPanel>>,
    history_open: bool,
    // The row inspector, sharing the dock slot with the history panel while `inspector_open`;
    // it follows the active tab whether shown or not.
    inspector: Entity<Inspector>,
    inspector_open: bool,
    // The sidebar, tab area and dock split; its sizes are what the close saves.
    split: Entity<ResizableState>,
    // The saved layout the panels and every new tab start from.
    sidebar_width: Option<f32>,
    editor_height: Option<f32>,
    // The welcome view's recent workspaces, newest first.
    recent: Vec<PathBuf>,
    // The state file as read at start plus this shell's changes since, which also wait in
    // `state_file` for a write; `saving` says a writer is running.
    saved: SavedState,
    saving: bool,
    state_file: Arc<ChangeQueue<SavedState>>,
    // Tables opened this session, newest first, for the palette.
    recent_tables: Vec<TableRef>,
    requested: Option<PathBuf>,
    workspace: Option<Arc<Workspace>>,
    sidebar: Option<Entity<Sidebar>>,
    tabs: Vec<Entity<QueryTab>>,
    // Per tab, index for index: the model observation the status bar follows, the tab's own
    // events, and the browse changes that re-save the session.
    tab_observes: Vec<[Subscription; 3]>,
    active: Option<usize>,
    // The tabs in most-recently-used order for Ctrl+Tab, and the keystroke watch that ends a
    // walk when any other key is pressed.
    tab_recency: Recent<EntityId>,
    _walk_keys: Subscription,
    error: Option<String>,
    appearance: Appearance,
    // A window close is waiting on the sessions; the next close request goes through.
    closing: bool,
    // Sessions of closed tabs, each drained off the main thread until it ends; a window close
    // waits for the unfinished ones too.
    closings: Vec<(Arc<AtomicBool>, Task<()>)>,
    // A discard question (window close, tab close, switch, disconnect) is on screen, so no
    // second one stacks on it; `close_confirmed` records Close on the window close question.
    asking: Option<Question>,
    questions: u64,
    close_confirmed: bool,
    // Tabs are being reopened from the saved session, which is saved once at the end.
    restoring: bool,
    // The next "Query N" for an untitled tab; numbers are never reused within a session.
    next_query: usize,
    // The explorer's selection serial as the last tab activation left it, so `new_tab` can
    // tell a selection the user made since.
    activated_serial: u64,
    // The tab handle last right-clicked, which the strip's one context menu acts on.
    tab_menu_target: Option<usize>,
    // The tab menu is open; tab tooltips are off while it is.
    tab_menu_open: bool,
    // The root's focus, so window-wide actions reach the shell from any view.
    focus: FocusHandle,
    _os_appearance: Subscription,
    // The settings store; `appearance` mirrors its choice and its save failures become toasts.
    settings: Entity<SettingsStore>,
    _settings_events: [Subscription; 2],
    _sidebar_events: Option<Subscription>,
    // The foot reads the sidebar's link, so its changes redraw the shell.
    _sidebar_notify: Option<Subscription>,
    _history_events: Option<Subscription>,
    _inspector_events: Subscription,
    // The open connection dialog's form and its event subscription.
    connection_form: Option<(Entity<ConnectionForm>, Subscription)>,
    // The open Settings dialog's view and its Close subscription.
    settings_view: Option<(Entity<SettingsView>, Subscription)>,
    // The workspace the open form was built on; Save refuses once another is installed.
    form_base: Option<Arc<Workspace>>,
    // The running Test connection; dropping it abandons the attempt.
    connection_test: Option<Task<()>>,
    // MCP requests waiting for an answer, as the approvals poll last read them.
    approvals_waiting: Vec<ApprovalRequest>,
    // This session's recorded answers, newest first, and the dock panel that lists both.
    approvals_decided: Vec<SessionDecision>,
    approvals_panel: Entity<ApprovalsPanel>,
    approvals_open: bool,
    // The definition dock's panel, shown while `definition_open`, a flag never saved.
    definition_panel: Entity<DefinitionPanel>,
    definition_open: bool,
    _definition_events: Subscription,
    // The approvals poll, alive as long as the window.
    _approvals: Task<()>,
    _approval_asks: Task<()>,
}

impl Shell {
    /// A shell over `services` and the app state `saved` as read from its file, in the
    /// `appearance` choice, opening the workspace folder `dir` when given, with the history
    /// panel showing when `history_open`. Under System the theme follows later OS appearance
    /// changes.
    pub fn new(
        services: Services,
        saved: SavedState,
        dir: Option<PathBuf>,
        appearance: Appearance,
        history_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // An installed store wins over the seed; without one (bench) an in-memory store is made.
        let settings = settings_store::ensure(appearance, cx);
        let appearance = settings.read(cx).get().appearance;
        theme::apply(appearance, window.appearance(), window, cx);
        let settings_events = [
            cx.observe_in(&settings, window, |this, store, window, cx| {
                let appearance = store.read(cx).get().appearance;
                if this.appearance != appearance {
                    this.appearance = appearance;
                    theme::apply(appearance, window.appearance(), window, cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&settings, window, |_, _, event, window, cx| match event {
                SettingsEvent::SaveFailed(error) => ui::notice(
                    window,
                    cx,
                    ui::Notice {
                        text: format!(
                            "settings.json could not be saved ({error}); changes last until quit"
                        ),
                        kind: ui::NoticeKind::Warning,
                    },
                ),
            }),
        ];
        let os_appearance = cx.observe_window_appearance(window, |this, window, cx| {
            theme::apply(this.appearance, window.appearance(), window, cx);
        });
        let history_log = cx.new(|_| HistoryLog::new(services.paths.history.clone()));
        let state_file = Arc::new(ChangeQueue::new(services.paths.state.clone()));
        let approvals = poll_approvals(services.paths.approvals.clone(), window, cx);
        let approval_asks = ask_approvals(services.paths.approvals.clone(), window, cx);
        let inspector = cx.new(|cx| Inspector::new(window, cx));
        let inspector_events =
            cx.subscribe_in(
                &inspector,
                window,
                |this, _, event, window, cx| match event {
                    InspectorEvent::Close => this.toggle_inspector(cx),
                    InspectorEvent::Follow {
                        connection,
                        relation,
                        filter,
                    } => {
                        this.open_filtered(connection, relation, filter.clone(), window, cx);
                    }
                },
            );
        let definition_panel = cx.new(DefinitionPanel::new);
        let definition_events = cx.subscribe_in(
            &definition_panel,
            window,
            |this, _, event, window, cx| match event {
                DefinitionPanelEvent::Reload { connection, object } => {
                    this.view_definition(connection, object.clone(), window, cx)
                }
                DefinitionPanelEvent::OpenInTab { connection, sql } => {
                    this.open_sql(connection, sql, window, cx)
                }
                DefinitionPanelEvent::Close => this.set_dock(false, false, false, false, cx),
            },
        );
        // A walk ends on releasing Ctrl, and on any keystroke other than its own steps.
        let walk_keys = cx.observe_keystrokes(|this: &mut Shell, e, _, _| {
            let step = e.action.as_ref().is_some_and(|a| {
                a.name() == RecentTabBack.name() || a.name() == RecentTabForward.name()
            });
            if !step {
                this.tab_recency.commit();
            }
        });
        let mut shell = Shell {
            services,
            history_log,
            history_panel: None,
            history_open,
            inspector,
            inspector_open: saved.inspector_open && !history_open,
            split: cx.new(|_| ResizableState::default()),
            sidebar_width: saved.sidebar_width,
            editor_height: saved.editor_height,
            recent: saved.recent_workspaces.clone(),
            state_file,
            saved,
            saving: false,
            recent_tables: Vec::new(),
            requested: dir.clone(),
            workspace: None,
            sidebar: None,
            tabs: Vec::new(),
            tab_observes: Vec::new(),
            active: None,
            tab_recency: Recent::default(),
            _walk_keys: walk_keys,
            error: None,
            appearance,
            closing: false,
            closings: Vec::new(),
            asking: None,
            questions: 0,
            close_confirmed: false,
            restoring: false,
            next_query: 1,
            activated_serial: 0,
            tab_menu_target: None,
            tab_menu_open: false,
            focus,
            _os_appearance: os_appearance,
            settings,
            _settings_events: settings_events,
            _sidebar_events: None,
            _sidebar_notify: None,
            _history_events: None,
            _inspector_events: inspector_events,
            connection_form: None,
            settings_view: None,
            form_base: None,
            connection_test: None,
            approvals_waiting: Vec::new(),
            approvals_decided: Vec::new(),
            approvals_panel: cx.new(ApprovalsPanel::new),
            approvals_open: false,
            definition_panel,
            definition_open: false,
            _definition_events: definition_events,
            _approvals: approvals,
            _approval_asks: approval_asks,
        };
        // The OS close goes through `request_close`; a shell already gone lets it close.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |shell, cx| shell.request_close(window, cx))
                .unwrap_or(true)
        });
        if let Some(dir) = dir {
            shell.open_workspace(dir, window, cx);
        }
        shell
    }

    /// Whether the window may close now. Every tab's draft is written first; when any cannot
    /// be, it asks, answering false; Close then continues below, Keep open cancels. The first
    /// request with tabs or a catalog session closes them all, shows "closing sessions" when
    /// there is a tab, answers false, and removes the window once every closing resolves or
    /// `QUIT_CAP` passes. With neither, or on a second request, it answers true. Another
    /// discard question on screen is withdrawn as if answered Keep; while its own question is
    /// on screen it answers false and asks nothing.
    pub fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            self.flush_saved();
            return true;
        }
        match self.asking.take() {
            Some(q) if q.window_close => {
                self.asking = Some(q);
                return false;
            }
            Some(q) => q.dialog.close(window, cx),
            None => {}
        }
        if !self.close_confirmed {
            let lost = self.flush_drafts(cx);
            let staged = self.staged_total(cx);
            let running = running_titles(self.tabs.iter(), cx);
            if lost > 0 || staged > 0 || !running.is_empty() {
                self.ask_close(lost, staged, &running, window, cx);
                return false;
            }
        }
        self.save_layout(window, cx);
        self.flush_saved();
        let catalogs = self
            .sidebar
            .as_ref()
            .map(|s| s.update(cx, |s, _| s.close_catalogs()))
            .unwrap_or_default();
        if self.tabs.is_empty() && catalogs.is_empty() && self.pending_closings() == 0 {
            return true;
        }
        self.closing = true;
        // The tabs stay on screen so the status bar can say what the window waits for.
        let tabs: Vec<Closing> = self
            .tabs
            .iter()
            .map(|tab| {
                let model = tab.read(cx).model().clone();
                model.update(cx, |m, cx| m.close(cx))
            })
            .collect();
        let earlier: Vec<Task<()>> = self.closings.drain(..).map(|(_, task)| task).collect();
        let sessions = async move {
            let catalogs = join_all(catalogs.into_iter().map(CatalogClosing::wait));
            let tabs = join_all(tabs.into_iter().map(Closing::wait));
            drop(join(join(tabs, catalogs), join_all(earlier)).await);
        };
        // The cap bounds the wait whatever a session does, so a quit never hangs.
        let cap = cx.background_executor().timer(QUIT_CAP);
        cx.spawn_in(window, async move |this, cx| {
            if wait_capped(sessions, cap).await {
                eprintln!(
                    "dbento: sessions still closing after {} s; closing the window anyway",
                    QUIT_CAP.as_secs()
                );
            }
            let _ = this.update(cx, |shell, _| shell.flush_saved());
            let _ = cx.update(|window, _| window.remove_window());
        })
        .detach();
        cx.notify();
        false
    }

    // Asks whether closing the window may discard `staged` edits and the SQL of `lost` tabs
    // whose drafts could not be written, and cancel the statements of the `running` tabs, in
    // one question; Close takes the title bar's path, so the bounded shutdown runs exactly as
    // for an unedited window.
    fn ask_close(
        &mut self,
        lost: usize,
        staged: usize,
        running: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, mut parts) = match staged {
            0 => ("Close Dbento?".to_owned(), Vec::new()),
            n => (
                discard_title(n),
                vec!["Closing Dbento drops them.".to_owned()],
            ),
        };
        parts.extend((lost > 0).then(|| lost_detail(lost)));
        parts.extend(running_detail("Closing Dbento", running));
        let detail = parts.join(" ");
        self.ask_once(
            title,
            detail,
            "Keep open",
            window,
            cx,
            |shell, window, cx| shell.close_unasked(window, cx),
        );
        if let Some(q) = &mut self.asking {
            q.window_close = true;
        }
    }

    // Asks `ask_discard`'s question unless one is already on screen, running `close` on the
    // shell when the answer is Close. A question withdrawn meanwhile counts as Keep.
    fn ask_once(
        &mut self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        keep: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        close: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.asking.is_some() {
            return;
        }
        self.questions += 1;
        let serial = self.questions;
        let dialog = ask_discard(title, detail, keep, window, cx);
        let reply = dialog.answer.clone();
        self.asking = Some(Question {
            serial,
            window_close: false,
            dialog,
        });
        cx.spawn_in(window, async move |this, cx| {
            let answer = reply.recv().await;
            let _ = this.update_in(cx, |shell, window, cx| {
                if shell.asking.as_ref().map(|q| q.serial) != Some(serial) {
                    return;
                }
                shell.asking = None;
                if answer == Ok(1) {
                    close(shell, window, cx);
                }
            });
        })
        .detach();
    }

    // Records the window bounds, the sidebar width and the active tab's editor height; a
    // panel never laid out keeps its saved size.
    fn save_layout(&mut self, window: &Window, cx: &mut Context<Self>) {
        let state = WindowState::from_bounds(window.window_bounds());
        let sidebar = self
            .sidebar
            .as_ref()
            .and_then(|_| self.split.read(cx).sizes().first().copied())
            .map(f32::from);
        // A table tab opens mostly results, which is not the height the user chose.
        let editor = self
            .tab()
            .map(|t| t.read(cx))
            .filter(|t| t.origin().is_none())
            .and_then(|t| t.editor_height(cx));
        self.update_saved(
            move |s| {
                s.window = Some(state);
                s.sidebar_width = sidebar.or(s.sidebar_width);
                s.editor_height = editor.or(s.editor_height);
            },
            cx,
        );
    }

    // Staged edits summed over every tab, for the questions that drop them all.
    fn staged_total(&self, cx: &App) -> usize {
        self.tabs.iter().map(|t| staged_count(t, cx)).sum()
    }

    /// The title bar's close button: `request_close`, removing the window when it answers true.
    pub fn title_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.request_close(window, cx) {
            window.remove_window();
        }
    }

    /// Closes the window as Close on the close question does: nothing more is asked, and any
    /// running statement is cancelled by the bounded shutdown.
    pub fn close_unasked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_confirmed = true;
        self.title_close(window, cx);
    }

    /// The services this window was started with.
    pub fn services(&self) -> &Services {
        &self.services
    }

    /// The workspace folder resolved at startup (the argument, else the saved one), whether
    /// or not it opened.
    pub fn requested(&self) -> Option<&Path> {
        self.requested.as_deref()
    }

    /// The current appearance choice.
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// Moves the store to the next appearance choice; the store observer applies it here.
    pub fn cycle_appearance(&mut self, cx: &mut Context<Self>) {
        let next = self.appearance.next();
        self.settings
            .update(cx, |s, cx| s.change(cx, |v| v.appearance = next));
    }

    // Each change touches only its own fields and is replayed onto the file as it is then,
    // so another instance's appearance, recents and sessions survive. One writer runs at a
    // time, off the UI thread, taking every change queued so far: a burst costs one write.
    fn update_saved(
        &mut self,
        change: impl Fn(&mut SavedState) + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        change(&mut self.saved);
        self.state_file.push(Box::new(change));
        if std::mem::replace(&mut self.saving, true) {
            return;
        }
        let file = Arc::clone(&self.state_file);
        cx.spawn(async move |this, cx| {
            while file.pending() {
                let file = Arc::clone(&file);
                cx.background_spawn(async move { save_state(&file) }).await;
            }
            let _ = this.update(cx, |s, _| s.saving = false);
        })
        .detach();
    }

    // Writes any state change no writer has reached yet, now and on this thread, for a window
    // about to close, which may end the process before a background write lands.
    fn flush_saved(&mut self) {
        save_state(&self.state_file);
    }

    // Records the open tabs as this workspace's session, so the next start reopens them.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = &self.workspace else {
            return;
        };
        if self.restoring {
            return;
        }
        // A tab without a draft slot (a key's value tab) has nothing to restore, so it is left
        // out, and the active index moves to the kept tab at or before it.
        let kept: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| self.tabs[i].read(cx).draft_id().is_some())
            .collect();
        let active = self.active.and_then(|a| {
            let before = kept.iter().filter(|&&i| i <= a).count();
            (!kept.is_empty()).then(|| before.saturating_sub(1))
        });
        let tabs = kept
            .iter()
            .map(|&i| {
                let tab = self.tabs[i].read(cx);
                let model = tab.model().read(cx);
                TabState {
                    connection: model.spec().connection.clone(),
                    draft: tab.draft_id().unwrap_or_default().to_owned(),
                    file: tab.file().map(Path::to_path_buf),
                    table: tab.origin().map(|rel| table_state(rel, model)),
                }
            })
            .collect();
        let session = WorkspaceSession {
            workspace: ws.dir().to_path_buf(),
            tabs,
            active,
            collapsed: self
                .sidebar
                .as_ref()
                .map(|s| s.read(cx).collapsed_paths())
                .unwrap_or_default(),
        };
        self.update_saved(move |s| s.put_session(session.clone()), cx);
    }

    // Writes every tab's draft now, before the tabs go away; answers how many edited tabs
    // could not be kept.
    fn flush_drafts(&mut self, cx: &mut Context<Self>) -> usize {
        self.tabs
            .iter()
            .filter(|tab| tab.update(cx, |t, cx| t.flush_draft(cx)).is_err())
            .count()
    }

    /// The open workspace, if any.
    pub fn workspace(&self) -> Option<&Arc<Workspace>> {
        self.workspace.as_ref()
    }

    /// The open Settings dialog's view, if any.
    pub fn settings_view(&self) -> Option<&Entity<SettingsView>> {
        self.settings_view.as_ref().map(|(v, _)| v)
    }

    /// The open connection dialog's form, if any.
    pub fn connection_form(&self) -> Option<&Entity<ConnectionForm>> {
        self.connection_form.as_ref().map(|(f, _)| f)
    }

    /// The open workspace's connection list, if any.
    pub fn sidebar(&self) -> Option<&Entity<Sidebar>> {
        self.sidebar.as_ref()
    }

    /// Whether the tab strip's context menu is open; tab tooltips are off while it is.
    pub fn tab_menu_open(&self) -> bool {
        self.tab_menu_open
    }

    /// The open query tabs, left to right.
    pub fn tabs(&self) -> &[Entity<QueryTab>] {
        &self.tabs
    }

    /// The index of the active tab; None when no tab is open.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// The query history every tab records into.
    pub fn history_log(&self) -> &Entity<HistoryLog> {
        &self.history_log
    }

    /// The open workspace's history panel, if any.
    pub fn history_panel(&self) -> Option<&Entity<HistoryPanel>> {
        self.history_panel.as_ref()
    }

    /// Whether the history panel shows.
    pub fn history_open(&self) -> bool {
        self.history_open
    }

    /// The state of the sidebar, tab area and history dock split.
    pub fn split_state(&self) -> &Entity<ResizableState> {
        &self.split
    }

    /// Shows the history panel when hidden (closing the inspector), else hides it; saved.
    pub fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.set_dock(!self.history_open, false, false, false, cx);
    }

    /// The row inspector.
    pub fn inspector(&self) -> &Entity<Inspector> {
        &self.inspector
    }

    /// Whether the row inspector shows.
    pub fn inspector_open(&self) -> bool {
        self.inspector_open
    }

    /// Shows the inspector when hidden (closing the history panel), else hides it; saved.
    pub fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.set_dock(false, !self.inspector_open, false, false, cx);
    }

    /// Whether the approvals panel shows.
    pub fn approvals_open(&self) -> bool {
        self.approvals_open
    }

    /// Shows the approvals panel when hidden (closing history or the inspector), else hides
    /// it; not saved.
    pub fn toggle_approvals(&mut self, cx: &mut Context<Self>) {
        self.set_dock(false, false, !self.approvals_open, false, cx);
        self.show_approvals(cx);
    }

    /// Opens the definition dock on `object` and asks the sidebar to load it.
    pub fn view_definition(
        &mut self,
        connection: &str,
        object: ObjectRef,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sidebar) = self.sidebar.clone() else {
            return;
        };
        self.definition_panel
            .update(cx, |p, cx| p.loading(connection, object.clone(), cx));
        self.set_dock(false, false, false, true, cx);
        sidebar.update(cx, |s, cx| s.load_definition(connection, &object, cx));
    }

    /// Whether the definition panel shows.
    pub fn definition_open(&self) -> bool {
        self.definition_open
    }

    /// The definition panel, shown or not.
    pub fn definition_panel(&self) -> &Entity<DefinitionPanel> {
        &self.definition_panel
    }

    // The dock slot holds at most one panel; the history and inspector flags are saved
    // together, the approvals and definition flags never.
    fn set_dock(
        &mut self,
        history: bool,
        inspector: bool,
        approvals: bool,
        definition: bool,
        cx: &mut Context<Self>,
    ) {
        (self.history_open, self.inspector_open) = (history, inspector);
        self.approvals_open = approvals;
        self.definition_open = definition;
        if let Some(panel) = &self.history_panel {
            panel.update(cx, |p, cx| p.set_visible(history, cx));
        }
        self.update_saved(
            move |s| (s.history_open, s.inspector_open) = (history, inspector),
            cx,
        );
        for tab in &self.tabs {
            let pane = tab.read(cx).results_pane().clone();
            pane.update(cx, |p, cx| p.set_inspector_open(inspector, cx));
        }
        cx.notify();
    }

    // Points the inspector at the active tab, so its subscriptions change with the tab
    // rather than on every render.
    fn follow_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab().cloned();
        self.inspector.update(cx, |i, cx| i.follow(tab, window, cx));
    }

    /// A new tab on `connection` (else the active tab's, else the first) holding `sql`, marked
    /// saved; never runs.
    pub fn open_sql(
        &mut self,
        connection: &str,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let Some(conn) = self.fallback_connection(&ws, Some(connection), cx) else {
            return;
        };
        // The new tab is the one pushed; nothing opens while the window closes.
        let before = self.tabs.len();
        self.open_connection_named(&conn, window, cx);
        let Some(tab) = self.tabs.get(before).cloned() else {
            return;
        };
        tab.update(cx, |t, cx| {
            t.set_sql(sql, window, cx);
            t.mark_saved(cx);
        });
    }

    /// A history row's `sql` in a new tab on `connection` when that name still resolves.
    /// Else nothing opens: a warning names the missing connection and a picker asks which
    /// connection the SQL opens on, so a row never lands silently on another one.
    pub fn open_history(
        &mut self,
        connection: &str,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if ws.connection(connection).is_some() {
            return self.open_sql(connection, sql, window, cx);
        }
        let text = format!("{connection} no longer exists; pick a connection to run this.");
        let notice = ui::Notice {
            text,
            kind: ui::NoticeKind::Warning,
        };
        ui::notice(window, cx, notice);
        let names: Vec<String> = ws
            .connections()
            .iter()
            .map(|c| c.name().to_owned())
            .collect();
        let spec = confirm::PickAsk {
            title: format!("{connection} no longer exists").into(),
            detail: "Pick a connection to open this on.".into(),
            sql: sql.to_owned().into(),
            rows: (ws.connections().iter())
                .map(|c| (c.name().to_owned().into(), engine_label(c.engine())))
                .collect(),
        };
        let picked = confirm::pick(spec, window, cx);
        let sql = sql.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(ix) = picked.recv().await else {
                return;
            };
            let Some(name) = names.get(ix) else {
                return;
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.open_sql(name, &sql, window, cx)
            });
        })
        .detach();
    }

    /// The active query tab, if any.
    pub fn tab(&self) -> Option<&Entity<QueryTab>> {
        self.tabs.get(self.active?)
    }

    /// Shows tab `ix`, focuses its editor and selects its connection in the explorer. A tab
    /// whose session never started connects now; a failed connect is retried by the next Run.
    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        self.tab_recency.touch(tab.entity_id());
        self.show(ix, window, cx);
    }

    // Shows tab `ix` as `activate` does, leaving the recent order alone for a Ctrl+Tab walk.
    fn show(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        self.active = Some(ix);
        let model = tab.read(cx).model().clone();
        if model.read(cx).events().is_none() {
            model.update(cx, |m, cx| m.connect(cx));
        }
        let editor = tab.read(cx).editor().clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
        self.show_in_explorer(ix, cx);
        if let Some(sidebar) = &self.sidebar {
            self.activated_serial = sidebar.read(cx).selection_serial();
        }
        self.follow_active(window, cx);
        self.save_session(cx);
        cx.notify();
    }

    // One Ctrl+Tab step: shows the next tab of the walk, back to older or forward to newer.
    fn walk_recent(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tab_recency.step(back) else {
            return;
        };
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            self.show(ix, window, cx);
        }
    }

    // Ctrl+1 to Ctrl+8 by `ix`, Ctrl+9 as None for the last tab; a missing tab does nothing.
    fn go_to_tab(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let ix = ix.or(self.tabs.len().checked_sub(1));
        if let Some(ix) = ix.filter(|ix| *ix < self.tabs.len()) {
            self.activate(ix, window, cx);
        }
    }

    /// Moves tab `from` to position `to` in the strip, keeping the active tab active, and saves
    /// the new order to the session. An index past the last tab changes nothing.
    pub fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let active = self.tab().map(Entity::entity_id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        let observes = self.tab_observes.remove(from);
        self.tab_observes.insert(to, observes);
        self.active = active.and_then(|id| self.tabs.iter().position(|t| t.entity_id() == id));
        self.tab_menu_target = None;
        self.save_session(cx);
        cx.notify();
    }

    // Ctrl+Shift+PageUp and PageDown: the active tab one place over, stopping at either end.
    fn move_active(&mut self, right: bool, cx: &mut Context<Self>) {
        let Some(at) = self.active else {
            return;
        };
        let to = if right {
            Some(at + 1)
        } else {
            at.checked_sub(1)
        };
        if let Some(to) = to {
            self.move_tab(at, to, cx);
        }
    }

    // A table tab reveals its relation in the explorer; any other tab selects its connection.
    fn show_in_explorer(&self, ix: usize, cx: &mut Context<Self>) {
        let (Some(tab), Some(sidebar), Some(ws)) =
            (self.tabs.get(ix), &self.sidebar, &self.workspace)
        else {
            return;
        };
        let (name, origin) = {
            let tab = tab.read(cx);
            let name = tab.model().read(cx).spec().connection.clone();
            (name, tab.origin().cloned())
        };
        // Explorer rows are declared connections, so a child tab without an origin selects none.
        let row = ws.connections().iter().position(|c| c.name() == name);
        sidebar.update(cx, |s, cx| match (origin, row) {
            (Some(rel), _) => s.reveal(NodeKey::Relation { conn: name, rel }, cx),
            (None, Some(ix)) => s.select(ix, cx),
            (None, None) => {}
        });
    }

    // The name of the connection selected in the explorer: a declared one or a derived child.
    fn selected_connection(&self, cx: &App) -> Option<String> {
        self.sidebar.as_ref()?.read(cx).selected_name()
    }

    // The name of the workspace's declared connection `ix`.
    fn declared_name(&self, ix: usize) -> Option<String> {
        let ws = self.workspace.as_ref()?;
        Some(ws.connections().get(ix)?.name().to_owned())
    }

    // The connection a tab opens on: `wanted` when it resolves, else the active tab's, else
    // the first declared one.
    fn fallback_connection(
        &self,
        ws: &Workspace,
        wanted: Option<&str>,
        cx: &App,
    ) -> Option<String> {
        let active = self
            .tab()
            .map(|t| t.read(cx).model().read(cx).spec().connection.clone());
        let resolves = |name: &&str| ws.connection(name).is_some();
        wanted
            .filter(resolves)
            .map(str::to_owned)
            .or_else(|| active.filter(|a| resolves(&a.as_str())))
            .or_else(|| Some(ws.connections().first()?.name().to_owned()))
    }

    /// Opens a tab on the connection selected in the explorer since the last tab activation,
    /// else on the active tab's (the explorer's selection when no tab is open), and activates it.
    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let moved = (self.sidebar.as_ref())
            .is_some_and(|s| s.read(cx).selection_serial() != self.activated_serial);
        let selected = self.selected_connection(cx);
        // A tab whose connection is gone counts as no tab, so the selection decides.
        let tab = self
            .tab()
            .map(|t| t.read(cx).model().read(cx).spec().connection.clone())
            .filter(|name| {
                (self.workspace.as_ref()).is_some_and(|ws| ws.connection(name).is_some())
            });
        if let Some(name) = new_tab_connection(tab, selected, moved) {
            self.open_connection_named(&name, window, cx);
        }
    }

    /// Opens a new tab on the workspace's connection `ix` and activates it. Its session
    /// connects at once, so the first Run does not wait on the handshake. Nothing opens while
    /// the window closes, since the close would not wait for it.
    pub fn open_connection(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.declared_name(ix) {
            self.open_connection_named(&name, window, cx);
        }
    }

    /// As `open_connection`, on a declared or derived connection; nothing for an unknown name.
    pub fn open_connection_named(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.open_unnumbered(name, window, cx) {
            self.number_untitled(&tab, cx);
        }
    }

    // Opens and activates a new tab on connection `name` without naming it, for callers that
    // give it a file or an origin; activating the new tab connects it.
    fn open_unnumbered(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<QueryTab>> {
        let tab = self.push_tab(name, Drafts::new_id(), window, cx)?;
        self.activate(self.tabs.len() - 1, window, cx);
        Some(tab)
    }

    // Gives `tab` the next "Query N" when nothing else names it: no file, origin or number.
    fn number_untitled(&mut self, tab: &Entity<QueryTab>, cx: &mut Context<Self>) {
        let t = tab.read(cx);
        if t.file().is_some() || t.origin().is_some() || t.number().is_some() {
            return;
        }
        let n = self.next_query;
        self.next_query += 1;
        tab.update(cx, |t, cx| {
            t.set_number(n);
            cx.notify();
        });
    }

    // Appends an unconnected tab on connection `name`, declared or derived, that keeps its
    // edited text as draft `draft_id`; None without that connection or while the window closes.
    fn push_tab(
        &mut self,
        name: &str,
        draft_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<QueryTab>> {
        let ws = self.workspace.clone()?;
        let conn = ws.connection(name)?.into_owned();
        if self.closing {
            return None;
        }
        let spec = RunnerSpec {
            workspace: ws,
            connection: conn.name().to_owned(),
            store: Arc::clone(&self.services.store),
            env: Arc::clone(&self.services.env),
        };
        let log = self.history_log.clone();
        let model = cx.new(|_| {
            let mut model = TabModel::new(spec);
            model.set_history(log);
            model
        });
        let sidebar = self.sidebar.as_ref().map(Entity::downgrade);
        let draft = DraftSlot::new(Drafts::new(self.services.paths.drafts.clone()), draft_id);
        let height = self.editor_height.map(px);
        let tab = cx.new(|cx| {
            let mut tab = QueryTab::new(model.clone(), conn, sidebar, Some(draft), window, cx);
            if let Some(h) = height {
                tab.set_editor_height(h);
            }
            tab
        });
        // The status bar reads the active model, so every applied event batch re-renders; the
        // explorer's connection marks follow the sessions too. Staging an edit pins a preview
        // and clears its history, so neither a follow nor Back replaces the rows edited.
        let weak = tab.downgrade();
        let observe = cx.observe(&model, move |this, model, cx| {
            this.sync_links(cx);
            if !model.read(cx).edits().is_empty()
                && let Some(tab) = weak.upgrade()
                && (tab.read(cx).is_preview() || tab.read(cx).nav().current().is_some())
            {
                tab.update(cx, |t, cx| {
                    t.pin(cx);
                    t.nav_mut().clear();
                });
            }
            cx.notify();
        });
        let events = cx.subscribe_in(&tab, window, |this, tab, event, window, cx| match event {
            QueryTabEvent::Reconnect => this.ask_reconnect(tab, window, cx),
            QueryTabEvent::SwitchConnection(name) => {
                this.switch_tab_connection(tab, name, window, cx)
            }
            QueryTabEvent::OpenTable(rel) => {
                let name = tab.read(cx).model().read(cx).spec().connection.clone();
                this.open_relation_named(&name, rel, RelationKind::Table, window, cx);
            }
        });
        // A filter, a sort or leaving the browse changes what the session restores.
        let browse = cx.subscribe(&model, |this, _, e: &BrowseEvent, cx| {
            if matches!(e, BrowseEvent::Run(_) | BrowseEvent::Cleared) {
                this.save_session(cx);
            }
        });
        let inspector = self.inspector_open;
        let pane = tab.read(cx).results_pane().clone();
        pane.update(cx, |p, cx| p.set_inspector_open(inspector, cx));
        self.tab_observes.push([observe, events, browse]);
        self.tab_recency.add(tab.entity_id());
        self.tabs.push(tab.clone());
        Some(tab)
    }

    /// A click on connection `ix` in the explorer: activates the newest query tab on it, else
    /// opens one.
    pub fn click_connection(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.declared_name(ix) else {
            return;
        };
        // A key tab hides its editor, so opening the connection skips it for one that has one.
        let newest = self.tabs.iter().rposition(|tab| {
            let model = tab.read(cx).model().read(cx);
            model.spec().connection == name && model.key_view().is_none()
        });
        match newest {
            Some(tab) => self.activate(tab, window, cx),
            None => self.open_connection(ix, window, cx),
        }
    }

    /// Opens `rel` on `connection`. A table opens as a browse tab: the tab whose browse
    /// state or pending target is `rel` is activated and re-run, else a new tab loads the
    /// target and runs once it lands. Any other kind shows `select_text` and runs it, in a tab
    /// already holding exactly that SELECT or a new one, so no edited SQL is ever replaced.
    pub fn open_relation(
        &mut self,
        connection: usize,
        rel: &RelationRef,
        kind: RelationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = self.declared_name(connection) {
            self.open_relation_named(&name, rel, kind, window, cx);
        }
    }

    /// As `open_relation`, on a declared or derived connection by name.
    pub fn open_relation_named(
        &mut self,
        connection: &str,
        rel: &RelationRef,
        kind: RelationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = (self.workspace.as_ref())
            .and_then(|ws| ws.connection(connection))
            .map(Cow::into_owned)
        else {
            return;
        };
        if self.closing {
            return;
        }
        let recent = TableRef {
            connection: conn.name().to_owned(),
            rel: rel.clone(),
            kind,
        };
        palette::push_recent(&mut self.recent_tables, recent);
        let sql = select_text(&conn, rel);
        let table = kind == RelationKind::Table;
        if let Some(ix) = self.relation_tab(conn.name(), rel, &sql, table, cx) {
            self.activate(ix, window, cx);
            // A pending browse tab runs when its target lands; one whose target failed asks
            // for it again; any other reused tab re-runs.
            if let Some(tab) = self.tab().cloned() {
                tab.update(cx, |t, cx| {
                    let m = t.model().read(cx);
                    let (browsing, waiting) = (m.browse().is_some(), m.browses());
                    if !table || browsing {
                        t.run_all(cx);
                    } else if !waiting {
                        t.model()
                            .clone()
                            .update(cx, |m, cx| m.load_target(rel.clone(), cx));
                    }
                });
            }
            return;
        }
        self.open_relation_tab(connection, rel, table, None, window, cx);
    }

    /// Shows `rel` of connection `connection` (by name) filtered by `filter` in the active
    /// tab's preview: the active tab when it is a preview, else the preview opened from it,
    /// else a new preview after the other tabs. A preview on another connection, or whose SQL
    /// the user edited, is pinned and a new one opens. The preview is activated, selects a
    /// single landed row and records the hop for Back. False when no connection has that name.
    pub fn open_filtered(
        &mut self,
        connection: &str,
        rel: &RelationRef,
        filter: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !(self.workspace.as_ref()).is_some_and(|ws| ws.connection(connection).is_some()) {
            return false;
        }
        if self.closing {
            return false;
        }
        let origin = self.tab().cloned();
        let target = origin.as_ref().and_then(|o| {
            if o.read(cx).is_preview() {
                return self.active;
            }
            let id = o.entity_id();
            self.tabs
                .iter()
                .position(|t| t.read(cx).preview_of() == Some(id))
        });
        let on_conn = |t: &Entity<QueryTab>, cx: &App| {
            t.read(cx).model().read(cx).spec().connection == connection
        };
        let target = match target {
            Some(t) if !on_conn(&self.tabs[t], cx) || self.tabs[t].read(cx).edited(cx) => {
                self.pin_tab(t, cx);
                None
            }
            t => t,
        };
        let hop = Hop::new(rel.clone(), filter.clone());
        let tab = match target {
            Some(t) => {
                self.activate(t, window, cx);
                self.load_hop(t, rel, filter, window, cx);
                self.tabs[t].clone()
            }
            None => {
                let Some(tab) =
                    self.open_relation_tab(connection, rel, true, Some(filter), window, cx)
                else {
                    return true;
                };
                // The new tab previews the one followed from; without one it is an ordinary tab.
                if let Some(o) = &origin {
                    let id = o.entity_id();
                    tab.update(cx, |t, cx| {
                        t.set_preview_of(id);
                        cx.notify();
                    });
                }
                let pane = tab.read(cx).results_pane().clone();
                pane.update(cx, |p, cx| p.select_single_landed(cx));
                tab
            }
        };
        tab.update(cx, |t, _| t.nav_mut().push(hop));
        cx.notify();
        true
    }

    // Shows `rel` filtered by `filter` in tab `t`, selecting a single landed row. A hop within
    // the relation already browsed only swaps the filter, so staged edits get the discard question.
    fn load_hop(
        &mut self,
        t: usize,
        rel: &RelationRef,
        filter: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(t).cloned() else {
            return;
        };
        let model = tab.read(cx).model().clone();
        let same = model
            .read(cx)
            .browse()
            .is_some_and(|b| same_relation(model.read(cx), b.target().relation(), rel));
        if same {
            model.update(cx, |m, cx| m.apply_filter(&filter, cx));
        } else {
            self.load_relation(t, rel, true, Some(filter), window, cx);
        }
        let pane = tab.read(cx).results_pane().clone();
        pane.update(cx, |p, cx| p.select_single_landed(cx));
    }

    /// Steps the active tab back to its previous hop and loads it; false when there is
    /// nothing to walk to, or the tab holds SQL the user typed, which a hop would replace.
    pub fn nav_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.nav_step(NavHistory::back, window, cx)
    }

    /// Steps the active tab forward to its next hop and loads it; false as for `nav_back`.
    pub fn nav_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.nav_step(NavHistory::forward, window, cx)
    }

    fn nav_step(
        &mut self,
        step: fn(&mut NavHistory) -> Option<Hop>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(t) = self.active.filter(|_| !self.closing) else {
            return false;
        };
        let tab = self.tabs[t].clone();
        if tab.read(cx).edited(cx) {
            return false;
        }
        let Some(hop) = tab.update(cx, |t, _| step(t.nav_mut())) else {
            return false;
        };
        self.load_hop(t, &hop.relation, hop.filter, window, cx);
        cx.notify();
        true
    }

    /// Pins tab `ix`, so no later follow replaces it.
    pub fn pin_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(ix).cloned() {
            tab.update(cx, |t, cx| t.pin(cx));
            cx.notify();
        }
    }

    // The tab on connection `name` showing `rel`: one holding exactly its select `sql`, or for
    // a `table` one browsing it or waiting for its target. A table whose target failed is the
    // unsaved tab opened from it that still holds only its select.
    fn relation_tab(
        &self,
        name: &str,
        rel: &RelationRef,
        sql: &str,
        table: bool,
        cx: &App,
    ) -> Option<usize> {
        self.tabs.iter().position(|tab| {
            let tab = tab.read(cx);
            let m = tab.model().read(cx);
            if m.spec().connection != name {
                return false;
            }
            let same = |r: &RelationRef| same_relation(m, r, rel);
            if !table {
                return tab.sql(cx) == sql;
            }
            if !m.browses() {
                return tab.file().is_none()
                    && tab.origin().is_some_and(same)
                    && tab.sql(cx) == sql;
            }
            m.browse().is_some_and(|b| same(b.target().relation()))
                || m.target_pending().is_some_and(same)
                || m.deferred_target().is_some_and(same)
        })
    }

    /// Opens Redis key `key` of DB `db` on connection `connection` in a new value tab, which
    /// reads the value's first page at once.
    pub fn open_key(
        &mut self,
        connection: usize,
        db: u32,
        key: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.declared_name(connection) else {
            return;
        };
        if let Some(tab) = self.open_unnumbered(&name, window, cx) {
            tab.update(cx, |t, cx| t.open_key(db, key, cx));
            // Opening recorded the tab while it still had a draft; it now has none to keep.
            self.save_session(cx);
        }
    }

    // Opens a new tab on `connection` holding `rel`'s select and loads it (`load_relation`);
    // None when the tab cannot open.
    fn open_relation_tab(
        &mut self,
        connection: &str,
        rel: &RelationRef,
        table: bool,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<QueryTab>> {
        let tab = self.open_unnumbered(connection, window, cx)?;
        // The new table tab gives a fifth of the query split to the editor.
        let split = (window.viewport_size().height - query_chrome()).max(px(0.));
        tab.update(cx, |t, _| t.set_editor_height(split * 0.2));
        self.load_relation(self.tabs.len() - 1, rel, table, filter, window, cx);
        Some(tab)
    }

    // Points tab `ix` at `rel`'s select, marked saved. A table loads its target, whose first
    // browse carries `filter`; anything else runs the select at once.
    fn load_relation(
        &mut self,
        ix: usize,
        rel: &RelationRef,
        table: bool,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(ix).cloned() else {
            return;
        };
        tab.update(cx, |t, cx| {
            point_at(t, rel, window, cx);
            // A table runs nothing until its target lands as a `BrowseEvent`.
            if table {
                t.model()
                    .clone()
                    .update(cx, |m, cx| m.load_target_filtered(rel.clone(), filter, cx));
            } else {
                t.run_all(cx);
            }
            cx.notify();
        });
        // Activation ran before the tab had this origin, so reveal it now.
        self.show_in_explorer(ix, cx);
    }

    /// Opens the palette on the saved queries (`#` mode), read afresh so files added on disk show.
    pub fn open_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Mode::Queries, window, cx);
    }

    /// Opens the palette in `mode` over the explorer's loaded catalog, the saved queries (read
    /// afresh; an error shows "Cannot open") and the recent tables. Nothing without a workspace.
    pub fn open_palette(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(ws), Some(sidebar)) = (self.workspace.clone(), self.sidebar.clone()) else {
            return;
        };
        let queries = match ws.saved_queries() {
            Ok(queries) => queries,
            Err(e) => return show_error("Cannot open", e.to_string(), window, cx),
        };
        let init = PaletteInit {
            sidebar,
            workspace: ws,
            queries,
            recent: self.recent_tables.clone(),
        };
        let palette = palette::open(init, mode, window, cx);
        cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
            window.close_dialog(cx);
            this.on_palette(event, window, cx);
        })
        .detach();
    }

    /// The tables opened this session, newest first (at most 20).
    pub fn recent_tables(&self) -> &[TableRef] {
        &self.recent_tables
    }

    // Connections are named in palette rows; a name the workspace no longer has opens nothing.
    fn on_palette(&mut self, event: &PaletteEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaletteEvent::OpenTable(t) => {
                self.open_relation_named(&t.connection, &t.rel, t.kind, window, cx)
            }
            PaletteEvent::OpenQuery(q) => self.open_saved(q, window, cx),
            PaletteEvent::OpenConnection(name) => self.open_connection_named(name, window, cx),
            PaletteEvent::Run(cmd) => self.run_palette_command(*cmd, window, cx),
        }
    }

    /// Runs `cmd` after the palette closed. Tab commands (RunStatement, RunAll, FormatSql,
    /// ToggleComment, CancelQuery) focus the active tab's editor and dispatch the action; the rest call the
    /// shell's own method. Without a tab, tab commands do nothing.
    pub fn run_palette_command(
        &mut self,
        cmd: PaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use PaletteCommand as C;
        match cmd {
            C::NewTab => self.new_tab(window, cx),
            C::CloseTab => {
                if let Some(ix) = self.active {
                    self.close_tab_at(ix, window, cx);
                }
            }
            C::SaveQuery => self.save_active(window, cx),
            C::NewConnection => self.new_connection(window, cx),
            C::ReloadWorkspace => self.reload_workspace(window, cx),
            C::ToggleHistory => self.toggle_history(cx),
            C::ToggleRowInspector => self.toggle_inspector(cx),
            C::CycleAppearance => self.cycle_appearance(cx),
            C::OpenSettings => self.open_settings(window, cx),
            // Like the `query-save` button: focus the editor so the action reaches the tab's
            // own handlers, whatever the dialog refocused on close.
            C::RunStatement
            | C::RunAll
            | C::FormatSql
            | C::Explain
            | C::ExplainAnalyze
            | C::ToggleComment
            | C::CancelQuery => {
                let (Some(tab), Some(action)) = (self.tab().cloned(), cmd.action()) else {
                    return;
                };
                let editor = tab.read(cx).editor().clone();
                editor.update(cx, |e, cx| e.focus(window, cx));
                window.dispatch_action(action, cx);
            }
            // The palette handles it itself and never emits it.
            C::KeyboardShortcuts => {}
        }
    }

    /// Activates the tab holding `q`, else opens it in a new tab on its folder's connection
    /// (unbound: the active tab's, else the first). A failed read shows "Cannot open".
    pub fn open_saved(&mut self, q: &SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.read(cx).file() == Some(q.path()))
        {
            return self.activate(ix, window, cx);
        }
        let text = match ws.read_query(q) {
            Ok(text) => text,
            Err(e) => return show_error("Cannot open", e.to_string(), window, cx),
        };
        let Some(conn) = self.fallback_connection(&ws, q.connection(), cx) else {
            return;
        };
        // Nothing opens while the window closes.
        let Some(tab) = self.open_unnumbered(&conn, window, cx) else {
            return;
        };
        tab.update(cx, |t, cx| {
            t.set_sql(&text, window, cx);
            t.set_file(q.path().to_path_buf(), text.clone(), cx);
        });
        self.save_session(cx);
    }

    /// Saves the active tab: to its file when it has one, else under a name asked for,
    /// relative to `queries/<connection>/`, confirming before an existing file is replaced.
    pub fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab().cloned() else {
            return;
        };
        if let Some(rel) = tab.read(cx).file().map(Path::to_path_buf) {
            return self.save_bound(&tab, &rel, window, cx);
        }
        let conn = tab.read(cx).model().read(cx).spec().connection.clone();
        // The file takes the extension of the connection's query language (`.js` on MongoDB).
        let ext = tab
            .read(cx)
            .conn()
            .engine()
            .capabilities()
            .language
            .file_extension();
        let tab = tab.downgrade();
        let reply = ask_name("untitled", ext, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(name) = reply.recv().await else {
                return;
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                if let Some(tab) = tab.upgrade() {
                    shell.save_as(&tab, &conn, name.trim(), ext, window, cx);
                }
            });
        })
        .detach();
    }

    // Saves `tab` to its own file `rel`, unless the file changed since the tab last read or
    // wrote it: then asks Reload, Overwrite or Cancel. A deleted file is simply recreated, and
    // one that cannot be read shows "Cannot save" and writes nothing.
    fn save_bound(
        &mut self,
        tab: &Entity<QueryTab>,
        rel: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        match ws.read_query_at(rel) {
            Ok(Some(text)) if tab.read(cx).disk() != Some(text.as_str()) => {}
            Ok(_) => return self.write_tab(tab, rel, window, cx),
            Err(e) => return show_error("Cannot save", e.to_string(), window, cx),
        }
        let reply = ask(
            Ask {
                title: "The file changed on disk".into(),
                detail: Some(
                    format!(
                        "{} was changed outside this tab since it was opened.",
                        rel.display()
                    )
                    .into(),
                ),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "save-conflict-cancel",
                        label: "Cancel".into(),
                        danger: false,
                    },
                    Choice {
                        id: "save-conflict-reload",
                        label: "Reload".into(),
                        danger: false,
                    },
                    Choice {
                        id: "save-conflict-overwrite",
                        label: "Overwrite".into(),
                        danger: true,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        let (tab, rel) = (tab.downgrade(), rel.to_path_buf());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice @ 1..=2) = reply.recv().await else {
                return;
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                let Some(tab) = tab.upgrade() else {
                    return;
                };
                if choice == 2 {
                    return shell.write_tab(&tab, &rel, window, cx);
                }
                shell.reload_tab(&tab, &rel, window, cx);
            });
        })
        .detach();
    }

    // Replaces `tab`'s text with what its file holds now, read again since the question was
    // asked; a file gone or unreadable meanwhile shows "Cannot reload" and changes nothing.
    fn reload_tab(
        &mut self,
        tab: &Entity<QueryTab>,
        rel: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let text = match ws.read_query_at(rel) {
            Ok(Some(text)) => text,
            Ok(None) => {
                let gone = format!("{} no longer exists.", rel.display());
                return show_error("Cannot reload", gone, window, cx);
            }
            Err(e) => return show_error("Cannot reload", e.to_string(), window, cx),
        };
        tab.update(cx, |t, cx| {
            t.set_sql(&text, window, cx);
            t.set_file(rel.to_path_buf(), text.clone(), cx);
        });
        self.save_session(cx);
        cx.notify();
    }

    // Saves `tab` as `name` with extension `ext` in `conn`'s folder; an existing file is
    // replaced only on Replace.
    fn save_as(
        &mut self,
        tab: &Entity<QueryTab>,
        conn: &str,
        name: &str,
        ext: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        // The dialog refuses these; a caller that skipped it writes nothing.
        if name_problem(name, ext).is_some() {
            return;
        }
        let rel = Path::new("queries")
            .join(conn)
            .join(query_file_name(name, ext));
        // Ask about the target itself, not the listing, which skips linked folders; a check
        // that fails shows its error and writes nothing.
        match ws.query_exists(&rel) {
            Ok(false) => return self.write_tab(tab, &rel, window, cx),
            Ok(true) => {}
            Err(e) => return show_error("Cannot save", e.to_string(), window, cx),
        }
        let reply = ask(
            Ask {
                title: "Replace the saved query?".into(),
                detail: Some(format!("{} already exists.", rel.display()).into()),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "overwrite-keep",
                        label: "Keep it".into(),
                        danger: false,
                    },
                    Choice {
                        id: "overwrite-replace",
                        label: "Replace".into(),
                        danger: true,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        let tab = tab.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            if reply.recv().await != Ok(1) {
                return;
            }
            let _ = this.update_in(cx, |shell, window, cx| {
                if let Some(tab) = tab.upgrade() {
                    shell.write_tab(&tab, &rel, window, cx);
                }
            });
        })
        .detach();
    }

    // Writes `tab`'s text to `rel` through core and ties the tab to it; a refusal or I/O
    // error shows "Cannot save" and leaves the tab as it was.
    fn write_tab(
        &mut self,
        tab: &Entity<QueryTab>,
        rel: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let sql = tab.read(cx).sql(cx);
        match ws.write_query(rel, &sql) {
            Ok(q) => {
                // Only one tab is ever bound to a file: another tab on it keeps its text, unbound.
                let path = q.path();
                let unbound: Vec<_> = self
                    .tabs
                    .iter()
                    .filter(|t| *t != tab && t.read(cx).file() == Some(path))
                    .cloned()
                    .collect();
                for other in unbound {
                    other.update(cx, |t, cx| t.unbind_file(cx));
                    self.number_untitled(&other, cx);
                }
                tab.update(cx, |t, cx| t.set_file(path.to_path_buf(), sql, cx));
            }
            Err(e) => show_error("Cannot save", e.to_string(), window, cx),
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Closes tab `ix`, first asking when it has staged edits or edited SQL, since nothing
    /// else keeps them; one question names both losses.
    pub fn close_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let edited = tab.read(cx).edited(cx);
        // Closing a tab answers its open apply prompt with a roll back, so it asks nothing more.
        let staged = if tab.read(cx).apply_dialog_open() {
            0
        } else {
            staged_count(tab, cx)
        };
        let (title, detail) = match (staged, edited) {
            (0, false) => return self.remove_tab(ix, window, cx),
            (0, true) => ("Close this tab?".to_owned(), UNSAVED_SQL.to_owned()),
            (n, false) => (discard_title(n), "Closing the tab drops them.".to_owned()),
            (n, true) => (
                discard_title(n),
                format!("Closing the tab drops them. {UNSAVED_SQL}"),
            ),
        };
        let id = tab.entity_id();
        self.ask_once(
            title,
            detail,
            "Keep",
            window,
            cx,
            move |shell, window, cx| {
                // Other tabs may have closed meanwhile, so find this one again by identity.
                if let Some(ix) = shell.tabs.iter().position(|t| t.entity_id() == id) {
                    shell.remove_tab(ix, window, cx);
                }
            },
        );
    }

    /// Closes every tab except `keep` (all with None), asking once when any closing tab holds
    /// staged edits or unsaved SQL; Keep closes none.
    pub fn close_tabs_except(
        &mut self,
        keep: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closing: Vec<&Entity<QueryTab>> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, _)| Some(*ix) != keep)
            .map(|(_, t)| t)
            .collect();
        // As in `close_tab_at`, a tab whose apply prompt is open rolls back without asking.
        let staged: usize = closing
            .iter()
            .filter(|t| !t.read(cx).apply_dialog_open())
            .map(|t| staged_count(t, cx))
            .sum();
        let edited = closing.iter().filter(|t| t.read(cx).edited(cx)).count();
        let ids: Vec<EntityId> = closing.iter().map(|t| t.entity_id()).collect();
        let keep = keep.and_then(|ix| self.tabs.get(ix)).map(|t| t.entity_id());
        let unsaved = match edited {
            1 => "1 closing tab has SQL not saved anywhere.".to_owned(),
            n => format!("{n} closing tabs have SQL not saved anywhere."),
        };
        let (ask, drops) = match closing.len() {
            1 => ("Close this tab?", "Closing the tab drops them."),
            _ => ("Close these tabs?", "Closing the tabs drops them."),
        };
        let (title, detail) = match (staged, edited) {
            (0, 0) => return self.remove_tabs(&ids, keep, window, cx),
            (0, _) => (ask.to_owned(), unsaved),
            (n, 0) => (discard_title(n), drops.to_owned()),
            (n, _) => (discard_title(n), format!("{drops} {unsaved}")),
        };
        self.ask_once(
            title,
            detail,
            "Keep",
            window,
            cx,
            move |shell, window, cx| shell.remove_tabs(&ids, keep, window, cx),
        );
    }

    // Removes the tabs `ids` still open with nothing active, so no closing tab is activated
    // (and connected) on the way, then activates `keep`, else the first tab left.
    fn remove_tabs(
        &mut self,
        ids: &[EntityId],
        keep: Option<EntityId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active = None;
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == *id) {
                self.remove_tab(ix, window, cx);
            }
        }
        if self.tabs.is_empty() {
            return;
        }
        let next = keep
            .and_then(|k| self.tabs.iter().position(|t| t.entity_id() == k))
            .unwrap_or(0);
        self.activate(next, window, cx);
    }

    /// Asks for a label (`confirm::ask_text`, initial: the current title, empty refused);
    /// nothing on a file tab, whose title stays its file name.
    pub fn rename_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        if tab.read(cx).file().is_some() {
            return;
        }
        let spec = TextAsk {
            title: "Rename tab".into(),
            hint: "Name shown on the tab".into(),
            ok: "Rename".into(),
            initial: tab.read(cx).title(),
            check: Rc::new(|name: &str| name.is_empty().then(|| "Enter a name.".into())),
        };
        let tab = tab.downgrade();
        let reply = ask_text(spec, window, cx);
        cx.spawn_in(window, async move |_, cx| {
            let Ok(name) = reply.recv().await else { return };
            let _ = tab.update(cx, |t, cx| t.set_label(Some(name), cx));
        })
        .detach();
    }

    /// A new tab on tab `ix`'s connection holding its SQL, activated, unsaved, never run.
    pub fn duplicate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let (name, sql) = {
            let t = tab.read(cx);
            (t.model().read(cx).spec().connection.clone(), t.sql(cx))
        };
        // A fresh "Query N" keeps an empty baseline, so the copied SQL reads as unsaved.
        let Some(copy) = self.open_unnumbered(&name, window, cx) else {
            return;
        };
        self.number_untitled(&copy, cx);
        copy.update(cx, |t, cx| t.set_sql(&sql, window, cx));
    }

    /// Activates tab `ix`, then `save_active`.
    pub fn save_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            self.activate(ix, window, cx);
            self.save_active(window, cx);
        }
    }

    // Hands the explorer every tab session's link, merged per connection name, and the
    // connections a tab session reported read-only, which the lock draws like the badge.
    fn sync_links(&self, cx: &mut Context<Self>) {
        let Some(sidebar) = self.sidebar.clone() else {
            return;
        };
        let mut links: HashMap<String, LinkState> = HashMap::new();
        let mut read_only = HashSet::new();
        for tab in &self.tabs {
            let model = tab.read(cx).model().read(cx);
            let link = model.link();
            let entry = links.entry(model.spec().connection.clone()).or_default();
            *entry = std::mem::take(entry).merge(link);
            if model.read_only() == Some(true) {
                read_only.insert(model.spec().connection.clone());
            }
        }
        sidebar.update(cx, |s, cx| {
            s.set_session_links(links, cx);
            s.set_session_read_only(read_only, cx);
        });
    }

    // Drops tab `ix` with its draft and closes its session without waiting. The active tab
    // keeps its place when another closes; closing the active one activates the tab now at
    // its index.
    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(ix);
        self.tab_recency.remove(tab.entity_id());
        // Closing the tab rolls its held transaction back, so its Commit dialog goes too.
        if tab.read(cx).apply_dialog_open() {
            window.close_dialog(cx);
        }
        tab.update(cx, |t, cx| t.discard_draft(cx));
        drop(self.tab_observes.remove(ix));
        let model = tab.read(cx).model().clone();
        let closing = model.update(cx, |m, cx| m.close(cx));
        self.keep_closing(closing.wait(), cx);
        self.sync_links(cx);
        let next = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > ix => Some(a - 1),
            Some(a) if a == ix => Some(ix.min(self.tabs.len() - 1)),
            other => other,
        };
        self.active = None;
        match next {
            Some(next) => self.activate(next, window, cx),
            // The root keeps focus so the window-wide keys still reach the shell.
            None => {
                self.follow_active(window, cx);
                window.focus(&self.focus, cx);
            }
        }
        self.save_session(cx);
        cx.notify();
    }

    // Closes every tab's session without waiting, for a workspace change. Drafts and the
    // saved session stay, so the workspace reopens with these tabs.
    fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        self.tab_observes.clear();
        self.tab_recency = Recent::default();
        self.active = None;
        for tab in std::mem::take(&mut self.tabs) {
            let model = tab.read(cx).model().clone();
            let closing = model.update(cx, |m, cx| m.close(cx));
            self.keep_closing(closing.wait(), cx);
        }
        self.sync_links(cx);
    }

    // Drains a tab's or catalog's closing off the main thread at once, since an undrained one
    // keeps its runner blocked on a full channel, and keeps it for a window close to wait on.
    fn keep_closing(
        &mut self,
        closing: impl Future<Output = ()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.closings
            .retain(|(done, _)| !done.load(Ordering::SeqCst));
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let task = cx.background_spawn(async move {
            closing.await;
            flag.store(true, Ordering::SeqCst);
        });
        self.closings.push((done, task));
    }

    /// How many times the app state file has been written since start.
    pub fn state_writes(&self) -> usize {
        self.state_file.writes()
    }

    /// How many closed tabs' and catalogs' sessions are still closing.
    pub fn pending_closings(&self) -> usize {
        self.closings
            .iter()
            .filter(|(done, _)| !done.load(Ordering::SeqCst))
            .count()
    }

    /// `open_workspace(dir)` once every tab's draft is written, so the tabs come back when
    /// this workspace reopens; first asking, in one question, when a draft cannot be written,
    /// edits are staged or a statement runs. Does nothing while the window is closing, so the
    /// saved session stays as it was.
    pub fn switch_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        let lost = self.flush_drafts(cx);
        let staged = self.staged_total(cx);
        let running = running_titles(self.tabs.iter(), cx);
        if staged == 0 && lost == 0 && running.is_empty() {
            return self.open_workspace(dir, window, cx);
        }
        let (title, mut parts) = match staged {
            0 => ("Close the open tabs?".to_owned(), Vec::new()),
            n => (
                discard_title(n),
                vec!["Switching workspaces drops them.".to_owned()],
            ),
        };
        parts.extend((lost > 0).then(|| lost_detail(lost)));
        parts.extend(running_detail("Switching workspaces", &running));
        self.ask_once(
            title,
            parts.join(" "),
            "Keep",
            window,
            cx,
            move |shell, window, cx| shell.open_workspace(dir, window, cx),
        );
    }

    /// Opens the workspace in `dir` and remembers it for the next start. On failure the
    /// welcome view shows the error and any open workspace is closed.
    pub fn open_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.close_all_tabs(cx);
        self.follow_active(window, cx);
        // The dock names an object of the old workspace, whose sidebar would answer its load.
        self.definition_open = false;
        self.definition_panel.update(cx, |p, cx| p.clear(cx));
        match Workspace::open(&dir) {
            Ok(ws) => {
                let ws = Arc::new(ws);
                let services = self.services.clone();
                let sidebar = cx.new(|cx| Sidebar::new(Arc::clone(&ws), services, window, cx));
                self._sidebar_events = Some(cx.subscribe_in(
                    &sidebar,
                    window,
                    |this, _, event, window, cx| match event {
                        SidebarEvent::Open(ix) => this.click_connection(*ix, window, cx),
                        SidebarEvent::OpenRelation {
                            connection,
                            rel,
                            kind,
                        } => this.open_relation_named(connection, rel, *kind, window, cx),
                        SidebarEvent::Reload => this.reload_workspace(window, cx),
                        SidebarEvent::NewConnection => this.new_connection(window, cx),
                        SidebarEvent::Edit(name) => this.edit_connection(name, window, cx),
                        SidebarEvent::CollapseChanged => this.save_session(cx),
                        SidebarEvent::Move { item, to } => {
                            let edit = Edit::Move {
                                item: item.clone(),
                                to: to.clone(),
                            };
                            this.apply_edit(edit, window, cx);
                        }
                        SidebarEvent::NewFolder { parent } => {
                            this.new_folder(parent.clone(), window, cx)
                        }
                        SidebarEvent::Rename(item) => this.rename_item(item, window, cx),
                        SidebarEvent::Recolor { item, color } => {
                            this.recolor_item(item, *color, window, cx)
                        }
                        SidebarEvent::Delete(item) => this.delete_item(item, window, cx),
                        SidebarEvent::NewQuery { connection, sql } => {
                            this.open_sql(connection, sql, window, cx)
                        }
                        SidebarEvent::OpenIndex { connection, sql } => {
                            let before = this.tabs.len();
                            this.open_sql(connection, sql, window, cx);
                            if let Some(tab) = this.tabs.get(before).cloned() {
                                tab.update(cx, |t, cx| t.run_all(cx));
                            }
                        }
                        SidebarEvent::Notice(notice) => ui::notice(window, cx, notice.clone()),
                        SidebarEvent::Disconnect(name) => this.disconnect(name, window, cx),
                        SidebarEvent::Duplicate(name) => {
                            this.duplicate_connection(name, window, cx)
                        }
                        SidebarEvent::ViewDefinition { connection, object } => {
                            this.view_definition(connection, object.clone(), window, cx)
                        }
                        SidebarEvent::OpenKey {
                            connection,
                            db,
                            key,
                        } => this.open_key(*connection, *db, key.clone(), window, cx),
                        SidebarEvent::Definition {
                            connection,
                            object,
                            result,
                        } => {
                            let result = result.clone();
                            this.definition_panel.update(cx, |p, cx| {
                                p.show(connection, object, result, window, cx);
                            });
                        }
                    },
                ));
                // Restored before `restore_tabs`, whose session save would otherwise drop it.
                let collapsed: Vec<FolderPath> = self
                    .saved
                    .session(ws.dir())
                    .map(|x| x.collapsed.iter().filter_map(|p| p.parse().ok()).collect())
                    .unwrap_or_default();
                sidebar.update(cx, |s, cx| s.set_collapsed(collapsed, cx));
                self._sidebar_notify = Some(cx.observe(&sidebar, |_, _, cx| cx.notify()));
                self.sidebar = Some(sidebar);
                let log = self.history_log.clone();
                let open = self.history_open;
                let panel = cx.new(|cx| {
                    let mut panel = HistoryPanel::new(log, Arc::clone(&ws), window, cx);
                    panel.set_visible(open, cx);
                    panel
                });
                self._history_events = Some(cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event, window, cx| match event {
                        HistoryPanelEvent::Open { connection, sql } => {
                            this.open_history(connection, sql, window, cx)
                        }
                    },
                ));
                self.history_panel = Some(panel);
                // Recent entries name the real folder, so a link or `..` path adds no duplicate.
                let real = ws.dir().to_path_buf();
                self.workspace = Some(ws);
                self.error = None;
                // Forgetting the folder next time is a small loss, so it never blocks opening.
                self.update_saved(
                    move |s| {
                        s.remember_workspace(&real);
                        s.last_workspace = Some(dir.clone());
                    },
                    cx,
                );
                self.restore_tabs(window, cx);
            }
            Err(e) => {
                self.workspace = None;
                self.sidebar = None;
                self._sidebar_events = None;
                self._sidebar_notify = None;
                self.history_panel = None;
                self._history_events = None;
                self.error = Some(e.to_string());
                self.forget_if_saved(&dir, cx);
            }
        }
        self.recent = self.saved.recent_workspaces.clone();
        self.show_approvals(cx);
        cx.notify();
    }

    // Asks before reconnecting `tab` on the current workspace, since its session's open
    // transaction is rolled back; the answer reconnects only a tab still open on a connection
    // still in the workspace.
    fn ask_reconnect(
        &mut self,
        tab: &Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let name = tab.read(cx).session_conn().name().to_owned();
        let reply = ask(
            Ask {
                title: format!("Reconnect {name}?").into(),
                detail: Some("An open transaction will be rolled back.".into()),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "reconnect-cancel",
                        label: "Cancel".into(),
                        danger: false,
                    },
                    Choice {
                        id: "reconnect-ok",
                        label: "Reconnect".into(),
                        danger: true,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        let tab = tab.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            if reply.recv().await != Ok(1) {
                return;
            }
            let _ = this.update(cx, |shell, cx| {
                let (Some(tab), Some(ws)) = (tab.upgrade(), shell.workspace.clone()) else {
                    return;
                };
                if shell.closing || !shell.tabs.contains(&tab) || ws.connection(&name).is_none() {
                    return;
                }
                let model = tab.read(cx).model().clone();
                let closing = model.update(cx, |m, cx| m.reconnect(Arc::clone(&ws), cx));
                shell.keep_closing(closing.wait(), cx);
                tab.update(cx, |t, cx| t.set_workspace(&ws, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Switches `tab` to connection `name` of the current dbento.toml without asking: the old
    /// session closes in the background, the tab and explorer follow and the session is saved.
    /// A refused switch changes nothing.
    pub fn switch_tab_connection(
        &mut self,
        tab: &Entity<QueryTab>,
        name: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let model = tab.read(cx).model().clone();
        let switched = model.update(cx, |m, cx| m.switch_connection(name, Arc::clone(&ws), cx));
        let Ok(closing) = switched else {
            return;
        };
        self.keep_closing(closing.wait(), cx);
        tab.update(cx, |t, cx| t.follow_connection(cx));
        if let Some(ix) = self.active.filter(|&ix| self.tabs.get(ix) == Some(tab)) {
            self.show_in_explorer(ix, cx);
            if let Some(sidebar) = &self.sidebar {
                self.activated_serial = sidebar.read(cx).selection_serial();
            }
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Re-reads dbento.toml; on error shows it (`show_error`) and keeps the current workspace.
    pub fn reload_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.workspace.as_ref().map(|ws| ws.dir().to_path_buf()) else {
            return;
        };
        // A closing window has handed its sessions to the quit wait; nothing new may start.
        if self.closing {
            return;
        }
        match Workspace::open(&dir) {
            Ok(ws) => self.install_workspace(ws, window, cx),
            Err(e) => show_error("Cannot reload dbento.toml", e.to_string(), window, cx),
        }
    }

    /// Makes `ws` current for the explorer, the history panel, new tabs and open tabs; an open
    /// tab shows the new settings but keeps its session until the user reconnects.
    pub fn install_workspace(
        &mut self,
        ws: Workspace,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One shared value, so every view and every later tab agrees on the same workspace.
        let ws = Arc::new(ws);
        self.workspace = Some(Arc::clone(&ws));
        let closings = self
            .sidebar
            .as_ref()
            .map(|s| s.update(cx, |s, cx| s.set_workspace(Arc::clone(&ws), cx)))
            .unwrap_or_default();
        for closing in closings {
            self.keep_closing(closing.wait(), cx);
        }
        if let Some(panel) = &self.history_panel {
            panel.update(cx, |p, cx| p.set_workspace(Arc::clone(&ws), cx));
        }
        for tab in &self.tabs {
            tab.update(cx, |t, cx| t.set_workspace(&ws, cx));
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Plans `edit` on the installed workspace, writes it and installs the result, carrying collapse
    /// through `folder_after`. Returns true when written or when there was nothing to write.
    pub fn apply_edit(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let plan = match self.write_edit(&edit, window, cx) {
            Written::Failed => return false,
            Written::Unchanged => return true,
            Written::Done(plan) => *plan,
        };
        // Remapped first, so installing keeps each collapsed folder under its new path.
        if let Some(sidebar) = &self.sidebar {
            sidebar.update(cx, |s, cx| s.remap_folders(|p| plan.folder_after(p), cx));
        }
        self.install_workspace(plan.into_workspace(), window, cx);
        true
    }

    // Plans `edit` on the installed workspace and writes it, showing any refusal as a dialog;
    // the caller installs the written plan.
    fn write_edit(&mut self, edit: &Edit, window: &mut Window, cx: &mut Context<Self>) -> Written {
        let Some(ws) = self.workspace.clone() else {
            return Written::Failed;
        };
        if self.closing {
            return Written::Failed;
        }
        let plan = match plan_edit(&ws, edit) {
            Ok(plan) => plan,
            Err(e) => {
                show_error("Cannot change dbento.toml", e.to_string(), window, cx);
                return Written::Failed;
            }
        };
        // Writing an unchanged file would only risk a refusal for nothing.
        if plan.new_text() == ws.source_text() {
            return Written::Unchanged;
        }
        if let Err(e) = plan.apply() {
            if matches!(e, WriteError::Changed) {
                self.ask_reload(e.to_string(), window, cx);
            } else {
                show_error("Cannot change dbento.toml", e.to_string(), window, cx);
            }
            return Written::Failed;
        }
        Written::Done(Box::new(plan))
    }

    // A file changed on disk since load: the edit is dropped and Reload installs what is there.
    fn ask_reload(&mut self, detail: String, window: &mut Window, cx: &mut Context<Self>) {
        let reply = ask(
            Ask {
                title: "dbento.toml changed".into(),
                detail: Some(detail.into()),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "edit-error-ok",
                        label: "Close".into(),
                        danger: false,
                    },
                    Choice {
                        id: "edit-reload",
                        label: "Reload workspace".into(),
                        danger: false,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if reply.recv().await == Ok(1) {
                let _ = this.update_in(cx, |shell, window, cx| shell.reload_workspace(window, cx));
            }
        })
        .detach();
    }

    /// Asks for a folder name and declares it under `parent` (None = top level).
    pub fn new_folder(
        &mut self,
        parent: Option<FolderPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let check = {
            let parent = parent.clone();
            move |name: &str| match folder_named(&ws, parent.as_ref(), name, None) {
                Ok(path) => plan_edit(&ws, &Edit::PutFolder { path, color: None })
                    .err()
                    .map(|e| e.to_string().into()),
                Err(says) => Some(says),
            }
        };
        let spec = TextAsk {
            title: "New folder".into(),
            hint: format!(
                "Name, {}",
                parent
                    .as_ref()
                    .map_or("at the top level".to_owned(), |p| format!(
                        "in {}",
                        p.as_str()
                    ))
            )
            .into(),
            ok: "Create".into(),
            initial: String::new(),
            check: Rc::new(check),
        };
        let reply = ask_text(spec, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(name) = reply.recv().await else { return };
            let _ = this.update_in(cx, |shell, window, cx| {
                let Ok(path) = child_text(parent.as_ref(), &name).parse::<FolderPath>() else {
                    return;
                };
                shell.apply_edit(Edit::PutFolder { path, color: None }, window, cx);
            });
        })
        .detach();
    }

    /// Asks for a new name for a folder and renames its subtree.
    pub fn rename_item(
        &mut self,
        item: &WorkspaceItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let WorkspaceItem::Connection(name) = item {
            return self.rename_connection(name, window, cx);
        }
        let (Some(ws), WorkspaceItem::Folder(from)) = (self.workspace.clone(), item) else {
            return;
        };
        let parent = from.parent();
        let check = {
            let (from, parent) = (from.clone(), parent.clone());
            move |name: &str| match folder_named(&ws, parent.as_ref(), name, Some(&from)) {
                Ok(to) => {
                    let edit = Edit::RenameFolder {
                        from: from.clone(),
                        to,
                    };
                    plan_edit(&ws, &edit).err().map(|e| e.to_string().into())
                }
                Err(says) => Some(says),
            }
        };
        let spec = TextAsk {
            title: format!("Rename folder {}", from.name()).into(),
            hint: "New name".into(),
            ok: "Rename".into(),
            initial: from.name().to_owned(),
            check: Rc::new(check),
        };
        let reply = ask_text(spec, window, cx);
        let from = from.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(name) = reply.recv().await else { return };
            let _ = this.update_in(cx, |shell, window, cx| {
                let Ok(to) = child_text(parent.as_ref(), &name).parse::<FolderPath>() else {
                    return;
                };
                shell.apply_edit(Edit::RenameFolder { from, to }, window, cx);
            });
        })
        .detach();
    }

    /// Sets or clears a folder's or connection's own colour.
    pub fn recolor_item(
        &mut self,
        item: &WorkspaceItem,
        color: Option<Color>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let edit = match item {
            WorkspaceItem::Folder(path) => Edit::PutFolder {
                path: path.clone(),
                color,
            },
            WorkspaceItem::Connection(name) => {
                let spec = self
                    .workspace
                    .as_ref()
                    .and_then(|ws| ws.connection_spec(name));
                let Some(mut spec) = spec else { return };
                spec.color = color;
                Edit::PutConnection {
                    previous: Some(name.clone()),
                    name: name.clone(),
                    spec,
                }
            }
            _ => return,
        };
        self.apply_edit(edit, window, cx);
    }

    /// Asks, then deletes a folder (its contents move up to its parent) or a connection.
    pub fn delete_item(
        &mut self,
        item: &WorkspaceItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let WorkspaceItem::Connection(name) = item {
            return self.ask_delete_connection(name.clone(), None, window, cx);
        }
        let WorkspaceItem::Folder(path) = item else {
            return;
        };
        let up = path.parent();
        let detail = format!(
            "Its connections and folders move to {}.",
            up.as_ref().map_or("the top level", |p| p.as_str())
        );
        let reply = ask(
            Ask {
                title: format!("Delete folder {}?", path.name()).into(),
                detail: Some(detail.into()),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "delete-cancel",
                        label: "Cancel".into(),
                        danger: false,
                    },
                    Choice {
                        id: "delete-ok",
                        label: "Delete".into(),
                        danger: true,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        let path = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            if reply.recv().await != Ok(1) {
                return;
            }
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.apply_edit(Edit::DeleteFolder { path }, window, cx);
            });
        })
        .detach();
    }

    // The menu's rename: refused while a tab runs on the connection, and checked by planning
    // the edit, so a bad or taken name stays in the dialog.
    fn rename_connection(&mut self, old: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if self.closing {
            return;
        }
        if self.connection_in_use(old, cx) {
            show_error("Connection in use", IN_USE.to_owned(), window, cx);
            return;
        }
        let renamed = |ws: &Workspace, old: &str, name: &str| {
            ws.connection_spec(old).map(|spec| Edit::PutConnection {
                previous: Some(old.to_owned()),
                name: name.to_owned(),
                spec,
            })
        };
        // The check runs per keystroke, so it reads the spec once rather than reparsing.
        let Some(spec) = ws.connection_spec(old) else {
            return;
        };
        let check = {
            let old = old.to_owned();
            move |name: &str| {
                let edit = Edit::PutConnection {
                    previous: Some(old.clone()),
                    name: name.to_owned(),
                    spec: spec.clone(),
                };
                plan_edit(&ws, &edit).err().map(|e| e.to_string().into())
            }
        };
        let env = secret_env_var(old);
        let hint = if (self.services.env)(&env).is_some() {
            format!("New name. Its password variable {env} becomes DBENTO_SECRET_<NEW NAME>.")
        } else {
            "New name".to_owned()
        };
        let spec = TextAsk {
            title: format!("Rename connection {old}").into(),
            hint: hint.into(),
            ok: "Rename".into(),
            initial: old.to_owned(),
            check: Rc::new(check),
        };
        let reply = ask_text(spec, window, cx);
        let old = old.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(name) = reply.recv().await else { return };
            let _ = this.update_in(cx, |shell, window, cx| {
                // A tab may have opened on it while the dialog was up.
                if shell.connection_in_use(&old, cx) {
                    show_error("Connection in use", IN_USE.to_owned(), window, cx);
                    return;
                }
                let Some(base) = shell.workspace.clone() else {
                    return;
                };
                // Plan from the spec installed now, so a reload while the dialog was up stands.
                let Some(edit) = renamed(&base, &old, &name) else {
                    show_error(
                        "Connection gone",
                        format!("{old} is no longer in dbento.toml"),
                        window,
                        cx,
                    );
                    return;
                };
                if let Written::Done(plan) = shell.write_edit(&edit, window, cx) {
                    shell
                        .finish_connection_write(&base, &old, &name, *plan, None, None, window, cx);
                }
            });
        })
        .detach();
    }

    /// Opens the connection dialog on a new Postgres connection.
    pub fn new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_connection_form(FormStart::New, window, cx);
    }

    /// Opens the connection dialog on `name`'s keys as written; an unknown name does nothing.
    pub fn edit_connection(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = self
            .workspace
            .as_ref()
            .and_then(|ws| ws.connection_spec(name));
        if let Some(spec) = spec {
            self.open_connection_form(FormStart::Edit(name.to_owned(), spec), window, cx);
        }
    }

    /// Opens the connection dialog as a new connection copied from `name`, named "<name>-copy"
    /// (or the first free "<name>-copy-N"); no secret is read or copied.
    pub fn duplicate_connection(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let Some(spec) = ws.connection_spec(name) else {
            return;
        };
        let (mut copy, mut n) = (format!("{name}-copy"), 2);
        while ws.declared(&copy).is_some() {
            copy = format!("{name}-copy-{n}");
            n += 1;
        }
        self.open_connection_form(FormStart::Copy(copy, spec), window, cx);
    }

    /// Asks once when a tab on `conn` holds staged edits or runs a statement, then disconnects
    /// its tabs and catalog. Close drops the staged edits and cancels the statements; the tabs
    /// stay open and reconnect on their next run.
    pub fn disconnect(&mut self, conn: &str, window: &mut Window, cx: &mut Context<Self>) {
        let on_conn: Vec<&Entity<QueryTab>> = self
            .tabs
            .iter()
            .filter(|t| t.read(cx).model().read(cx).spec().connection == conn)
            .collect();
        let staged: usize = on_conn.iter().map(|t| staged_count(t, cx)).sum();
        let running = running_titles(on_conn.into_iter(), cx);
        if staged == 0 && running.is_empty() {
            return self.disconnect_now(conn, cx);
        }
        let (title, mut parts) = match staged {
            0 => (format!("Disconnect {conn}?"), Vec::new()),
            n => (
                discard_title(n),
                vec!["Disconnecting drops them.".to_owned()],
            ),
        };
        parts.extend(running_detail("Disconnecting", &running));
        let conn = conn.to_owned();
        self.ask_once(
            title,
            parts.join(" "),
            "Keep",
            window,
            cx,
            move |shell, _, cx| shell.disconnect_now(&conn, cx),
        );
    }

    // Retires every runner on `conn` and its catalog session, draining each closing like a closed
    // tab's, then reverts staged edits: disconnecting ends a save in flight, so the revert takes.
    fn disconnect_now(&mut self, conn: &str, cx: &mut Context<Self>) {
        let models: Vec<_> = self
            .tabs
            .iter()
            .map(|t| t.read(cx).model().clone())
            .filter(|m| m.read(cx).spec().connection == conn)
            .collect();
        for model in models {
            let closing = model.update(cx, |m, cx| {
                let closing = m.disconnect(cx);
                m.revert_edits(cx);
                closing
            });
            self.keep_closing(closing.wait(), cx);
        }
        if let Some(sidebar) = self.sidebar.clone()
            && let Some(closing) = sidebar.update(cx, |s, cx| {
                let closing = s.disconnect(conn);
                cx.notify();
                closing
            })
        {
            self.keep_closing(closing.wait(), cx);
        }
        self.sync_links(cx);
        cx.notify();
    }

    // One dialog at a time, and none while the window closes. The form lives as the dialog's
    // child; Escape closes it like Cancel, while a stray click outside keeps the typed fields.
    fn open_connection_form(
        &mut self,
        start: FormStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if self.closing || self.connection_form.is_some() {
            return;
        }
        let title = match start {
            FormStart::Edit(..) => "Edit connection",
            FormStart::New | FormStart::Copy(..) => "New connection",
        };
        let folders = ws.folders().iter().map(|f| f.path().clone()).collect();
        let dir = ws.dir().to_path_buf();
        let secrets_in_memory = self.services.secrets_in_memory;
        let env_fallback = match &start {
            FormStart::Edit(name, _) => (self.services.env)(&secret_env_var(name)).is_some(),
            FormStart::New | FormStart::Copy(..) => false,
        };
        let form = cx.new(|cx| {
            let form = match start {
                FormStart::New => ConnectionForm::new(None, folders, secrets_in_memory, window, cx),
                FormStart::Edit(name, spec) => {
                    ConnectionForm::new(Some((name, spec)), folders, secrets_in_memory, window, cx)
                }
                FormStart::Copy(name, spec) => {
                    ConnectionForm::copy_of(&name, spec, folders, secrets_in_memory, window, cx)
                }
            };
            form.with_base_dir(dir).with_env_fallback(env_fallback)
        });
        let events = cx.subscribe_in(&form, window, |this, form, event, window, cx| match event {
            ConnectionFormEvent::Save => this.save_connection(window, cx),
            ConnectionFormEvent::Cancel => this.close_connection_form(window, cx),
            ConnectionFormEvent::Reload => this.rebase_connection_form(form, window, cx),
            ConnectionFormEvent::Test => this.test_connection(form, window, cx),
            ConnectionFormEvent::Delete => {
                if let Some(name) = form.read(cx).previous().map(str::to_owned) {
                    this.ask_delete_connection(name, Some(form), window, cx);
                }
            }
            // Dropping the task aborts the attempt made with the old values.
            ConnectionFormEvent::Edited => this.connection_test = None,
        });
        self.connection_form = Some((form.clone(), events));
        self.form_base = Some(Arc::clone(&ws));
        let shell = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let first = form.focus_handle(cx);
            window.open_dialog(cx, move |dialog, window, cx| {
                let shell = shell.clone();
                let muted = cx.theme().muted_foreground;
                confirm::frame(dialog, px(560.), None, cx)
                    .margin_top((window.viewport_size().height / 10.).max(px(64.)))
                    .overlay_closable(false)
                    .on_close(move |_, window, cx| {
                        let _ = shell.update(cx, |s, cx| s.drop_connection_form(window, cx));
                    })
                    // Built as the kit's unscrolled content, so the height cap reaches the
                    // form and only its body scrolls between a fixed title and footer.
                    .content({
                        let form = form.clone();
                        move |content, _, cx| {
                            content.min_h_0().child(
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .child(confirm::title_row(
                                        Some((Icons::Database, muted)),
                                        title,
                                        cx,
                                    ))
                                    .child(form.clone()),
                            )
                        }
                    })
            });
            // The kit focuses its own host on open; moving focus into the form keeps
            // Escape and Enter on the dialog and lets Ctrl+S reach SaveConnection.
            first.focus(window, cx);
        });
    }

    /// Opens the Settings dialog; does nothing while it or any other dialog is open.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing || self.settings_view.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let view = cx.new(|cx| SettingsView::new(window, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            SettingsViewEvent::Close => {
                if this.settings_view.take().is_some() {
                    window.close_dialog(cx);
                }
            }
        });
        self.settings_view = Some((view.clone(), events));
        let shell = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            // Another dialog may have opened since the call; it keeps the window.
            if window.has_active_dialog(cx) {
                let _ = shell.update(cx, |s, _| s.settings_view = None);
                return;
            }
            let focus = view.focus_handle(cx);
            window.open_dialog(cx, move |dialog, window, cx| {
                let shell = shell.clone();
                let muted = cx.theme().muted_foreground;
                confirm::frame(dialog, px(560.), None, cx)
                    .margin_top((window.viewport_size().height / 10.).max(px(64.)))
                    .on_close(move |_, _, cx| {
                        let _ = shell.update(cx, |s, _| s.settings_view = None);
                    })
                    .content({
                        let view = view.clone();
                        move |content, _, cx| {
                            content.min_h_0().child(
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .child(confirm::title_row(
                                        Some((Icons::Settings, muted)),
                                        "Settings",
                                        cx,
                                    ))
                                    .child(view.clone()),
                            )
                        }
                    })
            });
            // Focus inside the view so Escape reaches the dialog and the picker's own handler.
            focus.focus(window, cx);
        });
    }

    // Reloads, then moves the form onto the new file so Save diffs against what is there now.
    // A failed reload keeps the form and its error; a vanished connection says so inline.
    fn rebase_connection_form(
        &mut self,
        form: &Entity<ConnectionForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.workspace.clone();
        self.reload_workspace(window, cx);
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if before.is_some_and(|b| Arc::ptr_eq(&b, &ws)) {
            return;
        }
        // A Test of the old form must not answer for the rebased one.
        self.connection_test = None;
        self.form_base = Some(Arc::clone(&ws));
        form.update(cx, |f, cx| f.set_test(TestState::Idle, cx));
        let previous = form.read(cx).previous().map(str::to_owned);
        let loaded = previous.as_deref().and_then(|n| ws.connection_spec(n));
        let error = match (&previous, &loaded) {
            (Some(name), None) => Some(format!("`{name}` is no longer in dbento.toml")),
            _ => None,
        };
        let folders = ws.folders().iter().map(|f| f.path().clone()).collect();
        form.update(cx, |f, cx| {
            f.rebase(loaded, folders, window, cx);
            f.set_error(error, false, cx);
        });
    }

    fn close_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_connection_form(window, cx) {
            window.close_dialog(cx);
        }
    }

    // Lets go of the form, its typed password and any running test; true when one was open.
    fn drop_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.connection_test = None;
        self.form_base = None;
        let Some((form, _)) = self.connection_form.take() else {
            return false;
        };
        for field in [Field::Password, Field::KeyPassphrase] {
            let typed = form.read(cx).input(field).clone();
            typed.update(cx, |s, cx| s.set_value("", window, cx));
        }
        true
    }

    // Plans the edit without writing it and opens a session on the result with the typed
    // password, or the stored secret when the field is empty. It runs nothing and closes the
    // session; the keychain read and the connect both stay off the UI thread.
    fn test_connection(
        &mut self,
        form: &Entity<ConnectionForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        if self.closing {
            return;
        }
        let f = form.read(cx);
        let typed = f.password(cx);
        let typed_passphrase = f.key_passphrase(cx);
        let previous = f.previous().map(str::to_owned);
        let planned = f.edit(cx).and_then(|(name, mut spec)| {
            // Postgres needs a database to connect to, but the database is what a Test lists:
            // with Database left empty, the test opens the server's own `postgres` instead, so
            // Host, User and Password are enough to pick from the list. Save still needs one.
            if spec.engine == Engine::Postgres
                && spec.database.as_deref().is_none_or(|d| d.trim().is_empty())
            {
                spec.database = Some(TEST_DATABASE_POSTGRES.to_owned());
            }
            let edit = Edit::PutConnection {
                previous: f.previous().map(str::to_owned),
                name: name.clone(),
                spec,
            };
            plan_edit(&ws, &edit)
                .map(|plan| (name, plan))
                .map_err(|e| e.to_string())
        });
        let (name, plan) = match planned {
            Ok(planned) => planned,
            Err(says) => {
                form.update(cx, |f, cx| f.set_test(TestState::Failed(says), cx));
                return;
            }
        };
        // A rename is not applied yet, so stored secrets are still under the old name.
        let renamed = previous.filter(|p| *p != name);
        form.update(cx, |f, cx| f.set_test(TestState::Testing, cx));
        let (store, env) = (
            Arc::clone(&self.services.store),
            Arc::clone(&self.services.env),
        );
        let attempt = crate::tokio_bridge::spawn(cx, async move {
            let plan = Arc::new(plan);
            let key_passphrase = match typed_passphrase {
                Some(typed) => Some(typed),
                None => {
                    let (plan, name) = (Arc::clone(&plan), name.clone());
                    let (store, env, renamed) =
                        (Arc::clone(&store), Arc::clone(&env), renamed.clone());
                    tokio::task::spawn_blocking(move || {
                        let ws = plan.workspace();
                        let conn = ws.connection(&name).ok_or_else(|| missing(&name))?;
                        let found = resolve_key_passphrase(ws, &conn, &*store, &*env)
                            .map_err(|e| e.to_string())?;
                        // Only a key that needs a passphrase reads the old name's slot, so a
                        // plain key still never touches the keychain.
                        match (found, renamed) {
                            (None, Some(old)) if tls_key_is_encrypted(&conn) => {
                                match store.get(&SecretKey::key_passphrase(ws.dir(), old)) {
                                    Ok(found) => Ok(found),
                                    Err(SecretError::Unavailable { .. }) => Ok(None),
                                    Err(e) => Err(e.to_string()),
                                }
                            }
                            (found, _) => Ok(found),
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())??
                }
            };
            let secret = match typed {
                Some(typed) => Some(typed),
                None => {
                    let (plan, name) = (Arc::clone(&plan), name.clone());
                    tokio::task::spawn_blocking(move || {
                        let ws = plan.workspace();
                        let conn = ws.connection(&name).ok_or_else(|| missing(&name))?;
                        let found =
                            resolve_secret(ws, &conn, &*store, &*env).map_err(|e| e.to_string())?;
                        // Only a keychain connection reads the old name's secret.
                        match (found, renamed) {
                            (None, Some(old)) if keychain_backed(&conn) => store
                                .get(&SecretKey::new(ws.dir(), old))
                                .map_err(|e| e.to_string()),
                            (found, _) => Ok(found),
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())??
                }
            };
            let ws = plan.workspace();
            let conn = ws.connection(&name).ok_or_else(|| missing(&name))?;
            let mut options = SessionOptions::default();
            options.key_passphrase = key_passphrase;
            let mut session = connect(ws, &conn, secret, options)
                .await
                .map_err(|e| e.to_string())?;
            // The server's databases fill Database's suggestions. The session connected, so a
            // list the engine lacks, the login may not read or the server stalls on leaves the
            // test passed.
            let Some(databases) = listed_within(TEST_DATABASES_CAP, session.databases()).await
            else {
                crate::catalog::abandon(session);
                return Ok(Vec::new());
            };
            // A connection test runs nothing that writes, so close has nothing to report.
            let _ = session.close().await;
            Ok::<Vec<String>, String>(databases)
        });
        let form = form.downgrade();
        self.connection_test = Some(cx.spawn_in(window, async move |_, cx| {
            let (state, databases) = match attempt.await {
                Ok(Ok(databases)) => (TestState::Connected, databases),
                Ok(Err(says)) => (TestState::Failed(says), Vec::new()),
                Err(e) => (TestState::Failed(e.to_string()), Vec::new()),
            };
            // Only the attempt the form still waits on may answer.
            let _ = form.update(cx, |f, cx| {
                if *f.test_state() == TestState::Testing {
                    f.set_test(state, cx);
                    f.set_databases(databases, cx);
                }
            });
        }));
    }

    // The typed password and where it goes, when the saved connection reads it from the
    // keychain; any other source, or an empty field, keeps the store as it is.
    fn password_to_store(
        form: &Entity<ConnectionForm>,
        plan: &EditPlan,
        name: &str,
        cx: &Context<Self>,
    ) -> Option<(SecretKey, Secret)> {
        let ws = plan.workspace();
        if !keychain_backed(ws.declared(name)?) {
            return None;
        }
        let typed = form.read(cx).password(cx)?;
        Some((ws.secret_target(name)?, typed))
    }

    // Stores the password off the UI thread (a keychain call can take seconds); a failure
    // shows once the dialog is gone, since the file is already written.
    fn store_password(
        &mut self,
        key: SecretKey,
        secret: Secret,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = Arc::clone(&self.services.store);
        let stored = cx.background_spawn(async move { store.set(&key, &secret) });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = stored.await {
                let _ = cx.update(|window, cx| {
                    show_error("Password not saved", e.to_string(), window, cx)
                });
            }
        })
        .detach();
    }

    // True when an open tab runs on `name`; such a connection cannot be renamed or deleted.
    fn connection_in_use(&self, name: &str, cx: &Context<Self>) -> bool {
        self.tabs
            .iter()
            .any(|t| runs_on(&t.read(cx).model().read(cx).spec().connection, name))
    }

    // Reads the form, plans the edit and writes it. Every failure before the file is replaced
    // stays inline in the dialog, and a file changed since load offers Reload; success
    // installs the validated workspace without reading the file again.
    fn save_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(ws)) = (self.connection_form().cloned(), self.workspace.clone())
        else {
            return;
        };
        if self.closing {
            return;
        }
        // A workspace installed behind the form's back (a reload elsewhere) makes its loaded
        // values stale, as a hand edit does.
        if !self.form_base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &ws)) {
            let says = WriteError::Changed.to_string();
            form.update(cx, |f, cx| f.set_error(Some(says), true, cx));
            return;
        }
        let previous = form.read(cx).previous().map(str::to_owned);
        let written = form.read(cx).edit(cx).map_err(|says| (says, false));
        let written = written.and_then(|(name, spec)| {
            // Checked before planning, so a refused rename never touches disk.
            if let Some(old) = previous.as_deref().filter(|old| *old != name)
                && self.connection_in_use(old, cx)
            {
                return Err((IN_USE.to_owned(), false));
            }
            let edit = Edit::PutConnection {
                previous: previous.clone(),
                name: name.clone(),
                spec,
            };
            plan_and_write(&ws, &edit).map(|plan| (name, plan))
        });
        match written {
            Err((says, stale)) => form.update(cx, |f, cx| f.set_error(Some(says), stale, cx)),
            Ok((name, plan)) => {
                // Read before the form closes, which clears the password field.
                let password = Self::password_to_store(&form, &plan, &name, cx);
                let key_passphrase = form.read(cx).key_passphrase(cx);
                let old_name = previous.clone().unwrap_or_else(|| name.clone());
                self.finish_connection_write(
                    &ws,
                    &old_name,
                    &name,
                    plan,
                    password,
                    key_passphrase,
                    window,
                    cx,
                );
            }
        }
    }

    // After a connection edit from `base` is written: a rename moves `queries/<old>/` before
    // install, any open form closes, and the keychain follows off the UI thread (moved,
    // removed, or `password` stored). A failed folder move keeps the rename and says so.
    #[allow(clippy::too_many_arguments)]
    fn finish_connection_write(
        &mut self,
        base: &Workspace,
        old_name: &str,
        name: &str,
        plan: EditPlan,
        password: Option<(SecretKey, Secret)>,
        key_passphrase: Option<Secret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reads_key =
            |ws: &Workspace, name: &str| ws.declared(name).is_some_and(|c| c.tls_key().is_some());
        let key_before = reads_key(base, old_name);
        let key_after = reads_key(plan.workspace(), name);
        let renamed = plan
            .renamed_connection()
            .map(|(old, new)| (old.to_owned(), new.to_owned()));
        let folder = renamed.as_ref().filter(|_| plan.move_queries().is_err());
        let folder =
            folder.map(|(old, new)| format!("Move queries/{old} to queries/{new} by hand."));
        let old_keychain = base.declared(old_name).is_some_and(keychain_backed);
        let new_keychain = plan.workspace().declared(name).is_some_and(keychain_backed);
        let new_key = plan.workspace().secret_target(name);
        // A new connection has no old passphrase slot, so its key comes from the written plan.
        let passphrase_from = base
            .key_passphrase_target(old_name)
            .or_else(|| plan.workspace().key_passphrase_target(name));
        self.install_workspace(plan.into_workspace(), window, cx);
        self.close_connection_form(window, cx);
        let old_key = base.secret_target(old_name).filter(|_| old_keychain);
        match (old_key, new_keychain, renamed.and(new_key)) {
            // The old entry has no reader any more.
            (Some(old_key), false, _) => self.remove_secret(old_key, window, cx),
            (Some(old_key), true, Some(new_key)) => {
                self.move_password(old_key, new_key, password, window, cx)
            }
            _ => {
                if let Some((key, secret)) = password {
                    self.store_password(key, secret, window, cx);
                }
            }
        }
        if let Some(from) = passphrase_from {
            self.follow_key_passphrase(
                from,
                name,
                (key_before, key_after),
                key_passphrase,
                window,
                cx,
            );
        }
        if let Some(detail) = folder {
            show_error("Saved queries not moved", detail, window, cx);
        }
    }

    // The client key's passphrase follows its connection as the password does, off the UI
    // thread: dropped once no `tls_key` reads it, moved on a rename, and replaced by a typed
    // one when the saved connection has a key.
    fn follow_key_passphrase(
        &mut self,
        from: SecretKey,
        name: &str,
        (before, after): (bool, bool),
        typed: Option<Secret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let to = from.renamed(name);
        let typed = typed.filter(|_| after);
        let stays = before && after && from == to;
        if typed.is_none() && (stays || !before) {
            return;
        }
        let store = Arc::clone(&self.services.store);
        let done = cx.background_spawn(async move {
            match (before, after) {
                (true, false) => store.delete(&from)?,
                (true, true) => {
                    move_secret(&*store, &from, &to)?;
                }
                _ => {}
            }
            match typed {
                Some(secret) => store.set(&to, &secret),
                None => Ok(()),
            }
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = done.await {
                let _ = cx.update(|window, cx| {
                    show_error("Key passphrase not saved", e.to_string(), window, cx)
                });
            }
        })
        .detach();
    }

    // After a rename is written: moves the stored secret to the new name, then stores the
    // typed password over it, both off the UI thread. A failure keeps the rename and asks
    // for the password again.
    fn move_password(
        &mut self,
        from: SecretKey,
        to: SecretKey,
        password: Option<(SecretKey, Secret)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = Arc::clone(&self.services.store);
        let new = to.connection().to_owned();
        let moved = cx.background_spawn(async move {
            move_secret(&*store, &from, &to)?;
            match password {
                Some((key, secret)) => store.set(&key, &secret),
                None => Ok(()),
            }
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = moved.await {
                let detail = format!("{e}. Enter the password again for {new}.");
                let _ =
                    cx.update(|window, cx| show_error("Password not moved", detail, window, cx));
            }
        })
        .detach();
    }

    // Delete asks once, and is refused while a tab runs on the connection. The refusal goes
    // to `form` when the dialog asked, else to an error dialog.
    fn ask_delete_connection(
        &mut self,
        name: String,
        form: Option<&Entity<ConnectionForm>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        if self.connection_in_use(&name, cx) {
            Self::refuse_in_use(form, window, cx);
            return;
        }
        let reply = ask(
            Ask {
                title: format!("Delete {name}?").into(),
                detail: Some(format!("Its saved queries stay in queries/{name}/.").into()),
                sql: None,
                language: "sql",
                choices: vec![
                    Choice {
                        id: "delete-cancel",
                        label: "Cancel".into(),
                        danger: false,
                    },
                    Choice {
                        id: "delete-ok",
                        label: "Delete".into(),
                        danger: true,
                    },
                ],
                default: 0,
                cancel: 0,
            },
            window,
            cx,
        );
        let form = form.map(|f| f.downgrade());
        cx.spawn_in(window, async move |this, cx| {
            if reply.recv().await != Ok(1) {
                return;
            }
            let _ = this.update_in(cx, |shell, window, cx| match form {
                Some(form) => {
                    if let Some(form) = form.upgrade() {
                        shell.delete_connection(Some(&form), &name, window, cx);
                    }
                }
                None => shell.delete_connection(None, &name, window, cx),
            });
        })
        .detach();
    }

    fn refuse_in_use(
        form: Option<&Entity<ConnectionForm>>,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        match form {
            Some(form) => form.update(cx, |f, cx| f.set_error(Some(IN_USE.to_owned()), false, cx)),
            None => show_error("Connection in use", IN_USE.to_owned(), window, cx),
        }
    }

    // Removes the entry from dbento.toml, then the stored secret off the UI thread; the
    // secret goes only once the file no longer names the connection.
    // Without a form, refusals show as `write_edit`'s dialogs.
    fn delete_connection(
        &mut self,
        form: Option<&Entity<ConnectionForm>>,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        // The answer may arrive after the dialog closed or a tab opened on the connection.
        let current = form.is_none_or(|form| self.connection_form().is_some_and(|f| f == form));
        if self.closing || !current {
            return;
        }
        if self.connection_in_use(name, cx) {
            Self::refuse_in_use(form, window, cx);
            return;
        }
        let edit = Edit::DeleteConnection {
            name: name.to_owned(),
        };
        let plan = match form {
            None => match self.write_edit(&edit, window, cx) {
                Written::Done(plan) => *plan,
                Written::Failed | Written::Unchanged => return,
            },
            Some(form) => match plan_and_write(&ws, &edit) {
                Ok(plan) => plan,
                Err((says, stale)) => {
                    form.update(cx, |f, cx| f.set_error(Some(says), stale, cx));
                    return;
                }
            },
        };
        let had_keychain = ws.declared(name).is_some_and(keychain_backed);
        let had_key = ws.declared(name).is_some_and(|c| c.tls_key().is_some());
        self.install_workspace(plan.into_workspace(), window, cx);
        self.close_connection_form(window, cx);
        if let Some(key) = ws.secret_target(name).filter(|_| had_keychain) {
            self.remove_secret(key, window, cx);
        }
        if let Some(key) = ws.key_passphrase_target(name).filter(|_| had_key) {
            self.remove_secret(key, window, cx);
        }
    }

    // Deletes a stored secret no connection reads any more, off the UI thread.
    fn remove_secret(&mut self, key: SecretKey, window: &mut Window, cx: &mut Context<Self>) {
        let store = Arc::clone(&self.services.store);
        let removed = cx.background_spawn(async move { store.delete(&key) });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = removed.await {
                let _ = cx.update(|window, cx| {
                    show_error("Password not removed", e.to_string(), window, cx)
                });
            }
        })
        .detach();
    }

    // Reopens the workspace's saved tabs and its unclaimed drafts without running anything;
    // only the activated tab connects. A saved query that no longer reads leaves its tab
    // unbound, keeping the draft text.
    fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let drafts = Drafts::new(self.services.paths.drafts.clone()).load(ws.dir());
        // A child is not declared, so every saved or drafted name that still resolves joins
        // the declared ones; a child the parent no longer lists stays out.
        let session = self.saved.session(ws.dir());
        let mut owned: Vec<String> = ws
            .connections()
            .iter()
            .map(|c| c.name().to_owned())
            .collect();
        let used = session
            .map_or(&[][..], |s| &s.tabs[..])
            .iter()
            .map(|t| t.connection.as_str())
            .chain(drafts.iter().map(|d| d.connection.as_str()));
        for name in used {
            if !owned.iter().any(|n| n == name) && ws.connection(name).is_some() {
                owned.push(name.to_owned());
            }
        }
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        let (plan, active) = plan_restore(session, drafts, &names);
        // The folder is listed only when some tab edits a saved query.
        let queries = if plan.iter().any(|t| t.file.is_some()) {
            ws.saved_queries().unwrap_or_default()
        } else {
            Vec::new()
        };
        self.restoring = true;
        self.next_query = 1;
        // A tab the closing window refuses must not drop out of the saved session.
        let mut refused = false;
        for restored in plan {
            let Some(tab) = self.push_tab(&restored.connection, restored.draft, window, cx) else {
                refused = true;
                continue;
            };
            let file = restored.file.and_then(|rel| {
                let q = queries.iter().find(|q| q.path() == rel)?;
                Some((ws.read_query(q).ok()?, rel))
            });
            tab.update(cx, |t, cx| {
                if restored.label.is_some() {
                    t.set_label(restored.label, cx);
                }
                let bound = file.is_some();
                if let Some((text, rel)) = file {
                    t.set_sql(&text, window, cx);
                    t.set_file(rel, text, cx);
                }
                if let Some(table) = &restored.table {
                    restore_table(t, table, !bound && restored.sql.is_none(), window, cx);
                }
                if let Some(sql) = &restored.sql {
                    t.set_sql(sql, window, cx);
                    // A browsing tab's draft holds its browse SQL, which counts as saved.
                    if restored.table.as_ref().is_some_and(|t| t.browse.is_some()) {
                        t.mark_saved(cx);
                    }
                }
            });
            self.number_untitled(&tab, cx);
        }
        if let Some(active) = active {
            self.activate(active, window, cx);
        }
        // Only the activated tab connects here, which keeps a large session within the cap.
        debug_assert!(
            self.tabs
                .iter()
                .filter(|t| t.read(cx).model().read(cx).events().is_some())
                .count()
                <= PRECONNECT_CAP,
            "a restored session connects at most PRECONNECT_CAP tabs"
        );
        self.restoring = false;
        if !refused {
            self.save_session(cx);
        }
    }

    // A folder that no longer opens leaves the recent list and, when it was the saved one,
    // the next start, so its error does not greet every later start; a bad argument leaves
    // the saved folder alone.
    fn forget_if_saved(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let saved = &self.saved;
        let known = saved.last_workspace.as_deref() == Some(dir)
            || saved.recent_workspaces.iter().any(|p| p == dir);
        if !known {
            return;
        }
        let dir = dir.to_path_buf();
        self.update_saved(
            move |s| {
                s.forget_workspace(&dir);
                if s.last_workspace.as_deref() == Some(dir.as_path()) {
                    s.last_workspace = None;
                }
            },
            cx,
        );
    }

    // Asks for one folder and opens it; cancelling the prompt changes nothing.
    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reply = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open workspace".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = reply.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.switch_workspace(dir, window, cx)
            });
        })
        .detach();
    }

    // The strip, Zed-style: the active tab sits on the editor surface and joins it, inactive
    // tabs sit on the chrome behind the hairline. Each tagged tab wears its connection's dot
    // and top line, and its close button shows on hover, always on the active tab.
    fn tab_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let area = panel_box(cx)
            .id("tab-area")
            .test_support()
            .flex()
            .flex_col();
        let Some(tab) = self.tab().cloned() else {
            return area
                .items_center()
                .justify_center()
                .child(self.start_card(cx));
        };
        let theme = cx.theme();
        let bar = theme.tab_bar;
        let strip = div()
            .id("tab-bar")
            .test_support()
            .flex()
            .flex_none()
            .h(TAB_STRIP)
            .bg(bar)
            .overflow_hidden()
            // One menu on the strip, as in the explorer: capture clears the target, a handle's
            // bubble handler sets it, and the deferred builder reads it.
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.tab_menu_target = None))
            .children({
                let titles: Vec<(String, String)> = self
                    .tabs
                    .iter()
                    .map(|t| {
                        let t = t.read(cx);
                        (t.title(), t.conn().name().to_owned())
                    })
                    .collect();
                let pairs: Vec<(&str, &str)> = titles
                    .iter()
                    .map(|(t, c)| (t.as_str(), c.as_str()))
                    .collect();
                tab_labels(&pairs)
                    .into_iter()
                    .enumerate()
                    .map(|(ix, label)| self.tab_handle(ix, label, cx))
                    .collect::<Vec<_>>()
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .px_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("tab-new")
                            .ghost()
                            .compact()
                            .icon(Icons::Plus)
                            .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx))),
                    ),
            )
            .child(div().flex_1().border_b_1().border_color(cx.theme().border))
            .context_menu({
                let weak = cx.entity().downgrade();
                move |menu, _, cx| {
                    // As in the explorer, the menu counts as open from its build until dismissed.
                    let _ = weak.update(cx, |s, cx| {
                        s.tab_menu_open = true;
                        cx.notify();
                    });
                    let closed = weak.clone();
                    cx.subscribe_self(move |_, _: &DismissEvent, cx| {
                        let _ = closed.update(cx, |s, cx| {
                            s.tab_menu_open = false;
                            cx.notify();
                        });
                    })
                    .detach();
                    tab_menu(menu, &weak, cx)
                }
            });
        area.child(strip).child(div().flex_1().min_h_0().child(tab))
    }

    // One tab handle: `("tab", ix)` labelled with its strip label `title`, holding the kind glyph
    // `("tab-connection", ix)` named after the connection, the name (truncated), the tag dot
    // and `("tab-close", ix)`. Only the active tab's top line is `tab-accent`. A clash suffix is
    // its own `("tab-suffix", ix)` so the title truncates and the connection stays readable.
    fn tab_handle(
        &self,
        ix: usize,
        title: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let border = theme.border;
        let active = self.active == Some(ix);
        let (surface, fg) = if active {
            (theme.tab_active, theme.tab_active_foreground)
        } else {
            (theme.tab_bar, theme.tab_foreground)
        };
        let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
        let (wash, bar) = (theme.drop_target, theme.drag_border);
        let tab = self.tabs[ix].read(cx);
        let conn = SharedString::from(tab.conn().name().to_owned());
        let glyph = if tab.origin().is_some() {
            Icons::Table2
        } else {
            Icons::FileCode
        };
        let kind = div()
            .id(("tab-connection", ix))
            .aria_label(conn.clone())
            .test_support()
            .flex()
            .flex_none()
            .child(ui::icon(glyph, px(14.), muted));
        // A running tab spins in the strip, so a run behind another tab still shows.
        let running = tab.model().read(cx).running().then(|| {
            div()
                .id(("tab-running", ix))
                .aria_label("Running")
                .test_support()
                .flex()
                .flex_none()
                .child(
                    Spinner::new()
                        .with_size(gpui_kit::component::Size::Size(px(12.)))
                        .color(muted),
                )
        });
        let color = tab.conn().effective_color();
        let dot = color.map(|color| {
            div()
                .id(("tab-color", ix))
                .aria_label(color_name(color))
                .test_support()
                .size(px(8.))
                .flex_none()
                .rounded_full()
                .bg(tag_color(color))
        });
        // A tagged tab carries a top line; only the active tab's is `tab-accent`.
        let strength = tab_line_strength(active, tab.conn().is_production());
        let line = color.map(|color| {
            let id: ElementId = if active {
                "tab-accent".into()
            } else {
                ("tab-line", ix).into()
            };
            div()
                .id(id)
                .aria_label(color_name(color))
                .test_support()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(2.))
                .bg(tag_color(color).alpha(strength))
        });
        let suffix = format!(" \u{B7} {conn}");
        let name = match title.strip_suffix(suffix.as_str()) {
            Some(head) => div()
                .flex()
                .flex_1()
                .min_w_0()
                .child(div().min_w_0().truncate().child(head.to_owned()))
                .child(
                    div()
                        .id(("tab-suffix", ix))
                        .aria_label(conn.clone())
                        .test_support()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(suffix),
                ),
            None => div().flex_1().min_w_0().truncate().child(title.clone()),
        };
        // A preview's name is italic and marked, as the follow that opened it may replace it.
        let name = if tab.is_preview() {
            div()
                .id(("tab-preview", ix))
                .aria_label("Preview")
                .test_support()
                .flex()
                .flex_1()
                .min_w_0()
                .italic()
                .child(name)
                .into_any_element()
        } else {
            name.into_any_element()
        };
        let group = SharedString::from(format!("tab-{ix}"));
        // Transparent rather than hidden while idle, so the button keeps its place and hitbox.
        let close = div()
            .id(("tab-close", ix))
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(px(3.))
            .text_color(if active { muted } else { transparent_black() })
            .group_hover(group.clone(), |s| s.text_color(muted))
            .hover(|s| s.bg(hover).text_color(fg))
            .child(Icon::new(Icons::X).size_3())
            .tooltip(|window, cx| {
                theme::tooltip_keys("Close tab", keys_for(&CloseTab, cx), window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close_tab_at(ix, window, cx);
            }));
        let handle = div()
            .id(("tab", ix))
            .group(group)
            .aria_label(title.clone())
            .aria_selected(active)
            .test_support()
            .relative()
            .flex()
            .flex_shrink(1.)
            .min_w(px(72.))
            .max_w(px(280.))
            .items_center()
            .gap_2()
            .pl_3()
            .pr_1()
            .bg(surface)
            .text_color(fg)
            .border_r_1()
            .border_color(border)
            .text_sm()
            .cursor_pointer()
            .children(line)
            .child(kind)
            .child(name)
            .children(running)
            .children(dot)
            .child(close)
            .when(!self.tab_menu_open, |h| {
                h.tooltip(move |window, cx| theme::tooltip(conn.clone(), window, cx))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, _| this.tab_menu_target = Some(ix)),
            )
            // A tab dragged onto another takes that one's place in the strip.
            .on_drag(DraggedTab(ix, title.clone().into()), |d, _, _, cx| {
                cx.new(|_| TabDragPreview(d.1.clone()))
            })
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(wash).border_color(bar))
            .on_drop(cx.listener(move |this, d: &DraggedTab, _, cx| this.move_tab(d.0, ix, cx)))
            // A middle click closes as the close button does, guard included, without activating.
            .on_aux_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if e.is_middle_click() {
                    cx.stop_propagation();
                    this.close_tab_at(ix, window, cx);
                }
            }))
            // A double click also pins a preview.
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if e.click_count() == 2 {
                    this.pin_tab(ix, cx);
                }
                this.activate(ix, window, cx);
            }));
        // Inactive tabs sit behind the strip's hairline; the active one opens onto the editor.
        if active { handle } else { handle.border_b_1() }
    }

    // The status bar over the open tab's connection and model, when there is a tab.
    fn foot(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab().map(|t| t.read(cx));
        // Without a tab the bar names the explorer's selected connection and its link.
        let idle = match (&tab, self.sidebar.as_ref().map(|s| s.read(cx))) {
            (None, Some(sidebar)) => self.selected_connection(cx).map(|name| {
                let link = sidebar.link(&name);
                (name, link)
            }),
            _ => None,
        };
        let idle = idle.as_ref().map(|(n, l)| (n.as_str(), l));
        status_bar(tab.map(|t| (t, t.model().read(cx))), idle, self.closing, cx)
    }

    // The no-tab column: four start actions, each calling the same method as its key binding.
    // New tab keeps Ctrl T's meaning, so with no tab and no selection it opens nothing.
    fn start_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        type Action = fn(&mut Shell, &mut Window, &mut Context<Shell>);
        let actions: [(&str, IconName, Option<SharedString>, Action); 4] = [
            ("New tab", Icons::Plus, keys_for(&NewTab, cx), |s, w, cx| {
                s.new_tab(w, cx)
            }),
            (
                "Open query",
                Icons::FileCode,
                keys_for(&OpenQuery, cx),
                |s, w, cx| s.open_query(w, cx),
            ),
            (
                "New connection",
                Icons::Database,
                keys_for(&NewConnection, cx),
                |s, w, cx| s.new_connection(w, cx),
            ),
            (
                "History",
                Icons::Clock,
                keys_for(&ToggleHistory, cx),
                |s, _, cx| s.toggle_history(cx),
            ),
        ];
        let theme = cx.theme();
        let (muted, hover, radius) = (theme.muted_foreground, theme.list_hover, theme.radius);
        let rows = actions
            .into_iter()
            .enumerate()
            .map(|(ix, (label, glyph, keys, act))| {
                div()
                    .id(("start-action", ix))
                    .aria_label(label)
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .w_full()
                    .h(px(32.))
                    .px(px(8.))
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(ui::icon(glyph, px(14.), muted))
                    .child(div().flex_1().child(label))
                    .children(keys.map(|k| ui::key_chip(chip_keys(&k), cx)))
                    .on_click(cx.listener(move |this, _, window, cx| act(this, window, cx)))
            });
        column("No tab open", "start-title", cx)
            .id("start-card")
            .test_support()
            .child(ui::section_label("Start", cx).mt(px(4.)))
            .children(rows)
    }

    // The welcome column: the open button, the recent workspaces and an error slot.
    fn welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, fg, hover, radius) = (
            theme.muted_foreground,
            theme.danger,
            theme.foreground,
            theme.list_hover,
            theme.radius,
        );
        let recent = self.recent.iter().enumerate().map(|(ix, dir)| {
            let label = SharedString::from(dir.display().to_string());
            let dir = dir.clone();
            div()
                .id(("recent-workspace", ix))
                .aria_label(label.clone())
                .test_support()
                .flex()
                .items_center()
                .w_full()
                .h(px(28.))
                .px(px(8.))
                .rounded(radius)
                .text_sm()
                .text_color(muted)
                .cursor_pointer()
                .hover(move |s| s.bg(hover).text_color(fg))
                // A long path drops its head, so the folder name at its end stays readable.
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .child(label),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switch_workspace(dir.clone(), window, cx)
                }))
        });
        let recents = (!self.recent.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .w_full()
                .mt(px(4.))
                .child(ui::section_label("Recent", cx).mb(px(4.)))
                .children(recent)
        });
        let body = column("Open a workspace", "welcome-title", cx)
            .id("welcome-card")
            .test_support()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::dim(cx))
                    .child("Open a workspace folder that holds a dbento.toml."),
            )
            .child(
                Button::new("welcome-open")
                    .primary()
                    .w_full()
                    .label("Open workspace")
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_open(window, cx))),
            )
            .children(recents);
        // The error slot sits below the column and keeps its height when empty, so the column
        // stays tight and does not jump when an error appears.
        let slot = div()
            .w(px(360.))
            .min_h(px(48.))
            .pt(px(8.))
            .children(self.error.clone().map(|error| {
                div()
                    .id("welcome-error")
                    .aria_label(error.clone())
                    .test_support()
                    .text_color(danger)
                    .child(error)
            }));
        div()
            .id("welcome")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_4()
            .child(body)
            .child(slot)
    }
}

/// The label each tab shows, from `(title, connection)` in strip order: a title carried by
/// tabs on two or more connections gets " · connection" on every tab carrying it.
pub fn tab_labels(tabs: &[(&str, &str)]) -> Vec<String> {
    tabs.iter()
        .map(|&(title, conn)| {
            let clash = tabs.iter().any(|&(t, c)| t == title && c != conn);
            if clash {
                format!("{title} \u{B7} {conn}")
            } else {
                title.to_owned()
            }
        })
        .collect()
}

/// How strongly a tagged tab draws its top line: full when active or production, else half.
pub fn tab_line_strength(active: bool, production: bool) -> f32 {
    if active || production { 1. } else { 0.5 }
}

// The 360 px column shared by the start and welcome views: no box, so the tab area's fill
// shows through, led by its SemiBold heading. Callers add the body.
fn column(title: &'static str, title_id: &'static str, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(8.))
        .w(px(360.))
        .text_color(cx.theme().foreground)
        .child(
            div()
                .id(title_id)
                .aria_label(title)
                .test_support()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
}

/// Whether a tab on `tab_connection` runs on `connection` or one of its children.
pub fn runs_on(tab_connection: &str, connection: &str) -> bool {
    tab_connection
        .strip_prefix(connection)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The connection a new tab opens on: the explorer's selection when it `moved` since the last
/// tab activation, else the active `tab`'s, else the selection.
pub fn new_tab_connection<T>(tab: Option<T>, selected: Option<T>, moved: bool) -> Option<T> {
    match selected {
        Some(s) if moved => Some(s),
        _ => tab.or(selected),
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The side panels share what the tab area leaves at its minimum, so a narrow window
        // squeezes them instead of pushing the dock out of sight.
        // The expanded record takes the inspector's place; the dock stays open underneath.
        let expanded = self
            .tab()
            .is_some_and(|t| t.read(cx).results_pane().read(cx).view() == ResultView::Record);
        let inspector = self.inspector_open && !expanded;
        let docked = self.history_open || inspector || self.approvals_open || self.definition_open;
        let sides = if docked { 2. } else { 1. };
        let split_width = window.viewport_size().width - RAIL_WIDTH - GUTTER;
        let side_max = ((split_width - MIN_TAB_AREA) / sides).max(PANEL_MIN);
        // The gutters are padding inside the resizable panels, every gap on the tab area's
        // side, so the sidebar box is its panel's saved width and each resize handle stays on
        // a box edge, reaching into the gap beside it.
        let docked: Option<AnyView> = if self.approvals_open {
            Some(self.approvals_panel.clone().into())
        } else if self.definition_open {
            Some(self.definition_panel.clone().into())
        } else if self.inspector_open {
            inspector.then(|| self.inspector.clone().into())
        } else {
            self.history_panel
                .clone()
                .filter(|_| self.history_open)
                .map(Into::into)
        };
        let dock = docked.map(|panel| {
            resizable_panel()
                .size(px(320.) + GUTTER)
                .size_range(PANEL_MIN..side_max)
                .flex_shrink_0()
                .child(gutter(px(0.), GUTTER).child(panel))
        });
        let content = match &self.sidebar {
            // The side panels never shrink, so a panel added or removed resizes only the tab
            // area; a drag still moves them, since it sets their size.
            Some(sidebar) => h_resizable("shell-split")
                .with_handle_appearance(gap_handle())
                .with_state(&self.split)
                .child(
                    resizable_panel()
                        .size(px(self.sidebar_width.unwrap_or(260.)))
                        .size_range(PANEL_MIN..side_max)
                        .flex_shrink_0()
                        .child(
                            gutter(px(0.), px(0.)).child(
                                panel_box(cx)
                                    .id("sidebar")
                                    .test_support()
                                    .child(sidebar.clone()),
                            ),
                        ),
                )
                .child(resizable_panel().child(gutter(GUTTER, GUTTER).child(self.tab_area(cx))))
                // The group's `children` replaces the panels, so the dock is added with `child`.
                .when_some(dock, |group, panel| group.child(panel))
                .into_any_element(),
            None => self.welcome(cx).into_any_element(),
        };
        // Only a workspace has panels to switch, so the welcome view has no rail.
        let rail = self.sidebar.as_ref().map(|_| self.rail(cx));
        let rail_shown = rail.is_some();
        div()
            .id("shell")
            .test_support()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CycleAppearance, _, cx| this.cycle_appearance(cx)))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewConnection, window, cx| this.new_connection(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleHistory, _, cx| this.toggle_history(cx)))
            .on_action(cx.listener(|this, _: &ToggleRowInspector, _, cx| this.toggle_inspector(cx)))
            .on_action(cx.listener(|this, _: &NavBack, window, cx| {
                this.nav_back(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NavForward, window, cx| {
                this.nav_forward(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ReloadWorkspace, window, cx| {
                this.reload_workspace(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SaveQuery, window, cx| this.save_active(window, cx)))
            .on_action(cx.listener(|this, _: &OpenQuery, window, cx| this.open_query(window, cx)))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                this.open_palette(Mode::All, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenCommands, window, cx| {
                this.open_palette(Mode::Commands, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(ix) = this.active {
                    this.close_tab_at(ix, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &RecentTabBack, window, cx| {
                    this.walk_recent(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &RecentTabForward, window, cx| {
                this.walk_recent(false, window, cx)
            }))
            .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, _, _| {
                if !e.modifiers.control {
                    this.tab_recency.commit();
                }
            }))
            .on_action(cx.listener(|this, _: &GoToTab1, w, cx| this.go_to_tab(Some(0), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab2, w, cx| this.go_to_tab(Some(1), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab3, w, cx| this.go_to_tab(Some(2), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab4, w, cx| this.go_to_tab(Some(3), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab5, w, cx| this.go_to_tab(Some(4), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab6, w, cx| this.go_to_tab(Some(5), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab7, w, cx| this.go_to_tab(Some(6), w, cx)))
            .on_action(cx.listener(|this, _: &GoToTab8, w, cx| this.go_to_tab(Some(7), w, cx)))
            .on_action(cx.listener(|this, _: &GoToLastTab, w, cx| this.go_to_tab(None, w, cx)))
            .on_action(cx.listener(|this, _: &MoveTabLeft, _, cx| this.move_active(false, cx)))
            .on_action(cx.listener(|this, _: &MoveTabRight, _, cx| this.move_active(true, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.title_bar(cx))
            .child(
                div().flex_1().min_h_0().flex().children(rail).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .when(rail_shown, |d| d.pl(GUTTER))
                        .child(content),
                ),
            )
            .child(self.foot(cx))
    }
}

impl Shell {
    // The 40 px bar on the frame: the app icon at left and, with a workspace open, the
    // Open-query pill centred on the window rather than on the space the controls leave.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Linux draws its own close button, which otherwise removes the window directly.
        let this = cx.weak_entity();
        let bar = TitleBar::new()
            .h(TITLE_BAR)
            .border_b_0()
            .on_close_window(move |_, window, cx| {
                let _ = this.update(cx, |shell, cx| shell.title_close(window, cx));
            })
            .child(img(ui::APP_ICON).size(px(16.)).flex_none());
        let pill = self.sidebar.as_ref().map(|_| self.title_pill(cx));
        div()
            .relative()
            .flex_none()
            .child(bar)
            .children(pill.map(|pill| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    // Equal insets keep the pill centred and off the minimise, maximise and
                    // close buttons; in a narrow window the pill shrinks instead.
                    .px(TITLE_BAR_HEIGHT * 3.)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(pill)
            }))
    }

    fn title_pill(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (dim, fill) = (theme::dim(cx), theme::editor_fill(cx));
        let theme = cx.theme();
        div()
            .id("title-search")
            .aria_label(PALETTE_LABEL)
            .test_support()
            .flex()
            .items_center()
            .gap_2()
            .w(px(420.))
            .max_w_full()
            .h(px(28.))
            .pl_2()
            .pr_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(fill)
            .text_sm()
            .text_color(dim)
            .cursor_pointer()
            .hover(|s| s.border_color(theme.input))
            .child(ui::icon(Icons::Search, px(14.), theme.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().child(PALETTE_LABEL))
            .children(keys_for(&OpenPalette, cx).map(|k| {
                let keys = chip_keys(&k);
                ui::key_chip(keys.clone(), cx)
                    .id("title-search-keys")
                    .aria_label(keys)
                    .test_support()
            }))
            // The title bar starts a window move on a press, which a click here must not do.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(Mode::All, window, cx)))
    }

    // The activity rail: one button per panel or picker, each calling what its key calls,
    // with Appearance pinned to the bottom group (where room is left for more).
    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let appearance = format!("Appearance: {}", self.appearance.label());
        div()
            .flex_none()
            .w(RAIL_WIDTH)
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .py(GUTTER)
            .gap_1()
            .child(
                rail_button("rail-explorer", Icons::Database, "Explorer", None, true, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.focus_filter(window, cx))),
            )
            .child(
                rail_button(
                    "rail-queries",
                    Icons::FileCode,
                    "Saved queries",
                    keys_for(&OpenQuery, cx),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_query(window, cx))),
            )
            .child(
                rail_button(
                    "history-toggle",
                    Icons::Clock,
                    "History",
                    keys_for(&ToggleHistory, cx),
                    self.history_open,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_history(cx))),
            )
            .child(self.approvals_button(cx))
            .child(div().flex_1())
            .child(
                rail_button(
                    "rail-settings",
                    Icons::Settings,
                    "Settings",
                    keys_for(&OpenSettings, cx),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(
                rail_button(
                    "rail-appearance",
                    theme::appearance_icon(self.appearance),
                    appearance,
                    keys_for(&theme::CycleAppearance, cx),
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.cycle_appearance(cx))),
            )
    }

    // The MCP approvals button, toggling its panel, with an accent badge counting the pending
    // requests at its top right while there are any: round for one digit, a pill past that.
    fn approvals_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let badge = (!self.approvals_waiting.is_empty()).then(|| {
            let count = SharedString::from(badge_count(self.approvals_waiting.len()));
            div()
                .id("rail-approvals-badge")
                .aria_label(count.clone())
                .test_support()
                .absolute()
                .top(px(-2.))
                .right(px(-2.))
                .h(px(14.))
                .min_w(px(14.))
                .px(px(3.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .text_size(px(10.))
                .line_height(px(14.))
                .child(count)
        });
        rail_button(
            "rail-approvals",
            Icons::Bot,
            "MCP approvals",
            None,
            self.approvals_open,
            cx,
        )
        .children(badge)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_approvals(cx)))
    }

    /// MCP requests waiting for an answer, as the approvals poll last counted them.
    pub fn approvals_pending(&self) -> usize {
        self.approvals_waiting.len()
    }

    // This session's approvals whose grant may still change, with their index: those still
    // waiting, and those held back after a first Expired reading.
    fn grants_to_recheck(&self) -> Vec<(usize, String)> {
        self.approvals_decided
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.tentative || d.decided.outcome == Outcome::Granted(GrantState::Waiting)
            })
            .map(|(ix, d)| (ix, d.decided.request.id().to_owned()))
            .collect()
    }

    // Takes one poll's readings. A first Expired keeps the previous word for a tick, since
    // `claim` takes the grant before it writes the marker; a second one stands.
    fn approvals_read(
        &mut self,
        pending: Vec<ApprovalRequest>,
        states: Vec<(usize, GrantState)>,
        cx: &mut Context<Self>,
    ) {
        if pending.len() != self.approvals_waiting.len() {
            cx.notify();
        }
        self.approvals_waiting = pending;
        for (ix, state) in states {
            let Some(d) = self.approvals_decided.get_mut(ix) else {
                continue;
            };
            match state {
                GrantState::Expired if !d.tentative => d.tentative = true,
                state @ (GrantState::Waiting | GrantState::Ran | GrantState::Expired | _) => {
                    d.decided.outcome = Outcome::Granted(state);
                    d.tentative = false;
                }
            }
        }
        self.show_approvals(cx);
    }

    // Records an answer the store accepted, newest first; an Approve starts out waiting.
    fn approvals_decided(
        &mut self,
        request: ApprovalRequest,
        decision: Decision,
        cx: &mut Context<Self>,
    ) {
        let Some(outcome) = Outcome::of(decision) else {
            return;
        };
        // The answered request leaves the list now rather than at the next poll.
        self.approvals_waiting.retain(|r| r.id() != request.id());
        let decided = SessionDecision {
            decided: Decided::new(request, outcome),
            tentative: false,
        };
        self.approvals_decided.insert(0, decided);
        self.show_approvals(cx);
        cx.notify();
    }

    // Hands the panel the current lists and the workspace its connect command names.
    fn show_approvals(&self, cx: &mut Context<Self>) {
        let pending = self.approvals_waiting.clone();
        let decided = self
            .approvals_decided
            .iter()
            .map(|d| d.decided.clone())
            .collect();
        let dir = self.workspace.as_ref().map(|ws| ws.dir().to_path_buf());
        self.approvals_panel
            .update(cx, |p, cx| p.set(pending, decided, dir, cx));
    }

    // The Explorer button's action: the sidebar always shows, so it moves focus to the filter.
    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sidebar) = &self.sidebar {
            let filter = sidebar.read(cx).filter_state().clone();
            filter.update(cx, |input, cx| input.focus(window, cx));
        }
    }
}

// The split's dividers in a gutter: nothing at rest, since the panel boxes draw their own
// edges, and the kit's pill once the pointer engages one.
fn gap_handle() -> ResizeHandleRenderer {
    let kit = resize_handle_appearance();
    Rc::new(move |handle, window, cx| match handle.state() {
        ResizeHandleState::Idle => Some(div().flex_none().w(px(1.)).h_full().into_any_element()),
        ResizeHandleState::Hovered | ResizeHandleState::Pressed | ResizeHandleState::Dragging => {
            kit(handle, window, cx)
        }
    })
}

/// The title pill's text and aria label: what the palette searches.
const PALETTE_LABEL: &str = "Search tables, queries and commands";
const GUTTER: Pixels = px(6.);
const TITLE_BAR: Pixels = px(40.);
const TAB_STRIP: Pixels = px(36.);

// Whether `a` and `b` name one relation on `m`'s connection; SQLite names match without
// regard to case, as the database matches them.
fn same_relation(m: &TabModel, a: &RelationRef, b: &RelationRef) -> bool {
    let name = &m.spec().connection;
    let sqlite = m
        .spec()
        .workspace
        .connection(name)
        .is_some_and(|c| c.engine() == Engine::Sqlite);
    a == b
        || sqlite
            && a.schema().eq_ignore_ascii_case(b.schema())
            && a.name().eq_ignore_ascii_case(b.name())
}

// Names `tab` after `rel` and shows its select as saved text: what opening a relation and
// restoring a table tab share.
fn point_at(
    tab: &mut QueryTab,
    rel: &RelationRef,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) {
    let sql = select_text(tab.conn(), rel);
    tab.set_origin(rel.clone());
    tab.set_sql(&sql, window, cx);
    tab.mark_saved(cx);
}

// Gives a restored tab its table back, with the select when `fresh` (no draft or file text).
// A tab that still browsed defers its target to the first run, so restoring runs nothing; a
// table gone by then fails that load and runs the SQL as a plain query.
fn restore_table(
    tab: &mut QueryTab,
    table: &TableState,
    fresh: bool,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) {
    let rel = RelationRef::new(&table.schema, &table.name);
    if fresh {
        point_at(tab, &rel, window, cx);
    } else {
        tab.set_origin(rel.clone());
    }
    if let Some(b) = &table.browse {
        let sort = b.sort.as_ref().map(|s| Sort::new(&s.column, s.descending));
        let browse = Browse::new(b.filter.clone(), sort);
        tab.model()
            .clone()
            .update(cx, |m, cx| m.defer_target(rel, browse, cx));
    }
}

// What the session keeps of a tab opened on `rel`: the relation and the browse it runs.
fn table_state(rel: &RelationRef, m: &TabModel) -> TableState {
    let browse = m.browse_settings().map(|b| BrowseSaved {
        filter: b.filter,
        sort: b.sort.map(|s| SortSaved {
            column: s.column,
            descending: s.descending,
        }),
    });
    TableState {
        schema: rel.schema().to_owned(),
        name: rel.name().to_owned(),
        browse,
    }
}

// The window height outside a tab's query split: the title bar, the tab strip, the tab's
// toolbar, the status bar and the gutters between them.
fn query_chrome() -> Pixels {
    TITLE_BAR + GUTTER + TAB_STRIP + TOOLBAR_HEIGHT + GUTTER + STATUS_HEIGHT
}
const RAIL_WIDTH: Pixels = px(44.);

// A panel's slot inside its resizable panel: the gutter above and below, and `left` and
// `right` at the sides.
fn gutter(left: Pixels, right: Pixels) -> Div {
    div().size_full().py(GUTTER).pl(left).pr(right)
}

// A floating panel: the panel fill with a hairline edge and the shared radius.
pub(crate) fn panel_box(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .size_full()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
}

// The rail badge's text: the count, capped so it stays a small pill.
fn badge_count(n: usize) -> String {
    if n > 99 {
        "99+".to_owned()
    } else {
        n.to_string()
    }
}

// A 32 px rail button with an 18 px icon, labelled `label`; its tooltip boxes `keys` when
// the button has a shortcut. An active one wears the selection fill and a 2 px accent bar.
fn rail_button(
    id: &'static str,
    name: IconName,
    label: impl Into<SharedString>,
    keys: Option<SharedString>,
    active: bool,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    let label: SharedString = label.into();
    let theme = cx.theme();
    let (fg, dim, hover) = (theme.foreground, theme::dim(cx), theme.list_hover);
    let bar = active.then(|| {
        div()
            .absolute()
            .left(px(-6.))
            .top_0()
            .w(px(2.))
            .h_full()
            .bg(theme.primary)
    });
    let tip = label.clone();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .test_support()
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded(theme.radius)
        .cursor_pointer()
        .when(active, |s| s.bg(theme.list_active))
        .when(!active, |s| s.hover(|s| s.bg(hover)))
        .children(bar)
        // The icon names itself, so a test can read which glyph the rail shows.
        .child(
            div()
                .id(SharedString::from(format!("{id}-icon")))
                .aria_label(format!("{name:?}"))
                .test_support()
                .child(ui::icon(name, px(18.), if active { fg } else { dim })),
        )
        .tooltip(move |window, cx| theme::tooltip_keys(tip.clone(), keys.clone(), window, cx))
}

// The tab strip's context menu for the right-clicked handle; each row finds its tab again by
// identity on click, since tabs may close while the menu is open.
fn tab_menu(menu: PopupMenu, weak: &WeakEntity<Shell>, cx: &mut App) -> PopupMenu {
    let Some(shell) = weak.upgrade() else {
        return menu;
    };
    let (id, file) = {
        let s = shell.read(cx);
        let Some(tab) = s.tab_menu_target.and_then(|ix| s.tabs.get(ix)) else {
            return menu;
        };
        (tab.entity_id(), tab.read(cx).file().is_some())
    };
    let act = |f: fn(&mut Shell, usize, &mut Window, &mut Context<Shell>)| {
        let weak = weak.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |s, cx| {
                if let Some(ix) = s.tabs.iter().position(|t| t.entity_id() == id) {
                    f(s, ix, window, cx);
                }
            });
        }
    };
    let row = |id, icon, text, keys| ui::menu_item(id, icon, text, keys, cx);
    menu.min_w(px(180.))
        .item(
            row(
                "tab-menu-close",
                Some(Icons::X),
                "Close",
                keys_for(&CloseTab, cx),
            )
            .on_click(act(Shell::close_tab_at)),
        )
        .item(
            row("tab-menu-close-others", None, "Close others", None)
                .on_click(act(|s, ix, w, cx| s.close_tabs_except(Some(ix), w, cx))),
        )
        .item(
            row("tab-menu-close-all", None, "Close all", None)
                .on_click(act(|s, _, w, cx| s.close_tabs_except(None, w, cx))),
        )
        .separator()
        .item(
            row("tab-menu-rename", Some(Icons::Pencil), "Rename", None)
                .disabled(file)
                .on_click(act(Shell::rename_tab)),
        )
        .item(
            row(
                "tab-menu-duplicate",
                Some(Icons::CopyPlus),
                "Duplicate",
                None,
            )
            .on_click(act(Shell::duplicate_tab)),
        )
        .item(
            row(
                "tab-menu-save",
                Some(Icons::Save),
                "Save as query",
                keys_for(&SaveQuery, cx),
            )
            .on_click(act(Shell::save_tab)),
        )
}

// How many edits `tab` has staged, its apply prompt's held ones included.
fn staged_count(tab: &Entity<QueryTab>, cx: &App) -> usize {
    tab.read(cx).model().read(cx).edits().len()
}

// The titles of the tabs whose statement would be cancelled: one executing (fetching past
// its cap included) or a write draining. A result parked on its rows has them on screen.
fn running_titles<'a>(tabs: impl Iterator<Item = &'a Entity<QueryTab>>, cx: &App) -> Vec<String> {
    tabs.filter(|t| {
        let m = t.read(cx).model().read(cx);
        m.running() || m.settling()
    })
    .map(|t| t.read(cx).title())
    .collect()
}

// The question's sentence naming the tabs whose statements `action` cancels: both of two,
// else the first and a count of the rest.
fn running_detail(action: &str, titles: &[String]) -> Option<String> {
    let names = match titles {
        [] => return None,
        [one] => return Some(format!("{action} cancels the statement running in {one}.")),
        [a, b] => format!("{a} and {b}"),
        [a, rest @ ..] => format!("{a} and {} more tabs", rest.len()),
    };
    Some(format!(
        "{action} cancels the statements running in {names}."
    ))
}

// The close-tab question's detail for edited SQL.
const UNSAVED_SQL: &str = "Its SQL is not saved anywhere.";

// The question's detail when `lost` tabs' drafts could not be written.
fn lost_detail(lost: usize) -> String {
    match lost {
        1 => "1 tab could not be kept as a draft.".to_owned(),
        n => format!("{n} tabs could not be kept as drafts."),
    }
}

// The shell's discard question on screen; `serial` tells its answer from a later question's.
struct Question {
    serial: u64,
    window_close: bool,
    dialog: confirm::Asked,
}

// The Keep-or-Close question before edited SQL is discarded; Keep is the default and the
// cancel answer, so neither Enter nor Escape loses SQL.
fn ask_discard(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    keep: &'static str,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> confirm::Asked {
    let choice = |id, label: &'static str, danger| Choice {
        id,
        label: label.into(),
        danger,
    };
    confirm::ask_closable(
        Ask {
            title: title.into(),
            detail: Some(detail.into()),
            sql: None,
            language: "sql",
            choices: vec![
                choice("discard-keep", keep, false),
                choice("discard-close", "Close", true),
            ],
            default: 0,
            cancel: 0,
        },
        window,
        cx,
    )
}

// Only a connection whose engine takes a password and reads the keychain owns a stored secret.
fn keychain_backed(conn: &dbento_core::ConnectionConfig) -> bool {
    conn.engine().capabilities().password && matches!(conn.secret_source(), SecretSource::Keychain)
}

// How the connection dialog opens: blank, on an existing connection, or as a copy of one
// under a new name.
enum FormStart {
    New,
    Edit(String, ConnectionSpec),
    Copy(String, ConnectionSpec),
}

// What `write_edit` did: refused (already shown), found nothing to write, or wrote the plan.
enum Written {
    Failed,
    Unchanged,
    Done(Box<EditPlan>),
}

// The refusal to rename or delete a connection an open tab runs on.
const IN_USE: &str = "Close its tabs first";

// A planned edit always holds its connection; this only guards the lookup.
fn missing(name: &str) -> String {
    format!("`{name}` is not in the planned dbento.toml")
}

// The text of folder `name` inside `parent` (None = top level).
fn child_text(parent: Option<&FolderPath>, name: &str) -> String {
    match parent {
        Some(p) => format!("{}/{name}", p.as_str()),
        None => name.to_owned(),
    }
}

// The folder a name dialog's `name` means under `parent`, or why it cannot be one. `current` is
// the folder being renamed, which may keep its own name.
fn folder_named(
    ws: &Workspace,
    parent: Option<&FolderPath>,
    name: &str,
    current: Option<&FolderPath>,
) -> Result<FolderPath, SharedString> {
    if name.is_empty() {
        return Err("Enter a name.".into());
    }
    if name.contains('/') {
        return Err("A folder name cannot contain /.".into());
    }
    let path: FolderPath = child_text(parent, name)
        .parse()
        .map_err(|e| SharedString::from(format!("{e}")))?;
    let taken = ws.folders().iter().any(|f| f.path() == &path);
    if taken && current != Some(&path) {
        return Err(format!("A folder `{}` already exists.", path.as_str()).into());
    }
    Ok(path)
}

// A one-button error dialog, `save-error-ok`, for a saved query that cannot be read or written.
fn show_error(title: &'static str, detail: String, window: &mut Window, cx: &mut gpui_kit::App) {
    let ok = Choice {
        id: "save-error-ok",
        label: "OK".into(),
        danger: false,
    };
    let _ = ask(
        Ask {
            title: title.into(),
            detail: Some(detail.into()),
            sql: None,
            language: "sql",
            choices: vec![ok],
            default: 0,
            cancel: 0,
        },
        window,
        cx,
    );
}

// A tab handle being dragged: its strip index and title.
struct DraggedTab(usize, SharedString);

// The chip under the pointer while a tab is dragged, named after the tab.
struct TabDragPreview(SharedString);

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("tab-drag-preview")
            .aria_label(self.0.clone())
            .test_support()
            .h(TAB_STRIP)
            .px_3()
            .flex()
            .items_center()
            .max_w(px(280.))
            .truncate()
            .text_sm()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.drag_border)
            .rounded(theme.radius)
            .shadow_md()
            .opacity(0.9)
            .child(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::listed_within;
    use std::time::Duration;

    #[tokio::test]
    async fn a_database_list_that_never_answers_gives_up_at_the_cap() {
        let never = std::future::pending::<Result<Vec<String>, ()>>();
        assert_eq!(listed_within(Duration::from_millis(20), never).await, None);
    }

    #[tokio::test]
    async fn a_failed_database_list_offers_nothing_and_an_answer_is_kept() {
        let failed = async { Err::<Vec<String>, ()>(()) };
        assert_eq!(
            listed_within(Duration::from_secs(5), failed).await,
            Some(Vec::new())
        );
        let answered = async { Ok::<_, ()>(vec!["app".to_owned()]) };
        assert_eq!(
            listed_within(Duration::from_secs(5), answered).await,
            Some(vec!["app".to_owned()])
        );
    }
}
