//! The Dbento desktop app.

// A release build links as a Windows GUI program so no console opens beside the window;
// debug builds keep the console for their logs. Without a console, stdout and stderr
// writes are silently dropped unless the launcher redirected them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![warn(clippy::wildcard_enum_match_arm)]

use std::sync::Arc;
use std::time::Duration;

use dbento_app::app::{AppPaths, Services, start};
use dbento_app::launch::{self, Launch, LaunchError};
use dbento_app::team::{self, TeamHooks};
use dbento_core::paths::{STATE_DIR_VAR, StatePaths};
use dbento_core::{
    KeychainStore, KeychainTokenStore, MemoryStore, MemoryTokenStore, SecretStore, TimedStore,
    TimedTokenStore, TokenStore,
};
use gpui_kit::AppContext;

fn main() {
    // The command line is settled before GPUI starts, so help, version and a bad path all
    // answer without a display.
    let dir = match launch::parse(std::env::args_os().skip(1)) {
        Ok(Launch::Help) => return println!("{}", launch::USAGE),
        Ok(Launch::Version) => return println!("dbento-app {}", env!("CARGO_PKG_VERSION")),
        Ok(Launch::Open(dir)) => dir,
        Err(e @ LaunchError::UnknownFlag(_)) => {
            eprintln!("dbento-app: {e}\n\n{}", launch::USAGE);
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("dbento-app: {e}");
            std::process::exit(2);
        }
    };
    // GPUI waits forever for a Linux display that is not there, so say so and stop.
    #[cfg(target_os = "linux")]
    if !launch::has_display(|v| std::env::var_os(v)) {
        eprintln!("dbento-app needs a graphical display (WAYLAND_DISPLAY or DISPLAY is not set)");
        std::process::exit(1);
    }
    // `xtask app-bench` hands the app a job file; the bench then owns the whole process.
    #[cfg(feature = "bench")]
    if let Some(config) = std::env::var_os("DBENTO_APP_BENCH") {
        dbento_app::bench::main(config.into());
    }
    gpui_kit::application()
        .with_assets(dbento_app::ui::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            dbento_app::theme::install(cx);
            dbento_app::tokio_bridge::init(cx);
            // The real app follows outside edits to its workspace; test processes never ask to.
            dbento_app::workspace_reload::enable_watching(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            // DBENTO_STATE_DIR keeps a run's state apart, as `make demo` does.
            let Some(state_paths) = StatePaths::for_state_dir(std::env::var_os(STATE_DIR_VAR))
            else {
                eprintln!("dbento: no home directory for app state");
                return cx.quit();
            };
            let paths = AppPaths::from(state_paths.clone());
            // Connecting to the keychain can block, so it runs off the main thread; without
            // one, secrets live in memory for this run.
            let keychain =
                cx.background_spawn(async { (KeychainStore::new(), KeychainTokenStore::new()) });
            cx.spawn(async move |cx| {
                let (secrets, tokens) = keychain.await;
                let (store, secrets_in_memory): (Arc<dyn SecretStore>, bool) = match secrets {
                    Ok(k) => (Arc::new(TimedStore::new(k)), false),
                    Err(e) => {
                        eprintln!("dbento: keychain unavailable ({e}); secrets last until quit");
                        (Arc::new(MemoryStore::new()), true)
                    }
                };
                // Team tokens get the same timeout, since sign-in, join and sign-out read them
                // outside the sync, and the same memory fallback lasting until quit.
                let tokens: Arc<dyn TokenStore> = match tokens {
                    Ok(k) => Arc::new(TimedTokenStore::new(Arc::new(k), Duration::from_secs(10))),
                    Err(e) => {
                        eprintln!(
                            "dbento: keychain unavailable ({e}); team sign-ins last until quit"
                        );
                        Arc::new(MemoryTokenStore::new())
                    }
                };
                let services = Services {
                    paths,
                    store,
                    env: Arc::new(|v| std::env::var(v).ok()),
                    secrets_in_memory,
                };
                cx.update(|cx| {
                    // The schedule's first tick waits for `start` to install the settings store.
                    dbento_app::updates::install(
                        Some(services.paths.update_state.clone()),
                        dbento_app::updates::UpdateHooks::real(),
                        cx,
                    );
                    team::install(TeamHooks::new(tokens, state_paths), cx);
                    match start(services, dir, cx) {
                        Ok(_) => cx.activate(true),
                        Err(e) => {
                            eprintln!("dbento: {e}");
                            cx.quit();
                        }
                    }
                });
            })
            .detach();
        });
}
