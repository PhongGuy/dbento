//! The explorer's tree model: which folder and connection rows show, in what order.

use std::collections::HashSet;

use dbento_core::{FolderPath, Workspace, WorkspaceItem};
use gpui_kit::ElementId;

/// The deepest indent an explorer row shows; deeper folders line up at this level.
pub const MAX_INDENT_DEPTH: usize = 8;

/// The indent level a row at `depth` is drawn at: `depth`, capped at `MAX_INDENT_DEPTH`.
pub fn shown_depth(depth: usize) -> usize {
    depth.min(MAX_INDENT_DEPTH)
}

// Ids are keyed by name or path, not row position, so an id keeps naming the same row
// when an edit or reload shifts the rows. gpui-pilot sees them as `kind-name`, or as
// `kind[name]` when the name holds a hyphen, so `connection[toggle-lite]` stays distinct
// from `connection-toggle-lite`.
fn keyed(kind: &'static str, key: &str) -> ElementId {
    ElementId::from((ElementId::from(kind), key.to_owned()))
}

/// The element id of connection `name`'s explorer row.
pub fn connection_id(name: &str) -> ElementId {
    keyed("connection", name)
}

/// The AT-SPI id of connection `name`'s explorer row, spelled as the pilot sees `connection_id`.
pub fn connection_a11y_id(name: &str) -> String {
    if name.contains('-') {
        format!("connection[{name}]")
    } else {
        format!("connection-{name}")
    }
}

/// The element id of the folder row at `path`.
pub fn folder_id(path: &FolderPath) -> ElementId {
    keyed("folder", path.as_str())
}

/// The element id of a part of connection `name`'s row, such as `connection-toggle`.
pub fn part_id(part: &'static str, name: &str) -> ElementId {
    keyed(part, name)
}

/// The left bar a row shows while a dragged item hovers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropBar {
    /// The selection's accent bar.
    Accent,
    /// The drop target's bar.
    Drop,
}

/// A selected row keeps its accent bar under a drag, so the selection still shows; any other
/// row takes the drop colour. Both take the drop wash.
pub fn drop_bar(selected: bool) -> DropBar {
    if selected {
        DropBar::Accent
    } else {
        DropBar::Drop
    }
}

/// The payload of an explorer drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraggedItem(pub WorkspaceItem);

/// Whether `dragged` may drop into folder `target` (None = top level): never a folder into
/// itself or below itself.
pub fn drop_allowed(dragged: &WorkspaceItem, target: Option<&FolderPath>) -> bool {
    match dragged {
        // Walk up from the target; meeting the dragged folder means a drop into its own subtree.
        WorkspaceItem::Folder(from) => {
            std::iter::successors(target.cloned(), FolderPath::parent).all(|p| &p != from)
        }
        WorkspaceItem::Connection(_) | _ => true,
    }
}

/// One visible row of the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerRow {
    /// The folder or connection this row shows, by its stable id.
    pub item: WorkspaceItem,
    /// Nesting depth; top-level rows are 0.
    pub depth: usize,
    /// Position in `Workspace::folders()` for a folder, `Workspace::connections()` otherwise.
    pub index: usize,
    /// Whether a folder row shows its children; always false for connections.
    pub expanded: bool,
}

/// The visible rows of `ws`'s tree: at each level child folders, then connections, each in
/// the workspace's own order; rows below a folder in `collapsed` are left out.
pub fn explorer_rows(ws: &Workspace, collapsed: &HashSet<FolderPath>) -> Vec<ExplorerRow> {
    let mut rows = Vec::new();
    push_level(ws, collapsed, None, 0, &mut rows);
    rows
}

// Depth-first walk: a folder's row, then its whole subtree when expanded, then its next
// sibling. `folders()` is already in tree order, so filtering by parent keeps names sorted.
fn push_level(
    ws: &Workspace,
    collapsed: &HashSet<FolderPath>,
    parent: Option<&FolderPath>,
    depth: usize,
    rows: &mut Vec<ExplorerRow>,
) {
    for (index, folder) in ws.folders().iter().enumerate() {
        let path = folder.path();
        if path.parent().as_ref() != parent {
            continue;
        }
        let expanded = !collapsed.contains(path);
        rows.push(ExplorerRow {
            item: WorkspaceItem::Folder(path.clone()),
            depth,
            index,
            expanded,
        });
        if expanded {
            push_level(ws, collapsed, Some(path), depth + 1, rows);
        }
    }
    for (index, conn) in ws.connections().iter().enumerate() {
        if conn.folder() == parent {
            rows.push(ExplorerRow {
                item: WorkspaceItem::Connection(conn.name().to_owned()),
                depth,
                index,
                expanded: false,
            });
        }
    }
}
