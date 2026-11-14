//! The Dbento desktop app.

// A release build links as a Windows GUI program so no console opens beside the window;
// debug builds keep the console for their logs. Without a console, stdout and stderr
// writes are silently dropped unless the launcher redirected them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![warn(clippy::wildcard_enum_match_arm)]

use std::sync::Arc;

use dbento_app::app::{AppPaths, Services, start};
use dbento_app::launch::{self, Launch, LaunchError};
use dbento_core::paths::STATE_DIR_VAR;
use dbento_core::{KeychainStore, MemoryStore, SecretStore, TimedStore};
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
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            // DBENTO_STATE_DIR keeps a run's state apart, as `make demo` does.
            let Some(paths) = AppPaths::for_state_dir(std::env::var_os(STATE_DIR_VAR)) else {
                eprintln!("dbento: no home directory for app state");
                return cx.quit();
            };
            // Connecting to the keychain can block, so it runs off the main thread; without
            // one, secrets live in memory for this run.
            let keychain = cx.background_spawn(async { KeychainStore::new() });
            cx.spawn(async move |cx| {
                let (store, secrets_in_memory): (Arc<dyn SecretStore>, bool) = match keychain.await
                {
                    Ok(k) => (Arc::new(TimedStore::new(k)), false),
                    Err(e) => {
                        eprintln!("dbento: keychain unavailable ({e}); secrets last until quit");
                        (Arc::new(MemoryStore::new()), true)
                    }
                };
                let services = Services {
                    paths,
                    store,
                    env: Arc::new(|v| std::env::var(v).ok()),
                    secrets_in_memory,
                };
                cx.update(|cx| match start(services, dir, cx) {
                    Ok(_) => cx.activate(true),
                    Err(e) => {
                        eprintln!("dbento: {e}");
                        cx.quit();
                    }
                });
            })
            .detach();
        });
}
