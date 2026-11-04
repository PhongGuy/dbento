//! The one reading of a result cell that every output format starts from.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io;

use dbento_core::CellRef;

/// A cell as text, or `None` for NULL; bytes read as Postgres prints `bytea`.
/// A cell kind this build does not know is an error, so it never reads as NULL.
pub(crate) fn cell_text(cell: CellRef<'_>) -> io::Result<Option<Cow<'_, str>>> {
    match cell {
        CellRef::Null => Ok(None),
        CellRef::Text(s) => Ok(Some(Cow::Borrowed(s))),
        CellRef::Bytes(b) => Ok(Some(Cow::Owned(bytes_hex(b)))),
        _ => Err(io::Error::other("unsupported cell kind")),
    }
}

/// Formats bytes the way Postgres prints `bytea`: `\x` and lowercase hex.
fn bytes_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("\\x");
    for b in bytes {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_text_and_bytes_read_alike_for_every_format() {
        assert_eq!(cell_text(CellRef::Null).unwrap(), None);
        assert_eq!(cell_text(CellRef::Text("a")).unwrap().as_deref(), Some("a"));
        assert_eq!(
            cell_text(CellRef::Bytes(&[0x00, 0xab])).unwrap().as_deref(),
            Some("\\x00ab")
        );
    }
}
