//! Watching a workspace folder for outside changes.
//!
//! `dbento.toml` is watched through a non-recursive watch on the folder and `queries/` through a
//! recursive one. A debounced batch asks for a reread when it touches either; everything else in
//! the folder (temp files, `.git`) is ignored, and a rescan notice always counts.

use notify_debouncer_full::notify::{
    Error as NotifyError, ErrorKind, EventKind, RecommendedWatcher, RecursiveMode,
};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

/// How long the watcher waits for a burst of file events to settle before reporting it.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// What the watcher reports about the workspace folder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatchEvent {
    /// `dbento.toml` or something under `queries/` changed; the workspace should be reread.
    Changed,
    /// Watching stopped for good; no further events follow.
    Failed {
        /// Why watching stopped, readable by a person.
        reason: String,
    },
}

/// Why a workspace folder could not be watched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WatchError {
    /// The folder, or `queries/` in it, could not be watched.
    #[error("cannot watch {}: {message}", path.display())]
    Io {
        /// The path that could not be watched.
        path: PathBuf,
        /// The underlying error.
        message: String,
    },
    /// The system's limit on watched files was reached.
    #[error("the system's file watch limit was reached: {message}")]
    Limit {
        /// The underlying error.
        message: String,
    },
}

enum Input {
    Events(DebounceEventResult),
    Stop,
}

/// Watches one workspace folder on a thread of its own until dropped.
#[derive(Debug)]
pub struct WorkspaceWatcher {
    inputs: Sender<Input>,
    stopped: Arc<AtomicBool>,
}

impl WorkspaceWatcher {
    /// Starts watching `dir`, calling `on_event` from the watcher's thread for every report.
    ///
    /// # Errors
    ///
    /// [`WatchError::Io`] when `dir` (or its `queries/`) cannot be watched, [`WatchError::Limit`]
    /// when the system's watch limit is reached.
    pub fn start(
        dir: &Path,
        on_event: impl Fn(WatchEvent) + Send + 'static,
    ) -> Result<WorkspaceWatcher, WatchError> {
        let root = std::fs::canonicalize(dir).map_err(|e| WatchError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        let queries = root.join("queries");
        let (inputs, rx) = channel();
        let events = inputs.clone();
        let mut debouncer = new_debouncer(WATCH_DEBOUNCE, None, move |r: DebounceEventResult| {
            let _ = events.send(Input::Events(r));
        })
        .map_err(|e| start_error(&root, &e))?;
        debouncer
            .watch(&root, RecursiveMode::NonRecursive)
            .map_err(|e| start_error(&root, &e))?;
        let mut placed = place(&mut debouncer, &queries)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let shown = root.clone();
        std::thread::Builder::new()
            .name("workspace-watcher".into())
            .spawn(move || {
                let emit = |e: WatchEvent| {
                    if !flag.load(Ordering::SeqCst) {
                        on_event(e);
                    }
                };
                // Ends on Stop or a failure; ending drops the debouncer and its watches.
                while let Ok(Input::Events(result)) = rx.recv() {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    let events = match result {
                        Ok(events) => events,
                        // Only the watch limit is fatal; other stream errors leave watching intact.
                        Err(errors) => match errors
                            .iter()
                            .find(|e| matches!(e.kind, ErrorKind::MaxFilesWatch))
                        {
                            Some(e) => {
                                return emit(WatchEvent::Failed {
                                    reason: e.to_string(),
                                });
                            }
                            None => continue,
                        },
                    };
                    if !root.is_dir() {
                        return emit(WatchEvent::Failed {
                            reason: "the workspace folder was removed or moved".to_owned(),
                        });
                    }
                    // A replaced queries/ leaves a dead watch, so an event naming it, or a folder
                    // that came or went, places the recursive watch afresh.
                    if touches(&events, &queries) || placed != queries.is_dir() {
                        // The old watch may already be gone with its folder (WatchNotFound).
                        if placed {
                            let _ = debouncer.unwatch(&queries);
                        }
                        placed = match place(&mut debouncer, &queries) {
                            Ok(p) => p,
                            Err(e) => {
                                return emit(WatchEvent::Failed {
                                    reason: e.to_string(),
                                });
                            }
                        };
                    }
                    if batch_counts(&root, &queries, &events) {
                        emit(WatchEvent::Changed);
                    }
                }
            })
            .map_err(|e| WatchError::Io {
                path: shown,
                message: e.to_string(),
            })?;
        Ok(WorkspaceWatcher { inputs, stopped })
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        // The flag silences the callback at once and `Stop` wakes the thread so it ends now.
        // The notify handler holds a sender too, so the channel never disconnects on its own.
        // There is no join, so dropping inside the callback cannot deadlock.
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.inputs.send(Input::Stop);
    }
}

// Places the recursive watch on queries/ when it exists; false means there is nothing to watch.
fn place(
    d: &mut Debouncer<RecommendedWatcher, RecommendedCache>,
    queries: &Path,
) -> Result<bool, WatchError> {
    if !queries.is_dir() {
        return Ok(false);
    }
    match d.watch(queries, RecursiveMode::Recursive) {
        Ok(()) => Ok(true),
        // Gone again between the check and the watch; the next batch tries again.
        Err(e) if gone(&e) => Ok(false),
        Err(e) => Err(start_error(queries, &e)),
    }
}

fn gone(e: &NotifyError) -> bool {
    matches!(e.kind, ErrorKind::PathNotFound)
        || matches!(&e.kind, ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}

fn start_error(path: &Path, e: &NotifyError) -> WatchError {
    if matches!(e.kind, ErrorKind::MaxFilesWatch) {
        WatchError::Limit {
            message: e.to_string(),
        }
    } else {
        WatchError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    }
}

fn touches(events: &[DebouncedEvent], queries: &Path) -> bool {
    events.iter().any(|e| e.paths.iter().any(|p| p == queries))
}

// A rescan notice, or any non-access event on dbento.toml, the queries entry or below it.
fn batch_counts(root: &Path, queries: &Path, events: &[DebouncedEvent]) -> bool {
    let toml = root.join("dbento.toml");
    events.iter().any(|e| {
        e.need_rescan()
            || (!matches!(e.kind, EventKind::Access(_))
                && e.paths.iter().any(|p| *p == toml || p.starts_with(queries)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_full::notify::event::{AccessKind, CreateKind, Event, Flag, ModifyKind};
    use std::time::Instant;

    fn ev(kind: EventKind, path: &Path) -> DebouncedEvent {
        DebouncedEvent::new(
            Event::new(kind).add_path(path.to_path_buf()),
            Instant::now(),
        )
    }

    #[test]
    fn which_batches_ask_for_a_reread() {
        let root = Path::new("/w");
        let q = root.join("queries");
        let counts = |e: DebouncedEvent| batch_counts(root, &q, &[e]);
        assert!(
            counts(ev(
                EventKind::Modify(ModifyKind::Any),
                &root.join("dbento.toml")
            )),
            "dbento.toml"
        );
        assert!(
            counts(ev(EventKind::Create(CreateKind::Folder), &q)),
            "queries itself"
        );
        assert!(
            counts(ev(EventKind::Create(CreateKind::File), &q.join("a/b.sql"))),
            "deep in queries"
        );
        assert!(
            !counts(ev(
                EventKind::Access(AccessKind::Any),
                &root.join("dbento.toml")
            )),
            "a read"
        );
        assert!(
            !counts(ev(
                EventKind::Create(CreateKind::File),
                &root.join(".dbento.toml.edit.tmp")
            )),
            "temp file"
        );
        assert!(
            !counts(ev(
                EventKind::Create(CreateKind::Folder),
                &root.join(".git")
            )),
            ".git"
        );
        assert!(
            !counts(ev(
                EventKind::Create(CreateKind::Folder),
                &root.join("queries2")
            )),
            "lookalike"
        );
        let rescan = DebouncedEvent::new(
            Event::new(EventKind::Other).set_flag(Flag::Rescan),
            Instant::now(),
        );
        assert!(
            batch_counts(root, &q, &[rescan]),
            "a path-less rescan notice"
        );
    }
}

#[cfg(test)]
mod measure {
    use notify_debouncer_full::notify::RecursiveMode;
    use notify_debouncer_full::{DebounceEventResult, new_debouncer};
    use std::path::Path;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    use super::WATCH_DEBOUNCE;

    fn batches(rx: &Receiver<DebounceEventResult>, root: &Path) {
        while let Ok(r) = rx.recv_timeout(Duration::from_millis(800)) {
            match r {
                Ok(events) => {
                    println!("  batch");
                    for e in events {
                        let paths: Vec<String> = e
                            .paths
                            .iter()
                            .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                            .collect();
                        let rescan = if e.need_rescan() { " rescan" } else { "" };
                        println!("    {:?} {paths:?}{rescan}", e.kind);
                    }
                }
                Err(errors) => println!("  errors {errors:?}"),
            }
        }
    }

    #[test]
    #[ignore = "a measurement: run with --ignored --nocapture and record the output"]
    fn measure_recursive_queries_watch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let queries = root.join("queries");
        std::fs::create_dir_all(queries.join("lite")).unwrap();
        std::fs::write(root.join("dbento.toml"), "dbento = 1\n").unwrap();
        let (tx, rx) = channel();
        let mut d = new_debouncer(WATCH_DEBOUNCE, None, move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
        d.watch(&root, RecursiveMode::NonRecursive).unwrap();
        d.watch(&queries, RecursiveMode::Recursive).unwrap();
        let step = |what: &str| println!("{what}");

        step("new subfolder with a file, written at once");
        std::fs::create_dir_all(queries.join("new/sub")).unwrap();
        std::fs::write(queries.join("new/sub/a.sql"), "SELECT 1").unwrap();
        batches(&rx, &root);
        step("second write in the new subfolder");
        std::fs::write(queries.join("new/sub/a.sql"), "SELECT 2").unwrap();
        batches(&rx, &root);
        step("queries renamed away, a new one made with a file");
        std::fs::rename(&queries, root.join("old-queries")).unwrap();
        std::fs::create_dir_all(queries.join("lite")).unwrap();
        std::fs::write(queries.join("lite/b.sql"), "SELECT 3").unwrap();
        batches(&rx, &root);
        step("write in the new queries before re-placing");
        std::fs::write(queries.join("lite/b.sql"), "SELECT 4").unwrap();
        batches(&rx, &root);
        step("write in the moved-away folder");
        std::fs::write(root.join("old-queries/lite/x.sql"), "SELECT 5").unwrap();
        batches(&rx, &root);
        println!("unwatch {:?}", d.unwatch(&queries));
        println!("watch {:?}", d.watch(&queries, RecursiveMode::Recursive));
        step("write in the new queries after re-placing");
        std::fs::write(queries.join("lite/b.sql"), "SELECT 6").unwrap();
        batches(&rx, &root);
        step("write in the moved-away folder after re-placing");
        std::fs::write(root.join("old-queries/lite/x.sql"), "SELECT 7").unwrap();
        batches(&rx, &root);
        step("queries removed, then recreated with a file");
        std::fs::remove_dir_all(&queries).unwrap();
        batches(&rx, &root);
        std::fs::create_dir_all(queries.join("lite")).unwrap();
        std::fs::write(queries.join("lite/c.sql"), "SELECT 8").unwrap();
        batches(&rx, &root);
        step("dbento.toml saved by rename-over");
        std::fs::write(root.join(".dbento.toml.swp"), "dbento = 1\n# x\n").unwrap();
        std::fs::rename(root.join(".dbento.toml.swp"), root.join("dbento.toml")).unwrap();
        batches(&rx, &root);
        step("siblings: temp file, .git, its HEAD");
        std::fs::write(root.join(".dbento.toml.edit.tmp"), "x").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref").unwrap();
        batches(&rx, &root);
    }
}
