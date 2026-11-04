//! Ctrl-C handling: the first press cancels the running statement, a later one exits.

use std::io;
use std::sync::{Arc, Mutex};

use dbento_core::CancelHandle;
use tokio::sync::watch;

/// What one Ctrl-C press does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Press {
    Cancel,
    Exit,
}

/// The effect of a press after `before` earlier presses.
pub(crate) fn press(before: u32) -> Press {
    if before == 0 {
        Press::Cancel
    } else {
        Press::Exit
    }
}

/// Whether the run was interrupted; cloning shares the same state.
#[derive(Clone)]
pub(crate) struct Interrupt {
    stopped: watch::Receiver<bool>,
    /// The statement a press cancels; empty until a session exists.
    target: Arc<Mutex<Option<CancelHandle>>>,
}

impl Interrupt {
    /// Takes over Ctrl-C for the rest of the process. The handler is registered before this
    /// returns, so no early press reaches the default handler, which would kill the process.
    /// It listens on its own thread and runtime, so a blocked write cannot delay an exit.
    /// Install it before connecting, then [`Interrupt::arm`] it with the session's handle.
    pub(crate) fn install() -> io::Result<Interrupt> {
        let (tx, rx) = watch::channel(false);
        let target: Arc<Mutex<Option<CancelHandle>>> = Arc::default();
        let armed = Arc::clone(&target);
        let (registered, outcome) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("dbento-ctrl-c".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => return drop(registered.send(Err(e))),
                };
                let _entered = runtime.enter();
                #[cfg(unix)]
                let signal =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
                #[cfg(windows)]
                let signal = tokio::signal::windows::ctrl_c();
                let mut signal = match signal {
                    Ok(signal) => signal,
                    Err(e) => return drop(registered.send(Err(e))),
                };
                let _ = registered.send(Ok(()));
                runtime.block_on(async move {
                    let mut before = 0u32;
                    while signal.recv().await.is_some() {
                        match press(before) {
                            Press::Cancel => {
                                tx.send_replace(true);
                                if let Some(cancel) = lock(&armed).as_ref() {
                                    cancel.cancel();
                                }
                                eprintln!("cancelling; press Ctrl-C again to exit");
                            }
                            Press::Exit => std::process::exit(1),
                        }
                        before = before.saturating_add(1);
                    }
                });
            })?;
        outcome
            .recv()
            .map_err(|_| io::Error::other("the Ctrl-C thread ended before registering"))??;
        Ok(Interrupt {
            stopped: rx,
            target,
        })
    }

    /// Makes later presses cancel through `cancel`. A press before this only marks the
    /// run stopped, which the statement loop checks before it starts anything.
    pub(crate) fn arm(&self, cancel: CancelHandle) {
        *lock(&self.target) = Some(cancel);
    }

    /// True once the first press happened.
    pub(crate) fn stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    /// Returns once the first press happened, at once if it already did.
    pub(crate) async fn wait(&self) {
        let mut rx = self.stopped.clone();
        if rx.wait_for(|s| *s).await.is_err() {
            // The handler is gone, so no press can come any more.
            std::future::pending::<()>().await;
        }
    }
}

/// A poisoned lock still holds a usable handle; a press must never panic over it.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_press_cancels_and_any_later_press_exits() {
        assert_eq!(press(0), Press::Cancel);
        assert_eq!(press(1), Press::Exit);
        assert_eq!(press(7), Press::Exit);
    }
}
