//! Three-way merge of `dbento.toml` by entry.
//!
//! Each side's text is sliced into units: one per `connections.NAME` or `folders.NAME` entry,
//! one per other top-level item, plus any trailing text. Units are cut from the source spans
//! `toml_edit` reports, so comments and formatting travel with the entry they sit above.

use std::collections::HashSet;
use std::ops::Range;

use toml_edit::{Document, Item, Key, Table};

use TomlMergeConflict::Unmergeable;

/// The name of the unit holding non-blank text after the last item.
const END: &str = "(end of file)";
/// The top-level tables whose entries are units of their own.
const SPLIT: [&str; 2] = ["connections", "folders"];

/// Why the two sides of `dbento.toml` could not be merged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TomlMergeConflict {
    /// A file did not parse, or used a shape the merge does not handle.
    Unmergeable,
    /// These units changed differently on both sides.
    Units(Vec<String>),
}

/// Where an added unit belongs in the other side's text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    Root,
    Table(String),
    End,
}

/// One contiguous slice of a source text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    name: String,
    place: Place,
    range: Range<usize>,
}

// A unit starts where its leading decor starts, so comments above a header travel with it;
// an empty decor has no span, and then the header or key itself is the start.
fn key_start(k: &Key) -> Result<usize, TomlMergeConflict> {
    k.leaf_decor()
        .prefix()
        .and_then(|p| p.span())
        .or_else(|| k.span())
        .map(|r| r.start)
        .ok_or(Unmergeable)
}

fn header_start(t: &Table) -> Result<usize, TomlMergeConflict> {
    t.decor()
        .prefix()
        .and_then(|p| p.span())
        .or_else(|| t.span())
        .map(|r| r.start)
        .ok_or(Unmergeable)
}

// Every explicit header at or below `t` anchors unit `name`. Implicit parents carry the key's
// span, so explicitness comes from the flags; dotted tables sit inside their header's text.
fn headers(
    t: &Table,
    name: &str,
    place: &Place,
    out: &mut Vec<(usize, String, Place)>,
) -> Result<(), TomlMergeConflict> {
    if !t.is_implicit() && !t.is_dotted() {
        out.push((header_start(t)?, name.to_owned(), place.clone()));
    }
    for (_, item) in t.iter() {
        match item {
            Item::Table(sub) if !sub.is_dotted() => headers(sub, name, place, out)?,
            Item::Table(_) | Item::Value(_) | Item::None => {}
            Item::ArrayOfTables(_) => return Err(Unmergeable),
        }
    }
    Ok(())
}

/// Units in source order; they tile the text from the first unit's start to its end.
fn units(text: &str) -> Result<Vec<Unit>, TomlMergeConflict> {
    let doc = Document::parse(text).map_err(|_| Unmergeable)?;
    let root = doc.as_table();

    // Collect an anchor for every place a unit's text begins. Entries of the split tables
    // must be header tables; inline, dotted and array forms cannot be cut apart by span.
    let mut anchors = Vec::new();
    for (key, item) in root.iter() {
        match item {
            Item::Value(_) if !SPLIT.contains(&key) => {
                let k = root.key(key).ok_or(Unmergeable)?;
                anchors.push((key_start(k)?, key.to_owned(), Place::Root));
            }
            Item::Table(t) if !t.is_dotted() => {
                let place = Place::Table(key.to_owned());
                if SPLIT.contains(&key) {
                    if !t.is_implicit() {
                        anchors.push((header_start(t)?, key.to_owned(), place.clone()));
                    }
                    for (name, entry) in t.iter() {
                        match entry {
                            Item::Table(e) if !e.is_dotted() => {
                                headers(e, &format!("{key}.{name}"), &place, &mut anchors)?;
                            }
                            Item::Table(_)
                            | Item::Value(_)
                            | Item::ArrayOfTables(_)
                            | Item::None => {
                                return Err(Unmergeable);
                            }
                        }
                    }
                } else {
                    headers(t, key, &place, &mut anchors)?;
                }
            }
            Item::Value(_) | Item::Table(_) | Item::ArrayOfTables(_) | Item::None => {
                return Err(Unmergeable);
            }
        }
    }

    // In source order, a unit runs until the next unit's anchor; a name that comes back
    // after another unit is split across the file, which the splice cannot express.
    anchors.sort_by_key(|a| a.0);
    let mut out: Vec<Unit> = Vec::new();
    let mut seen = HashSet::new();
    for (start, name, place) in anchors {
        if out.last().is_some_and(|u| u.name == name) {
            continue;
        }
        if !seen.insert(name.clone()) {
            return Err(Unmergeable);
        }
        if let Some(prev) = out.last_mut() {
            prev.range.end = start;
        }
        out.push(Unit {
            name,
            place,
            range: start..text.len(),
        });
    }

    // Non-blank text after the last item is a unit of its own; blank text stays with the last.
    if let Some(span) = doc
        .trailing()
        .span()
        .filter(|s| !text[s.clone()].trim().is_empty())
    {
        if let Some(prev) = out.last_mut() {
            prev.range.end = span.start;
        }
        out.push(Unit {
            name: END.to_owned(),
            place: Place::End,
            range: span.start..text.len(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
