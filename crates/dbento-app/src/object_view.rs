//! The object tab's model: an object's metadata and a text, JSON or hex preview of its first
//! bytes, with every shown text made safe against hidden characters.

use std::fmt::Write as _;

use dbento_core::{ObjectChunk, ObjectKey, display_key};

use crate::objects::{ObjectItem, s3_url, size_words};
use crate::record::pretty_json;

/// How many bytes a preview reads from the start of the object.
pub const PREVIEW_BYTES: u32 = dbento_core::MAX_READ_BYTES;
/// Bytes shown on one hex row.
pub const HEX_ROW: usize = 16;
/// The most hex rows a preview shows.
pub const MAX_HEX_ROWS: usize = 65_536;
/// Past this size pretty JSON is not built and the raw text shows instead.
const MAX_PRETTY_BYTES: usize = 4 << 20;

/// How the preview shows the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// The bytes as text.
    Text,
    /// The bytes as indented JSON.
    Json,
    /// The bytes as `hexdump -C` rows.
    Hex,
}

/// The first bytes of an object; a plain copy of core's `ObjectChunk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The bytes read from the start of the object.
    pub bytes: Vec<u8>,
    /// The object's whole size in bytes.
    pub total_size: u64,
    /// True when `bytes` is shorter than the object.
    pub truncated: bool,
    /// The MIME type the store reports.
    pub content_type: Option<String>,
}

impl From<ObjectChunk> for Preview {
    fn from(c: ObjectChunk) -> Self {
        Preview {
            bytes: c.bytes,
            total_size: c.total_size,
            truncated: c.truncated,
            content_type: c.content_type,
        }
    }
}

/// The bytes as UTF-8, a character cut by a truncated read dropped; None when not UTF-8.
fn decode(bytes: &[u8], truncated: bool) -> Option<&str> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(s),
        // An error with no length is an incomplete sequence at the very end.
        Err(e) if truncated && e.error_len().is_none() => {
            std::str::from_utf8(bytes.get(..e.valid_up_to())?).ok()
        }
        Err(_) => None,
    }
}

/// Whether a whole read of `text` is JSON: it claims to be, by content type or first
/// character, and it parses.
fn is_json(text: &str, truncated: bool, content_type: Option<&str>) -> bool {
    let typed = content_type.is_some_and(|ct| {
        let essence = ct.split(';').next().unwrap_or_default().trim();
        essence.to_ascii_lowercase().ends_with("json")
    });
    let looks = matches!(text.trim_start().chars().next(), Some('{' | '['));
    !truncated && (typed || looks) && serde_json::from_str::<serde_json::Value>(text).is_ok()
}

/// The mode a preview opens in: Hex unless the bytes are UTF-8 without NUL, then Json when
/// the whole document parses as JSON, else Text.
pub fn preview_mode(bytes: &[u8], truncated: bool, content_type: Option<&str>) -> PreviewMode {
    match decode(bytes, truncated) {
        Some(text) if !text.contains('\0') => {
            if is_json(text, truncated, content_type) {
                PreviewMode::Json
            } else {
                PreviewMode::Text
            }
        }
        Some(_) | None => PreviewMode::Hex,
    }
}

/// Characters that can disguise or reorder text: controls, line separators and bidi controls.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{61c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// `s` safe to show as content: newlines and tabs kept, CRLF as LF, every other hidden
/// character as `\u{hex}`. Backslashes stay as written, since this is content, not a key.
pub fn safe_text(s: &str) -> String {
    let mut shown = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\t' => shown.push(c),
            c if hidden(c) => {
                let _ = write!(shown, "\\u{{{:x}}}", u32::from(c));
            }
            c => shown.push(c),
        }
    }
    shown
}

/// One `hexdump -C` row of up to 16 bytes starting at `offset`.
pub fn hex_line(chunk: &[u8], offset: u64) -> String {
    let mut line = format!("{offset:08x}  ");
    for i in 0..HEX_ROW {
        match chunk.get(i) {
            Some(b) => {
                let _ = write!(line, "{b:02x} ");
            }
            None => line.push_str("   "),
        }
        if i == 7 {
            line.push(' ');
        }
    }
    line.push(' ');
    line.push('|');
    line.extend(chunk.iter().take(HEX_ROW).map(|&b| {
        if (0x20..=0x7e).contains(&b) {
            char::from(b)
        } else {
            '.'
        }
    }));
    line.push('|');
    line
}

/// One object tab: which object, what its HEAD said, the preview and how it shows.
#[derive(Debug, Clone)]
pub struct ObjectView {
    bucket: String,
    key: String,
    meta: Option<ObjectItem>,
    preview: Option<Preview>,
    mode: Option<PreviewMode>,
    /// The text the current mode shows, built once per mode change.
    shown: Option<String>,
    reading: bool,
    error: Option<String>,
}

impl ObjectView {
    /// A view of `key` in `bucket` with nothing read yet.
    pub fn new(bucket: impl Into<String>, key: impl Into<String>) -> Self {
        ObjectView {
            bucket: bucket.into(),
            key: key.into(),
            meta: None,
            preview: None,
            mode: None,
            shown: None,
            reading: false,
            error: None,
        }
    }

    /// The object this view shows.
    pub fn object_key(&self) -> ObjectKey {
        ObjectKey::new(self.bucket.clone(), self.key.clone())
    }

    /// The tab title: the escaped key, then the bucket.
    pub fn title(&self) -> String {
        format!(
            "{} \u{b7} {}",
            display_key(&self.key),
            display_key(&self.bucket)
        )
    }

    /// Marks a read as started; false while one is already in flight.
    pub fn begin(&mut self) -> bool {
        if self.reading {
            return false;
        }
        self.reading = true;
        self.error = None;
        true
    }

    /// Ends the read with the object's metadata and first bytes, in the mode they earn.
    pub fn apply(&mut self, meta: ObjectItem, preview: Preview) {
        let mode = preview_mode(
            &preview.bytes,
            preview.truncated,
            preview.content_type.as_deref(),
        );
        self.reading = false;
        self.error = None;
        self.meta = Some(meta);
        self.preview = Some(preview);
        self.show(mode);
    }

    /// Ends the read with why it failed; the view then shows only the message.
    pub fn fail(&mut self, message: String) {
        self.reading = false;
        self.error = Some(message);
        self.meta = None;
        self.preview = None;
        self.mode = None;
        self.shown = None;
    }

    /// What the HEAD said, once read.
    pub fn meta(&self) -> Option<&ObjectItem> {
        self.meta.as_ref()
    }

    /// The metadata line: URL, size, last write, etag, storage class and type, absent parts
    /// left out, store texts escaped.
    pub fn header(&self) -> Option<String> {
        let m = self.meta.as_ref()?;
        let mut parts = vec![s3_url(&self.bucket, &self.key), size_words(m.size)];
        parts.extend(m.last_modified.as_deref().map(display_key));
        parts.extend(
            m.etag
                .as_deref()
                .map(|e| format!("etag {}", display_key(e))),
        );
        parts.extend(m.storage_class.as_deref().map(display_key));
        parts.extend(m.content_type.as_deref().map(display_key));
        Some(parts.join(" \u{b7} "))
    }

    /// How much of a cut read shows, as "first 1.0 MiB of 3.0 MiB shown".
    pub fn note(&self) -> Option<String> {
        let p = self.preview.as_ref().filter(|p| p.truncated)?;
        Some(format!(
            "first {} of {} shown",
            size_words(p.bytes.len() as u64),
            size_words(p.total_size)
        ))
    }

    /// Why the last read failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The mode shown, once read.
    pub fn mode(&self) -> Option<PreviewMode> {
        self.mode
    }

    /// The modes the bytes allow: Text when UTF-8, Json when it parses, always Hex.
    pub fn modes(&self) -> Vec<PreviewMode> {
        let Some(p) = &self.preview else {
            return Vec::new();
        };
        let mut modes = Vec::with_capacity(3);
        if let Some(text) = decode(&p.bytes, p.truncated) {
            modes.push(PreviewMode::Text);
            if is_json(text, p.truncated, p.content_type.as_deref()) {
                modes.push(PreviewMode::Json);
            }
        }
        modes.push(PreviewMode::Hex);
        modes
    }

    /// Switches to `mode`; false when the bytes do not allow it.
    pub fn set_mode(&mut self, mode: PreviewMode) -> bool {
        if !self.modes().contains(&mode) {
            return false;
        }
        if self.mode != Some(mode) {
            self.show(mode);
        }
        true
    }

    /// The text shown: safe text in Text, indented safe JSON in Json, None in Hex.
    pub fn text(&self) -> Option<String> {
        self.shown.clone()
    }

    /// How many hex rows the preview has, at most [`MAX_HEX_ROWS`].
    pub fn hex_rows(&self) -> usize {
        self.preview
            .as_ref()
            .map_or(0, |p| p.bytes.len().div_ceil(HEX_ROW).min(MAX_HEX_ROWS))
    }

    /// Hex row `i`, or None past the last.
    pub fn hex_row(&self, i: usize) -> Option<String> {
        if i >= self.hex_rows() {
            return None;
        }
        let bytes = &self.preview.as_ref()?.bytes;
        let start = i * HEX_ROW;
        let chunk = bytes.get(start..(start + HEX_ROW).min(bytes.len()))?;
        Some(hex_line(chunk, start as u64))
    }

    fn show(&mut self, mode: PreviewMode) {
        self.mode = Some(mode);
        let text = self
            .preview
            .as_ref()
            .and_then(|p| decode(&p.bytes, p.truncated));
        self.shown = match (mode, text) {
            (PreviewMode::Text, Some(t)) => Some(safe_text(t)),
            // Serde leaves bidi characters in strings, so the pretty text is made safe too.
            (PreviewMode::Json, Some(t)) => Some(safe_text(
                &pretty_json(t)
                    .filter(|p| p.len() <= MAX_PRETTY_BYTES)
                    .unwrap_or_else(|| t.to_owned()),
            )),
            (PreviewMode::Hex, _) | (PreviewMode::Text | PreviewMode::Json, None) => None,
        };
    }
}
