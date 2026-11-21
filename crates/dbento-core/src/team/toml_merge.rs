//! Three-way merge of `dbento.toml` by entry.
//!
//! Each side's text is sliced into units: one per `connections.NAME` or `folders.NAME` entry,
//! one per other top-level item, plus any trailing text. Units are cut from the source spans
//! `toml_edit` reports, so comments and formatting travel with the entry they sit above.

use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    // after another unit, or in another place, is split across the file, which the splice
    // cannot express.
    anchors.sort_by_key(|a| a.0);
    let mut out: Vec<Unit> = Vec::new();
    let mut seen = HashSet::new();
    for (start, name, place) in anchors {
        if out
            .last()
            .is_some_and(|u| u.name == name && u.place == place)
        {
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

/// The text a unit is compared by: line endings as `\n`, trailing whitespace trimmed.
fn identity(slice: &str) -> String {
    slice.replace("\r\n", "\n").trim_end().to_owned()
}

/// Each unit's name mapped to the identity of its slice.
fn identities<'a>(text: &str, units: &'a [Unit]) -> BTreeMap<&'a str, String> {
    units
        .iter()
        .map(|u| (u.name.as_str(), identity(&text[u.range.clone()])))
        .collect()
}

/// Where an addition of mine goes in theirs' text.
enum Slot {
    Start,
    After(usize),
    End,
}

/// Merge my and their edits of base's text per unit, building on theirs' text. Fails with
/// `Units` naming every unit changed differently on both sides, or `Unmergeable`.
pub(crate) fn merge_workspace_text(
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<String, TomlMergeConflict> {
    let (base_units, our_units, their_units) = (units(base)?, units(ours)?, units(theirs)?);
    // Units match by name alone, so one name in two places (a quoted root key against a table)
    // would be spliced at the wrong place; such a file is left to the conflict choice.
    let mut places = BTreeMap::new();
    for u in base_units.iter().chain(&our_units).chain(&their_units) {
        if *places.entry(u.name.as_str()).or_insert(&u.place) != &u.place {
            return Err(Unmergeable);
        }
    }
    let (b, o, t) = (
        identities(base, &base_units),
        identities(ours, &our_units),
        identities(theirs, &their_units),
    );

    // A side that left a unit as base had it yields to the other side; when both changed it,
    // only an identical change merges. My changes are then sorted into what they do to theirs.
    let names: BTreeSet<&str> = b.keys().chain(o.keys()).chain(t.keys()).copied().collect();
    let (mut conflicts, mut removed, mut replaced) = (Vec::new(), HashSet::new(), HashSet::new());
    for name in names {
        let (mine, base_side, their_side) = (o.get(name), b.get(name), t.get(name));
        if mine == base_side || mine == their_side {
            continue;
        }
        if their_side != base_side {
            conflicts.push(name.to_owned());
        } else if mine.is_none() {
            removed.insert(name);
        } else if their_side.is_some() {
            replaced.insert(name);
        }
    }
    if !conflicts.is_empty() {
        return Err(TomlMergeConflict::Units(conflicts));
    }

    // An addition of mine follows theirs' last unit in the same place; a root key with none to
    // follow goes first, anything else last.
    let crlf = theirs.contains("\r\n");
    let mut added: Vec<(Slot, &str)> = Vec::new();
    let is_new = |u: &&Unit| !b.contains_key(u.name.as_str()) && !t.contains_key(u.name.as_str());
    for u in our_units.iter().filter(is_new) {
        let same = their_units
            .iter()
            .rposition(|x| x.place == u.place && u.place != Place::End);
        let slot = match (same, &u.place) {
            (Some(i), _) => Slot::After(i),
            (None, Place::Root) => Slot::Start,
            (None, _) => Slot::End,
        };
        added.push((slot, &ours[u.range.clone()]));
    }

    let mut out = String::with_capacity(theirs.len() + ours.len());
    let first = their_units.first().map_or(theirs.len(), |u| u.range.start);
    out.push_str(&theirs[..first]);
    let push_added = |out: &mut String, pick: &dyn Fn(&Slot) -> bool| {
        for (_, slice) in added.iter().filter(|(s, _)| pick(s)) {
            push_mine(out, slice, crlf);
        }
    };
    push_added(&mut out, &|s| matches!(s, Slot::Start));
    for (i, u) in their_units.iter().enumerate() {
        let name = u.name.as_str();
        if replaced.contains(name) {
            if let Some(mine) = our_units.iter().find(|m| m.name == name) {
                push_mine(&mut out, &ours[mine.range.clone()], crlf);
            }
        } else if !removed.contains(name) {
            out.push_str(&theirs[u.range.clone()]);
        }
        push_added(&mut out, &|s| matches!(s, Slot::After(j) if *j == i));
    }
    push_added(&mut out, &|s| matches!(s, Slot::End));
    Ok(out)
}

// A slice of mine takes theirs' line endings and always ends in one, and never runs onto
// a last line of theirs that had no line ending.
fn push_mine(out: &mut String, slice: &str, crlf: bool) {
    let eol = if crlf { "\r\n" } else { "\n" };
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    let mut text = slice.replace("\r\n", "\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if crlf {
        text = text.replace('\n', "\r\n");
    }
    out.push_str(&text);
}

#[cfg(test)]
mod tests;
