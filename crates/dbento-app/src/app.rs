//! Startup: the app's file locations, the services every view shares, and opening the
//! main window over them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dbento_core::SecretStore;
use dbento_core::paths::StatePaths;
use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, point, px, size,
};
use serde::{Deserialize, Serialize};

use crate::identity::{self, APP_ID};
use crate::persist::load_json;
use crate::settings_store;
use crate::shell::Shell;
use crate::theme::Appearance;
use crate::ui::{self, Notice, NoticeKind};

/// Looks up an environment variable; tests pass a closure instead of the process environment.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Where the app keeps its own files, outside any workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct AppPaths {
    /// The window and workspace state, as JSON.
    pub state: PathBuf,
    /// The query history, one JSON object per line.
    pub history: PathBuf,
    /// The folder of unsaved query drafts.
    pub drafts: PathBuf,
    /// The folder of pending MCP approvals.
    pub approvals: PathBuf,
    /// The user's settings, as JSON.
    pub settings: PathBuf,
    /// The user's key binding overrides, as JSON.
    pub keymap: PathBuf,
}

impl AppPaths {
    /// The platform locations of [`StatePaths::platform`]. `None` without a home directory.
    pub fn platform() -> Option<AppPaths> {
        StatePaths::platform().map(AppPaths::from)
    }

    /// [`AppPaths::in_dir`] of `state_dir` when it is set and non-empty, as the demo's
    /// `DBENTO_STATE_DIR` sets it; otherwise [`AppPaths::platform`].
    pub fn for_state_dir(state_dir: Option<std::ffi::OsString>) -> Option<AppPaths> {
        StatePaths::for_state_dir(state_dir).map(AppPaths::from)
    }

    /// Every file directly under `dir`, for tests and portable setups.
    pub fn in_dir(dir: &Path) -> AppPaths {
        AppPaths::from(StatePaths::in_dir(dir))
    }
}

impl From<StatePaths> for AppPaths {
    fn from(p: StatePaths) -> AppPaths {
        AppPaths {
            state: p.state,
            history: p.history,
            drafts: p.drafts,
            approvals: p.approvals,
            settings: p.settings,
            keymap: p.keymap,
        }
    }
}

/// What the app reaches outside itself: its files, the keychain and the environment.
#[derive(Clone)]
pub struct Services {
    /// The app's own file locations.
    pub paths: AppPaths,
    /// Where connection secrets are read from.
    pub store: Arc<dyn SecretStore>,
    /// The environment-variable fallback for secrets.
    pub env: EnvLookup,
    /// The keychain was unreachable at start, so `store` lasts until quit.
    pub secrets_in_memory: bool,
}

/// What the app remembers between runs, kept at [`AppPaths::state`].
/// A file missing a field loads that field's default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    /// The workspace folder open when the app last opened one.
    pub last_workspace: Option<PathBuf>,
    /// The appearance choice of an older state file, read only to seed settings.json, which
    /// owns it; never written back.
    #[serde(skip_serializing)]
    pub appearance: Appearance,
    /// The open tabs of the most recent workspaces, newest first, at most [`MAX_SESSIONS`].
    pub sessions: Vec<WorkspaceSession>,
    /// Workspace folders opened lately, newest first, at most [`MAX_RECENT`].
    pub recent_workspaces: Vec<PathBuf>,
    /// The main window's bounds when it last closed; an unreadable entry loads as None.
    #[serde(deserialize_with = "none_if_unreadable")]
    pub window: Option<WindowState>,
    /// The explorer's width when the window last closed.
    pub sidebar_width: Option<f32>,
    /// The active tab's editor height when the window last closed.
    pub editor_height: Option<f32>,
    /// Whether the history panel was showing.
    pub history_open: bool,
    /// Whether the row inspector was showing; the history panel wins when both are set.
    pub inspector_open: bool,
}

// A non-finite bound is written as null; reading it as no saved window keeps that one bad
// number from failing the whole file and every other setting with it.
fn none_if_unreadable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// How many recent workspaces the welcome view lists.
pub const MAX_RECENT: usize = 10;
/// The smallest window size: the window manager stops a resize here, and a saved state is
/// clamped to it.
pub const MIN_WINDOW: (f32, f32) = (480., 320.);
/// The largest window side a saved state may restore.
pub const MAX_WINDOW_SIDE: f32 = 16384.;

/// The main window's bounds, in logical pixels; for a maximized window, the bounds it
/// returns to when unmaximized.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
    /// Whether the window was maximized or fullscreen.
    pub maximized: bool,
}

impl WindowState {
    /// The state of a window whose bounds are `bounds`.
    pub fn from_bounds(bounds: WindowBounds) -> WindowState {
        let (b, maximized) = match bounds {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
        };
        WindowState {
            x: b.origin.x.into(),
            y: b.origin.y.into(),
            width: b.size.width.into(),
            height: b.size.height.into(),
            maximized,
        }
    }
}

/// How many workspaces' sessions the state file keeps.
pub const MAX_SESSIONS: usize = 10;

/// One workspace's open tabs, left to right, and which was active.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSession {
    /// The workspace folder.
    pub workspace: PathBuf,
    /// The open tabs, left to right.
    pub tabs: Vec<TabState>,
    /// The index of the active tab.
    pub active: Option<usize>,
    /// Collapsed explorer folder paths.
    pub collapsed: Vec<String>,
}

/// One saved tab: where it runs, its draft id and the saved query it edits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabState {
    /// The connection's name in the workspace.
    pub connection: String,
    /// The tab's draft id, never its text.
    pub draft: String,
    /// The saved query the tab edits, relative to the workspace.
    pub file: Option<PathBuf>,
    /// The relation a table tab was opened on; absent from sessions saved before it was kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<TableState>,
    /// The N of an untitled tab's "Query N", so it keeps its title across a restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<usize>,
}

/// A table tab's relation and, while it still browsed, the browse it ran.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableState {
    /// The relation's schema, unquoted.
    pub schema: String,
    /// The relation's name, unquoted.
    pub name: String,
    /// The filter and sort it browsed with; None once its SQL was edited away from the browse.
    pub browse: Option<BrowseSaved>,
}

/// A browse's filter and sort.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowseSaved {
    /// The SQL placed inside `WHERE (...)`.
    pub filter: Option<String>,
    /// The sort column and direction.
    pub sort: Option<SortSaved>,
}

/// One sort column and its direction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSaved {
    /// The column or locator sorted by.
    pub column: String,
    /// Whether the sort is descending.
    pub descending: bool,
}

impl SavedState {
    /// The saved session of `workspace`, if any.
    pub fn session(&self, workspace: &Path) -> Option<&WorkspaceSession> {
        self.sessions.iter().find(|s| s.workspace == workspace)
    }

    /// Replaces that workspace's session and moves it first; keeps [`MAX_SESSIONS`].
    pub fn put_session(&mut self, session: WorkspaceSession) {
        self.sessions.retain(|s| s.workspace != session.workspace);
        self.sessions.insert(0, session);
        self.sessions.truncate(MAX_SESSIONS);
    }

    /// Puts `dir` first among the recent workspaces, once, keeping at most [`MAX_RECENT`].
    pub fn remember_workspace(&mut self, dir: &Path) {
        self.forget_workspace(dir);
        self.recent_workspaces.insert(0, dir.to_path_buf());
        self.recent_workspaces.truncate(MAX_RECENT);
    }

    /// Drops `dir` from the recent workspaces.
    pub fn forget_workspace(&mut self, dir: &Path) {
        self.recent_workspaces.retain(|p| p != dir);
    }
}

/// Each recent workspace with whether it is missing: its folder holds no `dbento.toml` now,
/// as when its drive or share is unmounted.
pub fn recent_status(recent: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    recent
        .iter()
        .map(|dir| (dir.clone(), !dir.join("dbento.toml").is_file()))
        .collect()
}

/// The workspace to open at startup: the folder given on the command line, else the last
/// one opened, else none.
pub fn initial_workspace(arg: Option<PathBuf>, saved: &SavedState) -> Option<PathBuf> {
    arg.or_else(|| saved.last_workspace.clone())
}

/// The main window's options, built on the kit title bar's and titled "Dbento". On Linux
/// the app draws its own title bar, so the compositor does not add a second one.
pub fn window_options() -> WindowOptions {
    window_options_for(None)
}

/// The main window's options; with `saved`, its bounds clamped to `MIN_WINDOW..=MAX_WINDOW_SIDE`,
/// maximized when it was.
pub fn window_options_for(saved: Option<&WindowState>) -> WindowOptions {
    let window_bounds = saved.map(|w| {
        let clamp =
            |v: f32, min: f32| px(if v.is_finite() { v } else { min }.clamp(min, MAX_WINDOW_SIDE));
        let b = Bounds {
            origin: point(px(w.x), px(w.y)),
            size: size(clamp(w.width, MIN_WINDOW.0), clamp(w.height, MIN_WINDOW.1)),
        };
        if w.maximized {
            WindowBounds::Maximized(b)
        } else {
            WindowBounds::Windowed(b)
        }
    });
    WindowOptions {
        window_bounds,
        window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
        window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
        titlebar: Some(TitlebarOptions {
            title: Some("Dbento".into()),
            ..TitleBar::title_bar_options()
        }),
        app_id: Some(APP_ID.into()),
        icon: identity::window_icon(),
        ..TitleBar::window_options()
    }
}

/// Opens the main window over `services`, on the workspace folder `dir` when given or
/// else the last one opened, and on the welcome view without either. The caller has run
/// `gpui_kit::init` and `theme::install`.
pub fn start(
    services: Services,
    dir: Option<PathBuf>,
    cx: &mut App,
) -> gpui_kit::Result<(AnyWindowHandle, Entity<Shell>)> {
    let saved = load_json::<SavedState>(&services.paths.state);
    if let Some(message) = &saved.recovered {
        eprintln!("dbento: {message}");
    }
    let dir = initial_workspace(dir, &saved.value);
    // settings.json owns the appearance; an older state file's choice moves there at once,
    // since state.json no longer writes it back.
    let seeding = saved.value.appearance != Appearance::default()
        && matches!(services.paths.settings.try_exists(), Ok(false));
    let store = settings_store::install(
        Some(services.paths.settings.clone()),
        saved.value.appearance,
        cx,
    );
    if seeding {
        store.update(cx, |s, cx| {
            s.change(cx, |_| {});
            s.flush(cx);
        });
    }
    let appearance = store.read(cx).get().appearance;
    let keymap_warning = crate::keymap::install_user(Some(services.paths.keymap.clone()), cx);
    let history_open = saved.value.history_open;
    let mut options = window_options_for(saved.value.window.as_ref());
    // An origin on a monitor that is gone would leave the window off-screen, so it then opens
    // centred at its saved size.
    if let Some(bounds) = &mut options.window_bounds {
        let b = bounds.get_bounds();
        if !cx.displays().iter().any(|d| d.bounds().contains(&b.origin)) {
            let centred = Bounds::centered(None, b.size, cx);
            *bounds = match bounds {
                WindowBounds::Windowed(_) => WindowBounds::Windowed(centred),
                WindowBounds::Maximized(_) => WindowBounds::Maximized(centred),
                WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(centred),
            };
        }
    }
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        // X11 takes its window class only from this call, never from the options' app id.
        window.set_app_id(APP_ID);
        cx.new(|cx| {
            Shell::new(
                services,
                saved.value,
                dir,
                appearance,
                history_open,
                window,
                cx,
            )
        })
    })?;
    let settings_warning = store.update(cx, |s, _| s.take_startup_warning());
    for text in settings_warning.into_iter().chain(keymap_warning) {
        opened.0.update(cx, |_, window, cx| {
            ui::notice(
                window,
                cx,
                Notice {
                    text,
                    kind: NoticeKind::Warning,
                },
            )
        })?;
    }
    Ok(opened)
}
