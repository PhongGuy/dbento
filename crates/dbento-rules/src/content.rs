//! The rules that read the contents of `dbento.toml`.

/// The format version a `dbento.toml` declares: its root `dbento` integer as a decimal string.
///
/// Anything toml's parser rejects (non-UTF-8, a syntax error, a duplicate key) or a `dbento` that
/// is missing or not an integer gives `None`. BOM, CRLF and comments are left to the parser.
pub(crate) fn detect_format(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let table = text.parse::<toml::Table>().ok()?;
    match table.get("dbento")? {
        toml::Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}
