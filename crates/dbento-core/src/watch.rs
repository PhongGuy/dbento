//! Watching a workspace folder for outside changes.
//!
//! `dbento.toml` is watched through a non-recursive watch on the folder and `queries/` through a
//! recursive one. A debounced batch asks for a reread when it touches either; everything else in
//! the folder (temp files, `.git`) is ignored, and a rescan notice always counts.

use notify_debouncer_full::DebouncedEvent;
use notify_debouncer_full::notify::EventKind;
use std::path::Path;

// A rescan notice, or any non-access event on dbento.toml, the queries entry or below it.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the watcher thread is its first caller")
)]
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

    const WATCH_DEBOUNCE: Duration = Duration::from_millis(250);

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
