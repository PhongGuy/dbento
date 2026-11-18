//! Finding details: dotted key paths built from names alone, never from values.

use std::borrow::Cow;
use std::fmt::Write as _;

use potluck_kind::Finding;

/// The most bytes a finding's detail carries.
pub(crate) const MAX_DETAIL: usize = 200;

/// One key-path segment as TOML would write it: bare when it can be, else a basic string.
pub(crate) fn segment(name: &str) -> Cow<'_, str> {
    let bare = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        return Cow::Borrowed(name);
    }
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // Writing to a String cannot fail.
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Cow::Owned(out)
}

/// Cuts `s` to at most [`MAX_DETAIL`] bytes, backing off to the nearest char boundary.
pub(crate) fn cap(mut s: String) -> String {
    if s.len() > MAX_DETAIL {
        let mut end = MAX_DETAIL;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Attaches the capped `detail` to `f`, or leaves `f` bare when there is nothing to say.
pub(crate) fn with_detail(f: Finding, detail: String) -> Finding {
    if detail.is_empty() {
        f
    } else {
        f.with_detail(&cap(detail))
    }
}
