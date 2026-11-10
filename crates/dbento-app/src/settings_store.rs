//! The one settings entity: every change applies live and is saved to `settings.json`
//! shortly after, in the debounced shape of Apinata's `Persisted`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};

use crate::persist::save_json;
use crate::settings::{self, Settings};
use crate::theme::Appearance;

/// How long a change waits before it is written, so a burst of edits costs one write.
pub const SAVE_DELAY: Duration = Duration::from_millis(500);

/// What the store reports to the shell.
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    /// A write failed; holds the I/O error text. Emitted at most once per session.
    SaveFailed(String),
}

/// The user's settings, owned by one entity installed as a global.
pub struct SettingsStore {
    settings: Settings,
    // None when there is no config folder; then nothing is ever written.
    path: Option<PathBuf>,
    writable: bool,
    generation: u64,
    // The generation last on disk, shared with the background writes.
    written: Arc<Mutex<u64>>,
    pending: Option<Task<()>>,
    failure_reported: bool,
    startup_warning: Option<String>,
}

impl EventEmitter<SettingsEvent> for SettingsStore {}

impl SettingsStore {
    /// The current settings.
    pub fn get(&self) -> &Settings {
        &self.settings
    }

    /// Applies `f`, notifies (the install observer then calls settings::apply), saves
    /// SAVE_DELAY later.
    pub fn change(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        self.generation += 1;
        if self.save_path().is_some() {
            let timer = cx.background_executor().timer(SAVE_DELAY);
            // Replacing the task cancels the previous countdown, so a burst costs one write.
            self.pending = Some(cx.spawn(async move |this, cx| {
                timer.await;
                let Ok(write) = this.update(cx, |s, cx| s.write(cx)) else {
                    return;
                };
                write.await;
            }));
        }
        cx.notify();
    }

    /// Every known field to its default; unknown fields kept; saved like a change.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.change(cx, Settings::reset_known);
    }

    /// Cancels the waiting save and writes a pending change now, on this thread; nothing when
    /// nothing is pending or the store is not writable. A failure emits SaveFailed (once).
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        let Some(path) = self.save_path() else {
            return;
        };
        let mut last = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *last >= self.generation {
            return;
        }
        match save_json(path, &self.settings) {
            Ok(()) => *last = self.generation,
            Err(e) => {
                drop(last);
                self.report(e.to_string(), cx);
            }
        }
    }

    /// Some("not saved: no config folder") without a path; Some("not saved: settings.json
    /// could not be read") when not writable.
    pub fn not_saved(&self) -> Option<&'static str> {
        if self.path.is_none() {
            Some("not saved: no config folder")
        } else if !self.writable {
            Some("not saved: settings.json could not be read")
        } else {
            None
        }
    }

    /// The load's recovered message, once.
    pub fn take_startup_warning(&mut self) -> Option<String> {
        self.startup_warning.take()
    }

    fn save_path(&self) -> Option<&PathBuf> {
        self.path.as_ref().filter(|_| self.writable)
    }

    fn report(&mut self, error: String, cx: &mut Context<Self>) {
        if !self.failure_reported {
            self.failure_reported = true;
            cx.emit(SettingsEvent::SaveFailed(error));
        }
    }

    fn write(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(path) = self.save_path().cloned() else {
            return Task::ready(());
        };
        let (value, generation, written) = (
            self.settings.clone(),
            self.generation,
            Arc::clone(&self.written),
        );
        // Writes serialise on the lock, and one no newer than the last landed write is skipped.
        let job = cx.background_spawn(async move {
            let mut last = written.lock().unwrap_or_else(|e| e.into_inner());
            if *last >= generation {
                return Ok(());
            }
            save_json(&path, &value).map(|()| *last = generation)
        });
        cx.spawn(async move |this, cx| {
            if let Err(e) = job.await {
                let _ = this.update(cx, |s, cx| s.report(e.to_string(), cx));
            }
        })
    }
}

// The installed store and the subscriptions that live as long as the app.
struct Installed {
    store: Entity<SettingsStore>,
    _subscriptions: [Subscription; 2],
}
impl Global for Installed {}

/// Loads `path` (None: in memory) with `seed`, sets the global, applies, observes, and registers
/// `cx.on_app_quit` calling `flush`. A second call returns the installed store unchanged.
pub fn install(path: Option<PathBuf>, seed: Appearance, cx: &mut App) -> Entity<SettingsStore> {
    if let Some(store) = store(cx) {
        return store;
    }
    let (settings, writable, startup_warning) = match &path {
        Some(path) => {
            let load = settings::load(path, seed);
            (load.settings, load.writable, load.recovered)
        }
        None => {
            let mut settings = Settings::default();
            settings.appearance = seed;
            (settings, true, None)
        }
    };
    let store = cx.new(|_| SettingsStore {
        settings,
        path,
        writable,
        generation: 0,
        written: Arc::new(Mutex::new(0)),
        pending: None,
        failure_reported: false,
        startup_warning,
    });
    let apply = |store: &Entity<SettingsStore>, cx: &mut App| {
        let current = store.read(cx).get().clone();
        settings::apply(&current, cx);
    };
    apply(&store, cx);
    let observe = cx.observe(&store, move |s, cx| apply(&s, cx));
    let quitting = store.clone();
    let quit = cx.on_app_quit(move |cx| {
        quitting.update(cx, |s, cx| s.flush(cx));
        async {}
    });
    cx.set_global(Installed {
        store: store.clone(),
        _subscriptions: [observe, quit],
    });
    store
}

/// The installed store, else `install(None, seed, cx)`.
pub fn ensure(seed: Appearance, cx: &mut App) -> Entity<SettingsStore> {
    install(None, seed, cx)
}

/// The installed store, if any.
pub fn store(cx: &App) -> Option<Entity<SettingsStore>> {
    cx.try_global::<Installed>().map(|i| i.store.clone())
}
