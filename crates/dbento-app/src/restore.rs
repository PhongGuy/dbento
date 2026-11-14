//! Which tabs a workspace reopens with: its saved session, then drafts no session names.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::app::{TableState, WorkspaceSession};
use crate::drafts::Draft;

/// One tab to reopen.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredTab {
    /// The connection's name.
    pub connection: String,
    /// The tab's draft id, kept so its later drafts replace the same file.
    pub draft: String,
    /// The saved query the tab edited, relative to the workspace.
    pub file: Option<PathBuf>,
    /// The draft's text, when the tab had one.
    pub sql: Option<String>,
    /// The tab's user-given name, from its draft.
    pub label: Option<String>,
    /// The relation a table tab was opened on, from the session; None for an orphan draft.
    pub table: Option<TableState>,
    /// The untitled tab's "Query N" number, from the session; None for an orphan draft.
    pub number: Option<usize>,
}

/// The tabs to reopen and the index to activate. The session's tabs come first in their
/// order, each with its draft's text; then every draft no session tab names, oldest first.
/// A tab or draft whose connection is not in `connections` is left out. The session's
/// active tab is activated when kept, else the first when any tab is restored, else None.
/// An orphan keeps its draft's `file`, which the shell binds when it still reads.
pub fn plan_restore(
    session: Option<&WorkspaceSession>,
    mut drafts: Vec<Draft>,
    connections: &[&str],
) -> (Vec<RestoredTab>, Option<usize>) {
    let known = |name: &str| connections.contains(&name);
    let saved = session.map_or(&[][..], |s| &s.tabs[..]);
    let named: HashSet<&str> = saved.iter().map(|t| t.draft.as_str()).collect();
    let mut tabs = Vec::new();
    let mut active = None;
    for (ix, tab) in saved.iter().enumerate() {
        if !known(&tab.connection) {
            continue;
        }
        if session.and_then(|s| s.active) == Some(ix) {
            active = Some(tabs.len());
        }
        let draft = drafts.iter().find(|d| d.id == tab.draft);
        tabs.push(RestoredTab {
            connection: tab.connection.clone(),
            draft: tab.draft.clone(),
            file: tab.file.clone(),
            sql: draft.map(|d| d.sql.clone()),
            label: draft.and_then(|d| d.label.clone()),
            table: tab.table.clone(),
            number: tab.number,
        });
    }
    // A crash between a draft's write and the next session save leaves a draft no saved tab
    // names; it comes back as an extra tab rather than being lost.
    drafts.sort_by_key(|d| d.saved_at_ms);
    tabs.extend(
        drafts
            .into_iter()
            .filter(|d| !named.contains(d.id.as_str()) && known(&d.connection))
            .map(|d| RestoredTab {
                connection: d.connection,
                draft: d.id,
                file: d.file,
                sql: Some(d.sql),
                label: d.label,
                table: None,
                number: None,
            }),
    );
    let active = active.or((!tabs.is_empty()).then_some(0));
    (tabs, active)
}
