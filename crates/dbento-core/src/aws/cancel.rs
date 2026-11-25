//! A cancel handle both AWS engines share: a call guarded by it stops when the session cancels.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

use crate::session::ExecuteError;

/// Cancels by generation: `cancel` bumps it, a delivered cancel or `spend` spends it.
#[derive(Debug)]
pub(crate) struct AwsCancel {
    generation: watch::Sender<u64>,
    spent: AtomicU64,
}

impl AwsCancel {
    pub(crate) fn new() -> Self {
        AwsCancel {
            generation: watch::Sender::new(0),
            spent: AtomicU64::new(0),
        }
    }

    pub(crate) fn cancel(&self) {
        // `send_modify` counts the cancel even with no statement listening.
        self.generation.send_modify(|g| *g += 1);
    }

    pub(crate) fn spend(&self) {
        let generation = *self.generation.borrow();
        self.spent.store(generation, Ordering::Relaxed);
    }

    /// `Err(Cancelled)` at once for an unspent cancel, or when one arrives while `f` runs, which
    /// drops `f`.
    pub(crate) async fn guard<F: Future>(&self, f: F) -> Result<F::Output, ExecuteError> {
        let mut generation = self.generation.subscribe();
        // Discards `wait_for`'s reply here: it holds the channel's lock, which `spend` needs.
        let cancelled = async {
            let unspent = |g: &u64| *g != self.spent.load(Ordering::Relaxed);
            let _ = generation.wait_for(unspent).await;
        };
        // Biased so an unspent cancel wins before `f` is first polled, and nothing is sent.
        tokio::select! {
            biased;
            () = cancelled => {
                self.spend();
                Err(ExecuteError::Cancelled)
            }
            out = f => Ok(out),
        }
    }
}
