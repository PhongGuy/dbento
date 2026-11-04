//! A tokio runtime owned by the GPUI app, so tokio-dependent futures (database drivers, timers)
//! can run from GPUI code and be awaited as GPUI tasks.
//!
//! Adapted and modified from Zed's gpui_tokio (crates/gpui_tokio/src/gpui_tokio.rs),
//! Apache-2.0, Copyright Zed Industries.

use std::future::Future;

use gpui_kit::{App, AppContext, Global, Task};
use tokio::runtime::{Handle, Runtime};
use tokio::task::AbortHandle;

pub use tokio::task::JoinError;

/// Starts a two-worker tokio runtime and stores it as an app global.
pub fn init(cx: &mut App) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to start the tokio runtime");
    let handle = runtime.handle().clone();
    cx.set_global(GlobalTokio {
        runtime: Some(runtime),
        handle,
    });
}

/// Runs `future` on the tokio runtime and returns a GPUI task resolving to its output.
/// Dropping the returned task aborts the tokio task. Panics if [`init`] was not called.
pub fn spawn<C, F, R>(cx: &C, future: F) -> Task<Result<R, JoinError>>
where
    C: AppContext,
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    cx.read_global(|tokio: &GlobalTokio, cx| {
        let join = tokio.handle.spawn(future);
        let guard = AbortOnDrop(join.abort_handle());
        // The guard lives inside the GPUI future, so cancelling that task aborts the
        // tokio one; after a normal join, aborting a finished task is a no-op.
        cx.background_spawn(async move {
            let result = join.await;
            drop(guard);
            result
        })
    })
}

/// A handle to the app's tokio runtime. Panics if [`init`] was not called.
pub fn handle(cx: &App) -> Handle {
    cx.global::<GlobalTokio>().handle.clone()
}

struct GlobalTokio {
    runtime: Option<Runtime>,
    handle: Handle,
}

impl Global for GlobalTokio {}

impl Drop for GlobalTokio {
    // Dropping a runtime blocks on its tasks; at app teardown, abandon them instead.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
