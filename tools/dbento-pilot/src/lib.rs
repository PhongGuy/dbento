//! Dbento under gpui-pilot's headless host: the launch glue an agent drives.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use dbento_app::app::{AppPaths, Services, start};
use dbento_app::persist::save_json;
use dbento_app::settings::Settings;
use dbento_app::theme::Appearance;
use dbento_core::MemoryStore;
use dbento_core::paths::STATE_DIR_VAR;
use gpui_kit::{App, AssetSource};
use gpui_pilot_host::{Launch, PilotApp};

/// Set to `dark`, `light` or `system` (through the launch call's `env`) to seed that
/// appearance into a launch whose settings file does not exist yet. The headless host reports
/// a dark system, so `light` is the only way to a light shot.
pub const THEME_VAR: &str = "DBENTO_PILOT_THEME";

/// Dbento's own startup, run inside the host against the launch's fresh data directory.
/// The one optional launch argument is the workspace folder (see [`workspace_arg`]).
pub struct Dbento;

impl PilotApp for Dbento {
    fn assets(&self) -> Option<Arc<dyn AssetSource>> {
        Some(Arc::new(dbento_app::ui::AppAssets))
    }

    fn init(&self, cx: &mut App) {
        gpui_kit::init(cx);
        dbento_app::theme::install(cx);
        dbento_app::tokio_bridge::init(cx);
    }

    fn open(&self, launch: &Launch, cx: &mut App) -> anyhow::Result<()> {
        // Secrets stay in memory, so a piloted run never reaches the keychain. The state
        // folder follows DBENTO_STATE_DIR so a QA run shares it with `dbento mcp`.
        let dir = state_dir(&launch.data_dir, std::env::var_os(STATE_DIR_VAR));
        let services = Services {
            paths: AppPaths::in_dir(&dir),
            store: Arc::new(MemoryStore::new()),
            env: Arc::new(|v| std::env::var(v).ok()),
            secrets_in_memory: false,
        };
        seed_appearance(&dir, std::env::var(THEME_VAR).ok().as_deref())?;
        start(services, workspace_arg(&launch.args)?, cx)?;
        Ok(())
    }
}

/// The folder the app keeps its files in: the launch env's `DBENTO_STATE_DIR` when set and
/// non-empty, else the launch's own data folder.
pub fn state_dir(data_dir: &Path, var: Option<OsString>) -> PathBuf {
    match var {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => data_dir.to_path_buf(),
    }
}

/// The workspace folder named by the launch `args`, or `None` without one. Errors:
/// "workspace path `<a>` must be absolute", "workspace path `<a>` is not a directory",
/// "dbento-pilot takes at most one workspace path".
pub fn workspace_arg(args: &[String]) -> anyhow::Result<Option<PathBuf>> {
    let [arg] = args else {
        if args.is_empty() {
            return Ok(None);
        }
        bail!("dbento-pilot takes at most one workspace path");
    };
    let path = Path::new(arg);
    if !path.is_absolute() {
        bail!("workspace path `{arg}` must be absolute");
    }
    if !path.is_dir() {
        bail!("workspace path `{arg}` is not a directory");
    }
    Ok(Some(path.to_path_buf()))
}

/// Writes a settings file holding the appearance `value` (`dark`, `light` or `system`) into
/// `data_dir` when none exists, so a `keep_data` relaunch keeps what the app saved. `None`
/// writes nothing; another value is an error naming [`THEME_VAR`].
pub fn seed_appearance(data_dir: &Path, value: Option<&str>) -> anyhow::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let appearance = match value {
        "dark" => Appearance::Dark,
        "light" => Appearance::Light,
        "system" => Appearance::System,
        other => bail!("{THEME_VAR} must be dark, light or system, not `{other}`"),
    };
    let path = AppPaths::in_dir(data_dir).settings;
    if path.exists() {
        return Ok(());
    }
    let mut settings = Settings::default();
    settings.appearance = appearance;
    save_json(&path, &settings).with_context(|| format!("cannot seed {}", path.display()))
}
