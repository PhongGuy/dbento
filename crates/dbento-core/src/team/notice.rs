//! The server's notice stream: a server-sent events parser and the reconnect backoff.

use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, Read};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

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
