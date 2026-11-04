//! Titles and folder names to portable file names: safe on Windows, macOS and Linux alike.

#[cfg(test)]
mod tests;

/// The longest name kept, in bytes, before `.sql` is added.
const MAX_BYTES: usize = 200;

/// A query title as a file name that ends in `.sql`, added unless already there (any case).
pub(crate) fn file_name(title: &str) -> String {
    let name = segment(title);
    if name.to_ascii_lowercase().ends_with(".sql") {
        name
    } else {
        format!("{name}.sql")
    }
}

/// A folder name as one portable path segment, with no extension added.
pub(crate) fn segment(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let mut out = trim(&replaced);
    // Cutting can expose a dot or space at the end, so trim again after the cut.
    if out.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out = trim(&out[..end]);
    }
    if out.is_empty() {
        return "query".to_owned();
    }
    // The reserved check runs on the final name, since the cut and trim can leave a bare device name.
    if is_reserved(&out) {
        out.insert(0, '_');
    }
    out
}

fn trim(s: &str) -> String {
    s.trim_matches(['.', ' ']).to_owned()
}

// Windows reserves these device names whatever the case or extension.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let b = stem.as_bytes();
            b.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && (b'1'..=b'9').contains(&b[3])
        }
    }
}
