//! The server's notice stream: a server-sent events parser, the reconnect backoff and the
//! reader thread that ties them together.

use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::account::TeamToken;
use super::api::{ApiError, PotluckClient};

/// The longest event line accepted; a longer one ends the stream as an error.
pub(crate) const LINE_CAP: usize = 64 * 1024;

/// What the reader tells the driver about the server's notice stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    /// The stream opened.
    Connected,
    /// The server is still there: a keep-alive or a block that names no ref.
    Alive,
    /// A branch of the repository now points at `sha`.
    Ref { git_ref: String, sha: String },
    /// The stream ended or failed; the reader is backing off.
    Down,
    /// The server refused the token; the reader has stopped.
    SignedOut,
}

/// The payload of an `event: ref` block.
#[derive(Deserialize)]
struct RefData {
    #[serde(rename = "ref")]
    git_ref: String,
    sha: String,
}

/// Parse an event stream until EOF, handing each notice to `emit`; stop as soon as it returns false.
///
/// Comment lines emit `Alive` at once; a blank line closes a block that had a field line, which
/// emits `Ref` for a well-formed `ref` event and `Alive` otherwise. A line over [`LINE_CAP`] is
/// `InvalidData`.
pub(crate) fn read_stream(
    input: &mut dyn BufRead,
    emit: &mut dyn FnMut(Notice) -> bool,
) -> io::Result<()> {
    let mut line = Vec::new();
    let mut event = String::new();
    let mut data = String::new();
    let mut in_block = false;
    loop {
        // Read at most one byte past the cap, so an endless line fails without buffering it all.
        line.clear();
        let read = (&mut *input)
            .take(LINE_CAP as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > LINE_CAP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "event line too long",
            ));
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = String::from_utf8_lossy(&line);

        let notice = if text.is_empty() {
            if !in_block {
                continue;
            }
            let notice = block_notice(&event, &data);
            event.clear();
            data.clear();
            in_block = false;
            notice
        } else if text.starts_with(':') {
            Notice::Alive
        } else {
            let (name, value) = text.split_once(':').unwrap_or((&text, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match name {
                "event" => value.clone_into(&mut event),
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
                _ => {}
            }
            in_block = true;
            continue;
        };
        if !emit(notice) {
            return Ok(());
        }
    }
}

/// A closed block is a `Ref` only when it is a `ref` event whose data names a ref and a sha.
fn block_notice(event: &str, data: &str) -> Notice {
    match serde_json::from_str::<RefData>(data) {
        Ok(RefData { git_ref, sha }) if event == "ref" => Notice::Ref { git_ref, sha },
        _ => Notice::Alive,
    }
}

/// Exponential reconnect delay: the base starts at `min`, doubles after each delay up to `max`,
/// and each delay is shaved by up to a quarter so readers do not reconnect in lockstep.
pub(crate) struct Backoff {
    min: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) fn new(min: Duration, max: Duration) -> Backoff {
        Backoff {
            min,
            max,
            next: min,
        }
    }

    /// The current base times (1 - 0.25 * jitter); the base then doubles, capped at max.
    pub(crate) fn delay(&mut self, jitter: f64) -> Duration {
        let delay = self.next.mul_f64(1.0 - 0.25 * jitter.clamp(0.0, 1.0));
        self.next = self.next.saturating_mul(2).min(self.max);
        delay
    }

    pub(crate) fn reset(&mut self) {
        self.next = self.min;
    }
}

/// A number in [0, 1) from hashing the clock with a randomly keyed hasher, so no RNG crate is needed.
pub(crate) fn jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    // The top 53 bits fill an f64 mantissa exactly, which keeps the result below 1.
    (RandomState::new().hash_one(nanos) >> 11) as f64 / (1u64 << 53) as f64
}

/// How long the reader waits between reconnects.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReaderConfig {
    pub(crate) backoff_min: Duration,
    pub(crate) backoff_max: Duration,
}

/// Start a thread that keeps the workspace's notice stream open and hands every notice to `out`.
///
/// Any close or failure is `Down` and a backed-off reconnect; a 401 is `SignedOut` and ends the
/// thread, as do `out` returning false and `stop` firing or being dropped during a wait. A read
/// blocked on a silent socket cannot be interrupted, so callers never join this thread.
pub(crate) fn spawn_reader(
    client: PotluckClient,
    token: TeamToken,
    team: &str,
    workspace: &str,
    config: ReaderConfig,
    stop: Receiver<()>,
    out: impl Fn(Notice) -> bool + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let (team, workspace) = (team.to_owned(), workspace.to_owned());
    thread::Builder::new()
        .name("dbento-team-notices".to_owned())
        .spawn(move || {
            let mut backoff = Backoff::new(config.backoff_min, config.backoff_max);
            loop {
                match client.open_events(&token, &team, &workspace) {
                    Ok(body) => {
                        if !out(Notice::Connected) {
                            return;
                        }
                        backoff.reset();
                        let mut open = true;
                        // A read error mid-stream is a close like any other.
                        let _ = read_stream(&mut BufReader::new(body), &mut |n| {
                            open = out(n);
                            open
                        });
                        if !open || !out(Notice::Down) {
                            return;
                        }
                    }
                    Err(ApiError::Unauthenticated) => {
                        out(Notice::SignedOut);
                        return;
                    }
                    Err(_) => {
                        if !out(Notice::Down) {
                            return;
                        }
                    }
                }
                // The wait doubles as the stop signal: a dropped sender ends the reader.
                match stop.recv_timeout(backoff.delay(jitter())) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Notice> {
        let mut seen = Vec::new();
        read_stream(&mut text.as_bytes(), &mut |n| {
            seen.push(n);
            true
        })
        .unwrap();
        seen
    }
    pub(super) fn reference(r: &str, sha: &str) -> Notice {
        Notice::Ref {
            git_ref: r.to_owned(),
            sha: sha.to_owned(),
        }
    }

    #[test]
    fn ref_events_keep_alives_and_other_blocks_parse() {
        let text = "retry: 2000\n\n:keep-alive\n\nevent: ref\ndata: {\"ref\": \"refs/heads/main\", \"sha\": \"a1\"}\n\nevent: ref\r\ndata: {\"ref\":\"refs/heads/dev\",\r\ndata: \"sha\":\"b2\"}\r\n\r\nid: 7\nevent: other\ndata: x\n\n";
        assert_eq!(
            parse(text),
            vec![
                Notice::Alive,
                Notice::Alive,
                reference("refs/heads/main", "a1"),
                reference("refs/heads/dev", "b2"),
                Notice::Alive
            ]
        );
    }

    #[test]
    fn a_bad_payload_is_only_liveness_an_endless_line_fails_and_false_stops() {
        assert_eq!(parse("event: ref\ndata: not json\n\n"), vec![Notice::Alive]);
        let long = format!("data: {}\n\n", "x".repeat(LINE_CAP + 1));
        assert!(read_stream(&mut long.as_bytes(), &mut |_| true).is_err());
        let mut count = 0;
        read_stream(&mut ":a\n:b\n".as_bytes(), &mut |_| {
            count += 1;
            false
        })
        .unwrap();
        assert_eq!(count, 1, "a false emit stops the read");
    }

    #[test]
    fn backoff_doubles_to_the_cap_jitters_down_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let secs: Vec<u64> = (0..8).map(|_| b.delay(0.0).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60]);
        b.reset();
        assert_eq!(b.delay(1.0), Duration::from_millis(750));
        assert_eq!(b.delay(0.5), Duration::from_millis(1750));
        assert!((0..100).map(|_| jitter()).all(|j| (0.0..1.0).contains(&j)));
    }
}

#[cfg(test)]
#[path = "../../tests/team/stub.rs"]
mod stub;

#[cfg(test)]
mod reader_tests {
    use super::stub::{Reply, Stub};
    use super::tests::reference;
    use super::*;
    use crate::team::{ServerUrl, TeamToken};
    use std::sync::mpsc;
    use std::time::Instant;

    const EVENTS: &str = "/api/v1/workspaces/acme/shared/events";

    fn reader(stub: &Stub, stop: mpsc::Receiver<()>) -> (JoinHandle<()>, mpsc::Receiver<Notice>) {
        let client = PotluckClient::new(&ServerUrl::parse(stub.uri()).unwrap(), false).unwrap();
        let token = TeamToken::parse(&format!("plk_{}", "A".repeat(43))).unwrap();
        let config = ReaderConfig {
            backoff_min: Duration::from_millis(10),
            backoff_max: Duration::from_millis(40),
        };
        let (tx, rx) = mpsc::channel();
        let handle = spawn_reader(client, token, "acme", "shared", config, stop, move |n| {
            tx.send(n).is_ok()
        })
        .unwrap();
        (handle, rx)
    }
    fn next(rx: &mpsc::Receiver<Notice>) -> Notice {
        rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }
    fn until(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_reader_reconnects_after_every_close_and_ends_on_401() {
        let stub = Stub::start();
        stub.reply("GET", EVENTS, Reply::Stream);
        let (_stop, stop_rx) = mpsc::channel();
        let (handle, rx) = reader(&stub, stop_rx);
        assert_eq!(next(&rx), Notice::Connected);
        until("first stream", || stub.hits("GET", EVENTS) == 1);
        stub.send("event: ref\ndata: {\"ref\":\"refs/heads/main\",\"sha\":\"a1\"}\n\n");
        assert_eq!(next(&rx), reference("refs/heads/main", "a1"));
        stub.close_streams();
        assert_eq!(next(&rx), Notice::Down);
        assert_eq!(next(&rx), Notice::Connected);
        until("second stream", || stub.hits("GET", EVENTS) == 2);
        stub.send(":keep-alive\n\n");
        assert_eq!(next(&rx), Notice::Alive);
        stub.reply("GET", EVENTS, Reply::Status(401));
        stub.close_streams();
        assert_eq!(next(&rx), Notice::Down);
        assert_eq!(next(&rx), Notice::SignedOut);
        until("the reader ended", || handle.is_finished());
        let head = stub.head("GET", EVENTS).unwrap();
        assert!(
            head.contains(&format!("authorization: bearer plk_{}", "a".repeat(43))),
            "{head}"
        );
        assert!(head.contains("accept: text/event-stream"), "{head}");
    }

    #[test]
    fn dropping_the_stop_sender_ends_a_backing_off_reader() {
        let stub = Stub::start();
        let (stop, stop_rx) = mpsc::channel();
        let (handle, rx) = reader(&stub, stop_rx);
        assert_eq!(next(&rx), Notice::Down, "a 404 counts as a close");
        drop(stop);
        until("the reader ended", || handle.is_finished());
    }
}
