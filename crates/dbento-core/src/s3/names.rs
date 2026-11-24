//! Showing S3 keys safely and turning them into file names.

use std::fmt::Write as _;

/// Longest file name suggested, leaving room for a download's temporary suffix under NAME_MAX.
const MAX_NAME_BYTES: usize = 200;

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

/// `key` safe to show: controls, line separators and bidi controls as `\u{hex}`, `\` doubled.
pub fn display_key(key: &str) -> String {
    // Doubling the backslash keeps the escape unambiguous: a literal `\u{a}` in a key shows as `\\u{a}`.
    let mut shown = String::with_capacity(key.len());
    for c in key.chars() {
        if c == '\\' {
            shown.push_str("\\\\");
        } else if hidden(c) {
            let _ = write!(shown, "\\u{{{:x}}}", u32::from(c));
        } else {
            shown.push(c);
        }
    }
    shown
}

/// A file name for saving the object at `key`; never empty, `.` or `..`.
///
/// It is the key's last non-empty `/` segment without hidden or Windows-reserved characters,
/// trimmed of dots and whitespace, cut to 200 bytes, with `_` before a Windows device name.
pub fn suggested_file_name(key: &str) -> String {
    let segment = key.rsplit('/').find(|s| !s.is_empty()).unwrap_or_default();
    let kept: String = segment
        .chars()
        .filter(|&c| !hidden(c) && !matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        .collect();
    let trimmed = trim_ends(&kept);
    let mut end = trimmed.len().min(MAX_NAME_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    // Trim again after the cut so a cut never leaves a trailing dot or space.
    let name = trim_ends(&trimmed[..end]);
    if name.is_empty() {
        "download".to_owned()
    } else if is_device_name(name) {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

fn trim_ends(s: &str) -> &str {
    s.trim_matches(|c: char| c == '.' || c.is_whitespace())
}

/// Windows reserves these stems whatever the case or extension: CON, PRN, AUX, NUL, COM1-9, LPT1-9.
fn is_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match stem.as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
