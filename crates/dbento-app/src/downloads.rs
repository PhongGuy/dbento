//! Object downloads for one window: each runs on its own session, reports its bytes, and can
//! be cancelled only while it runs, so a press never outlives the download it was meant for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use dbento_core::{
    CancelHandle, DEFAULT_MAX_DOWNLOAD, DownloadError, DownloadOptions, DownloadProgress,
    ObjectKey, SessionOptions,
};
use futures_util::future::{Either, select};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Task, TestSupportExt as _, px,
};

use crate::objects::{object_error_text, size_words};
use crate::runner::{self, RunnerSpec};
use crate::update_banner::label;
use crate::{theme, tokio_bridge, ui};

/// How often the strip redraws while a download runs, so its progress moves.
const TICK: Duration = Duration::from_millis(200);

/// A download's place in its window's list: 0, 1, 2 in start order.
pub type DownloadId = usize;

/// Where a download stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    /// Its session is opening.
    Connecting,
    /// Bytes are arriving.
    Running,
    /// The file landed with `bytes` bytes.
    Done {
        /// The bytes written.
        bytes: u64,
    },
    /// It stopped, for the reason shown.
    Failed(String),
    /// The user or the window close stopped it.
    Cancelled,
    /// The object is over the size cap; "Download anyway" retries at its size.
    TooLarge {
        /// The object's size in bytes.
        size: u64,
        /// The cap it went over.
        limit: u64,
    },
}

impl DownloadState {
    fn is_live(&self) -> bool {
        matches!(self, DownloadState::Connecting | DownloadState::Running)
    }
}

/// What to download, where to, and under which limits.
#[derive(Clone)]
pub struct DownloadRequest {
    /// The connection the download opens its own session on.
    pub spec: RunnerSpec,
    /// The object to fetch.
    pub key: ObjectKey,
    /// The file to write.
    pub dest: PathBuf,
    /// Replace an existing file; set only when the user chose to.
    pub force: bool,
    /// The largest object accepted, in bytes.
    pub max_size: u64,
}

impl DownloadRequest {
    /// A download that refuses an existing file and objects over the default cap.
    pub fn new(spec: RunnerSpec, key: ObjectKey, dest: PathBuf) -> Self {
        DownloadRequest {
            spec,
            key,
            dest,
            force: false,
            max_size: DEFAULT_MAX_DOWNLOAD,
        }
    }
}

/// One download's shared state between its GPUI item and its tokio task.
#[derive(Default)]
struct Link {
    /// The session's cancel handle once it opened; kept after the download ends.
    handle: Mutex<Option<CancelHandle>>,
}

impl Link {
    fn handle(&self) -> Option<CancelHandle> {
        self.handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct Item {
    req: DownloadRequest,
    state: DownloadState,
    progress: DownloadProgress,
    link: Arc<Link>,
    /// Closed by `cancel`; the task watches it while the session opens.
    stop: Sender<()>,
    /// Closes when the task has ended and its session is gone.
    ended: Receiver<()>,
    _task: Task<()>,
}

/// The window's downloads, in start order, with the strip that shows them.
#[derive(Default)]
pub struct Downloads {
    items: Vec<Option<Item>>,
    ticker: Option<Task<()>>,
}

/// A wait for every download `close_all` cancelled to end.
pub struct DownloadsClosing(Vec<Receiver<()>>);

impl DownloadsClosing {
    /// Resolves once each cancelled download's task has ended.
    pub async fn wait(self) {
        for ended in self.0 {
            // Nothing is ever sent; the channel closes when the task drops its end.
            let _ = ended.recv().await;
        }
    }
}

impl Downloads {
    /// No downloads yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `req` on a session of its own and returns its id.
    pub fn start(&mut self, req: DownloadRequest, cx: &mut Context<Self>) -> DownloadId {
        let id = self.items.len();
        let progress = DownloadProgress::new();
        let link = Arc::new(Link::default());
        let (stop, stopped) = async_channel::bounded::<()>(1);
        let (ended_tx, ended) = async_channel::bounded::<()>(1);
        let (opened_tx, opened) = async_channel::bounded::<()>(1);
        let work = download(
            req.clone(),
            progress.clone(),
            Arc::clone(&link),
            stopped,
            opened_tx,
        );
        let job = tokio_bridge::spawn(cx, work);
        let task = cx.spawn(async move |this, cx| {
            // The sender drops unsent when the open failed or was stopped.
            if opened.recv().await.is_ok() {
                let _ = this.update(cx, |d, cx| d.mark_running(id, cx));
            }
            let outcome = job.await.unwrap_or_else(|e| {
                DownloadState::Failed(format!("the download stopped unexpectedly: {e}"))
            });
            let _ = this.update(cx, |d, cx| d.finish(id, outcome, cx));
            drop(ended_tx);
        });
        self.items.push(Some(Item {
            req,
            state: DownloadState::Connecting,
            progress,
            link,
            stop,
            ended,
            _task: task,
        }));
        self.tick(cx);
        cx.notify();
        id
    }

    fn item(&self, id: DownloadId) -> Option<&Item> {
        self.items.get(id).and_then(Option::as_ref)
    }

    /// Where download `id` stands; None once dismissed or for an unknown id.
    pub fn state(&self, id: DownloadId) -> Option<DownloadState> {
        self.item(id).map(|i| i.state.clone())
    }

    /// Bytes written and the object's size once known.
    pub fn progress(&self, id: DownloadId) -> Option<(u64, Option<u64>)> {
        self.item(id)
            .map(|i| (i.progress.written(), i.progress.total()))
    }

    /// Stops a download that is connecting or running; false for any other.
    pub fn cancel(&mut self, id: DownloadId, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.items.get_mut(id).and_then(Option::as_mut) else {
            return false;
        };
        if !item.state.is_live() {
            return false;
        }
        // Close the stop channel before reading the handle: the task stores the handle and then
        // checks the channel, so one side always sees the other.
        item.stop.close();
        match item.link.handle() {
            Some(handle) => handle.cancel(),
            // Still connecting: the task drops the open, so it is over now.
            None => item.state = DownloadState::Cancelled,
        }
        cx.notify();
        true
    }

    /// Restarts a download refused as too large, once, with the cap at the object's size.
    pub fn download_anyway(
        &mut self,
        id: DownloadId,
        cx: &mut Context<Self>,
    ) -> Option<DownloadId> {
        let item = self.item(id)?;
        let DownloadState::TooLarge { size, .. } = item.state else {
            return None;
        };
        let mut req = item.req.clone();
        req.max_size = size;
        if let Some(slot) = self.items.get_mut(id) {
            *slot = None;
        }
        Some(self.start(req, cx))
    }

    /// Removes an ended download from the strip.
    pub fn dismiss(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        if let Some(slot) = self.items.get_mut(id)
            && slot.as_ref().is_some_and(|i| !i.state.is_live())
        {
            *slot = None;
            cx.notify();
        }
    }

    /// The core cancel handle of the download's session, kept after it ends.
    #[doc(hidden)]
    pub fn cancel_handle(&self, id: DownloadId) -> Option<CancelHandle> {
        self.item(id).and_then(|i| i.link.handle())
    }

    /// File names of the downloads still connecting or running.
    pub fn running(&self) -> Vec<String> {
        self.live().map(|i| file_name(&i.req.dest)).collect()
    }

    /// Cancels every running download; the returned wait ends once each has stopped.
    pub fn close_all(&mut self, cx: &mut Context<Self>) -> DownloadsClosing {
        let ids: Vec<DownloadId> = (0..self.items.len())
            .filter(|id| self.item(*id).is_some_and(|i| i.state.is_live()))
            .collect();
        let ended = ids
            .into_iter()
            .filter_map(|id| {
                self.cancel(id, cx);
                self.item(id).map(|i| i.ended.clone())
            })
            .collect();
        DownloadsClosing(ended)
    }

    fn live(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().flatten().filter(|i| i.state.is_live())
    }

    fn finish(&mut self, id: DownloadId, outcome: DownloadState, cx: &mut Context<Self>) {
        // A cancel during connect already ended the item; the task's late word changes nothing.
        if let Some(item) = self.items.get_mut(id).and_then(Option::as_mut)
            && item.state.is_live()
        {
            item.state = outcome;
            cx.notify();
        }
    }

    fn mark_running(&mut self, id: DownloadId, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get_mut(id).and_then(Option::as_mut)
            && item.state == DownloadState::Connecting
        {
            item.state = DownloadState::Running;
            cx.notify();
        }
    }

    /// Redraws on a timer while anything runs, so progress moves.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let more = this.update(cx, |d, cx| {
                    let more = d.live().next().is_some();
                    if more {
                        cx.notify();
                    } else {
                        d.ticker = None;
                    }
                    more
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        }));
    }
}

/// The tokio side of one download: open a session (dropped on a stop), publish its cancel
/// handle and say it opened, then download under it. The session drops when this returns.
async fn download(
    req: DownloadRequest,
    progress: DownloadProgress,
    link: Arc<Link>,
    stopped: Receiver<()>,
    opened: Sender<()>,
) -> DownloadState {
    let open = Box::pin(runner::open(&req.spec, |_| SessionOptions::default()));
    // `select` polls the stop first, so a cancel already pressed wins over a finished open.
    let mut session = match select(Box::pin(stopped.recv()), open).await {
        Either::Left(_) => return DownloadState::Cancelled,
        Either::Right((Err(message), _)) => return DownloadState::Failed(message),
        Either::Right((Ok(s), _)) => s,
    };
    let handle = session.cancel_handle();
    *link.handle.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle.clone());
    let _ = opened.try_send(());
    // A cancel pressed after the open finished but before the handle was stored reaches the
    // session here, and its guard fails the download before any request.
    if stopped.is_closed() {
        handle.cancel();
    }
    let opts = DownloadOptions::default()
        .with_progress(progress)
        .with_force(req.force)
        .with_max_size(req.max_size);
    match session.download_object(&req.key, &req.dest, opts).await {
        Ok(report) => DownloadState::Done {
            bytes: report.bytes,
        },
        Err(DownloadError::Cancelled) => DownloadState::Cancelled,
        Err(DownloadError::TooLarge { size, limit }) => DownloadState::TooLarge { size, limit },
        Err(e) => DownloadState::Failed(download_error_text(&e)),
    }
}

fn file_name(dest: &Path) -> String {
    dest.file_name().map_or_else(
        || dest.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// A download error as the app shows it: store texts escaped, sizes in words.
pub fn download_error_text(e: &DownloadError) -> String {
    match e {
        DownloadError::Remote(remote) => object_error_text(remote),
        DownloadError::TooLarge { size, limit } => format!(
            "the object is {}, over the {} limit",
            size_words(*size),
            size_words(*limit)
        ),
        DownloadError::Exists
        | DownloadError::Destination(_)
        | DownloadError::Truncated
        | DownloadError::Cancelled
        | DownloadError::Timeout
        | _ => e.to_string(),
    }
}

/// Bytes written, and of how many with the share done once the size is known.
pub fn progress_text(written: u64, total: Option<u64>) -> String {
    match total {
        None => size_words(written),
        Some(total) => {
            let percent = written
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100);
            format!(
                "{} of {} ({percent}%)",
                size_words(written),
                size_words(total)
            )
        }
    }
}

/// Whether saving to `dest` replaces something there, which the save dialog asked about.
pub fn replace_existing(dest: &Path) -> bool {
    dest.symlink_metadata().is_ok()
}

/// The folder the save dialog opens in: the last download's, else the downloads folder,
/// else home.
pub fn download_dir(
    last: Option<&Path>,
    downloads: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    last.or(downloads).or(home).map(Path::to_path_buf)
}

fn state_words(state: &DownloadState, written: u64, total: Option<u64>) -> String {
    match state {
        DownloadState::Connecting => "connecting".to_owned(),
        DownloadState::Running => progress_text(written, total),
        DownloadState::Done { bytes } => format!("done, {}", size_words(*bytes)),
        DownloadState::Failed(why) => format!("failed: {why}"),
        DownloadState::Cancelled => "cancelled".to_owned(),
        DownloadState::TooLarge { size, limit } => download_error_text(&DownloadError::TooLarge {
            size: *size,
            limit: *limit,
        }),
    }
}

/// The strip of downloads beside the status bar; None when there are none.
pub fn downloads_strip(downloads: &Entity<Downloads>, cx: &App) -> Option<AnyElement> {
    let d = downloads.read(cx);
    let rows: Vec<AnyElement> = d
        .items
        .iter()
        .enumerate()
        .filter_map(|(id, item)| item.as_ref().map(|i| download_row(downloads, id, i, cx)))
        .collect();
    if rows.is_empty() {
        return None;
    }
    let strip = h_flex()
        .id("downloads")
        .test_support()
        .flex_wrap()
        .flex_none()
        .gap_3()
        .px_3()
        .py_1()
        .min_h(px(28.))
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .text_sm()
        .children(rows);
    Some(strip.into_any_element())
}

fn download_row(
    downloads: &Entity<Downloads>,
    id: DownloadId,
    item: &Item,
    cx: &App,
) -> AnyElement {
    let (written, total) = (item.progress.written(), item.progress.total());
    let words = state_words(&item.state, written, total);
    let color = match item.state {
        DownloadState::Failed(_) | DownloadState::TooLarge { .. } => cx.theme().danger,
        DownloadState::Connecting
        | DownloadState::Running
        | DownloadState::Done { .. }
        | DownloadState::Cancelled => cx.theme().foreground,
    };
    let says = format!("{} · {words}", file_name(&item.req.dest));
    let mut row = h_flex().gap_2().child(label(("download", id), says, color));
    if item.state == DownloadState::Running {
        let percent = total
            .and_then(|t| written.saturating_mul(100).checked_div(t))
            .map_or_else(|| "…".to_owned(), |p| format!("{p}%"));
        row = row.child(label(("download-progress", id), percent, theme::dim(cx)));
    }
    let button = |name: &'static str, text: &'static str| {
        ui::secondary(Button::new((name, id))).small().label(text)
    };
    if item.state.is_live() {
        let d = downloads.clone();
        row = row.child(
            button("download-cancel", "Cancel").on_click(move |_, _, cx| {
                d.update(cx, |d, cx| d.cancel(id, cx));
            }),
        );
    }
    if matches!(item.state, DownloadState::TooLarge { .. }) {
        let d = downloads.clone();
        row = row.child(
            button("download-anyway", "Download anyway").on_click(move |_, _, cx| {
                d.update(cx, |d, cx| d.download_anyway(id, cx));
            }),
        );
    }
    if !item.state.is_live() {
        let d = downloads.clone();
        row = row.child(
            button("download-dismiss", "Dismiss").on_click(move |_, _, cx| {
                d.update(cx, |d, cx| d.dismiss(id, cx));
            }),
        );
    }
    row.into_any_element()
}
