//! A Redis stream's message tab: which read goes next, where Follow stands, the ring caps and
//! the groups panel's texts.

use std::time::Duration;

use dbento_core::message::{Direction, MessageCursor, MessageRead, Position, SourceRef};

/// How many messages one page asks for.
pub const MESSAGE_PAGE: u32 = 500;
/// The cell-byte bound every read carries, so one page or poll stays near this size.
pub const PAGE_BYTES: u64 = 4 << 20;
/// How long one Follow poll waits for new messages.
pub const FOLLOW_WAIT: Duration = Duration::from_secs(1);
/// The most rows a followed tab keeps; provisional until render cost and RSS are measured.
pub const RING_ROWS: u64 = 100_000;
/// The most result heap a followed tab keeps; provisional like `RING_ROWS`.
pub const RING_BYTES: usize = 256 << 20;

/// Whether new messages are polled: `Paused` keeps rows and position but sends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Not following.
    Off,
    /// Polling for new messages.
    On,
    /// Following, but holding polls.
    Paused,
}

/// A side panel's load state.
#[derive(Debug, Clone, PartialEq)]
pub enum Panel<T> {
    /// The read is in flight.
    Loading,
    /// The read landed.
    Ready(T),
    /// The read failed with this message.
    Failed(String),
}

/// One consumer group as the groups panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupRow {
    /// The group's name.
    pub name: String,
    /// The row's text, from `group_text`.
    pub text: String,
}

/// Words `n` with its unit, singular for one.
fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "g · 1 consumer · 2 pending · lag 0"; no lag part when `lag` is None.
pub fn group_text(name: &str, consumers: u64, pending: u64, lag: Option<u64>) -> String {
    let mut parts = vec![
        name.to_owned(),
        count(consumers, "consumer", "consumers"),
        format!("{pending} pending"),
    ];
    if let Some(lag) = lag {
        parts.push(format!("lag {lag}"));
    }
    parts.join(" \u{B7} ")
}

/// "1-1 · c1 · idle 1234 ms · 1 delivery".
pub fn pending_text(id: &str, consumer: &str, idle_ms: u64, deliveries: u64) -> String {
    format!(
        "{id} \u{B7} {consumer} \u{B7} idle {idle_ms} ms \u{B7} {}",
        count(deliveries, "delivery", "deliveries")
    )
}

/// "1 older dropped", "5 older dropped".
pub fn dropped_text(n: u64) -> String {
    format!("{n} older dropped")
}

/// One stream read page by page from its newest end, plus Follow: browses page back from
/// `prev`, polls read forward from `newest`, and only one read of either kind is in flight.
#[derive(Debug, Clone)]
pub struct MessageView {
    db: u32,
    key: Vec<u8>,
    newest: Option<MessageCursor>,
    prev: Option<MessageCursor>,
    follow: Follow,
    reading: bool,
    polling: bool,
    wait: Duration,
    dropped: u64,
    error: Option<String>,
    groups: Option<Panel<Vec<GroupRow>>>,
    pending: Option<(String, Panel<Vec<String>>)>,
}

impl MessageView {
    /// A view of stream `key` in DB `db` with nothing read and Follow off.
    pub fn new(db: u32, key: Vec<u8>) -> Self {
        MessageView {
            db,
            key,
            newest: None,
            prev: None,
            follow: Follow::Off,
            reading: false,
            polling: false,
            wait: FOLLOW_WAIT,
            dropped: 0,
            error: None,
            groups: None,
            pending: None,
        }
    }

    fn source(&self) -> SourceRef {
        SourceRef::redis(self.db, self.key.clone())
    }

    fn sized(mut read: MessageRead) -> MessageRead {
        read.want = MESSAGE_PAGE;
        read.max_bytes = Some(PAGE_BYTES);
        read
    }

    /// The newest page, read backward from the stream's end.
    pub fn newest_read(&self) -> MessageRead {
        Self::sized(MessageRead::browse(
            self.source(),
            Position::End,
            Direction::Backward,
        ))
    }

    /// The page before the oldest row shown; None while a browse or poll is in flight or none
    /// is older, so a browse never queues behind a poll's wait.
    pub fn older_read(&self) -> Option<MessageRead> {
        if self.reading || self.polling || !self.has_older() {
            return None;
        }
        let prev = self.prev.clone()?;
        Some(Self::sized(MessageRead::browse(
            self.source(),
            Position::Before(prev),
            Direction::Backward,
        )))
    }

    /// The next poll, forward from the newest row seen; only with Follow on and nothing in flight.
    pub fn poll_read(&self) -> Option<MessageRead> {
        if self.follow != Follow::On || self.reading || self.polling {
            return None;
        }
        let from = self.newest.clone().map_or(Position::End, Position::After);
        Some(Self::sized(MessageRead::follow(
            self.source(),
            from,
            self.wait,
        )))
    }

    /// Marks a browse as in flight.
    pub fn begin_read(&mut self) {
        self.reading = true;
        self.error = None;
    }

    /// Marks a poll as in flight.
    pub fn begin_poll(&mut self) {
        self.polling = true;
        self.error = None;
    }

    /// Takes a landed browse page's cursors: the first page sets `newest` and, as it replaces
    /// every row, forgets the ring's drops and a shortened wait; a last page ends paging back,
    /// and a page without `next` never clears `newest`.
    pub fn browsed(
        &mut self,
        next: Option<MessageCursor>,
        prev: Option<MessageCursor>,
        first: bool,
        last: bool,
    ) {
        if first {
            self.dropped = 0;
            self.wait = FOLLOW_WAIT;
            if next.is_some() {
                self.newest = next;
            }
        }
        self.prev = if last { None } else { prev };
        self.reading = false;
    }

    /// Takes a landed poll whose rows were shown.
    pub fn polled(&mut self, next: Option<MessageCursor>) {
        if next.is_some() {
            self.newest = next;
        }
        self.polling = false;
    }

    /// Ends a poll that landed unseen; `newest` stays so the next poll reads the same entries.
    pub fn poll_dropped(&mut self) {
        self.polling = false;
    }

    /// Ends a browse that landed unseen; its cursors are not taken.
    pub fn read_dropped(&mut self) {
        self.reading = false;
    }

    /// Ends the read in flight with `message`; Follow is left as it was.
    pub fn fail(&mut self, message: String) {
        self.reading = false;
        self.polling = false;
        self.error = Some(message);
    }

    /// Turns Follow off and clears any read in flight, with an optional reason.
    pub fn stop(&mut self, message: Option<String>) {
        self.follow = Follow::Off;
        self.reading = false;
        self.polling = false;
        self.error = message;
    }

    /// Sets Follow.
    pub fn set_follow(&mut self, follow: Follow) {
        self.follow = follow;
    }

    /// Takes a refusal of the wait as over `max`: a cap of at least 1 ms becomes the wait
    /// and true; a smaller one stops Follow.
    pub fn wait_too_long(&mut self, max: Duration) -> bool {
        self.polling = false;
        if max >= Duration::from_millis(1) {
            self.wait = max;
            true
        } else {
            self.stop(Some(
                "the session's statement timeout leaves no time to wait for new messages"
                    .to_owned(),
            ));
            false
        }
    }

    /// Counts rows the ring dropped; once any are gone, paging back ends.
    pub fn note_dropped(&mut self, n: u64) {
        self.dropped += n;
    }

    /// Whether an older page can be read: one is known and the ring dropped nothing.
    pub fn has_older(&self) -> bool {
        self.prev.is_some() && self.dropped == 0
    }

    /// The DB index the stream lives in.
    pub fn db(&self) -> u32 {
        self.db
    }

    /// The stream's key, as raw bytes.
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// The newest entry seen, where the next poll starts after.
    pub fn newest(&self) -> Option<&MessageCursor> {
        self.newest.as_ref()
    }

    /// Where Follow stands.
    pub fn follow(&self) -> Follow {
        self.follow
    }

    /// Whether a browse is in flight.
    pub fn reading(&self) -> bool {
        self.reading
    }

    /// Whether a poll is in flight.
    pub fn polling(&self) -> bool {
        self.polling
    }

    /// How long each poll waits.
    pub fn wait(&self) -> Duration {
        self.wait
    }

    /// How many older rows the ring has dropped.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Why the last read failed or Follow stopped, until the next read begins.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The groups panel, once opened.
    pub fn groups(&self) -> Option<&Panel<Vec<GroupRow>>> {
        self.groups.as_ref()
    }

    /// Opens, refreshes or closes the groups panel; closing it closes the pending list too.
    pub fn set_groups(&mut self, groups: Option<Panel<Vec<GroupRow>>>) {
        if groups.is_none() {
            self.pending = None;
        }
        self.groups = groups;
    }

    /// The group whose pending entries are listed, and the list.
    pub fn pending(&self) -> Option<(&str, &Panel<Vec<String>>)> {
        self.pending.as_ref().map(|(g, rows)| (g.as_str(), rows))
    }

    /// Lists `group`'s pending entries.
    pub fn set_pending(&mut self, group: String, rows: Panel<Vec<String>>) {
        self.pending = Some((group, rows));
    }
}
