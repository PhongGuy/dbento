//! A browse tab's state: the table it shows, its filter and sort, and the SQL that runs
//! them. Every change goes through `browse_sql`, so a state never holds SQL it cannot run.

use crate::completion::{Tok, lex};
use dbento_core::{
    Browse, BrowseError, EditTarget, Engine, ForeignKey, RelationRef, Sort, browse_sql,
};

/// A described table with its filter and sort, and the browse SQL for both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseState {
    target: EditTarget,
    browse: Browse,
    sql: String,
    foreign_keys: Vec<ForeignKey>,
}

impl BrowseState {
    /// The state for `browse` on `target`; fails when `browse_sql` refuses the filter.
    pub fn new(target: EditTarget, browse: Browse) -> Result<Self, BrowseError> {
        let sql = browse_sql(&target, &browse)?;
        Ok(Self {
            target,
            browse,
            sql,
            foreign_keys: Vec::new(),
        })
    }

    /// This state with `keys` as the table's foreign keys; `with_filter` and the sort builders keep them.
    pub fn with_foreign_keys(self, keys: Vec<ForeignKey>) -> Self {
        Self {
            foreign_keys: keys,
            ..self
        }
    }

    /// The table's foreign keys in both directions, as read with its target.
    pub fn foreign_keys(&self) -> &[ForeignKey] {
        &self.foreign_keys
    }

    /// The table this state browses.
    pub fn target(&self) -> &EditTarget {
        &self.target
    }

    /// The applied filter and sort.
    pub fn browse(&self) -> &Browse {
        &self.browse
    }

    /// `browse_sql(target, browse)`, computed once.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// Same sort, filter `filter` (None when blank, else kept as typed).
    pub fn with_filter(&self, filter: &str) -> Result<Self, BrowseError> {
        let filter = (!filter.trim().is_empty()).then(|| filter.to_owned());
        Self::new(
            self.target.clone(),
            Browse::new(filter, self.browse.sort.clone()),
        )
        .map(|s| s.with_foreign_keys(self.foreign_keys.clone()))
    }

    /// Same filter, sort `next_sort(current, column)`.
    pub fn with_next_sort(&self, column: &str) -> Result<Self, BrowseError> {
        let sort = next_sort(self.browse.sort.as_ref(), column);
        Self::new(
            self.target.clone(),
            Browse::new(self.browse.filter.clone(), sort),
        )
        .map(|s| s.with_foreign_keys(self.foreign_keys.clone()))
    }

    /// Same filter, sort `sort`; fails on an unknown or unsortable column.
    pub fn with_sort(&self, sort: Option<Sort>) -> Result<Self, BrowseError> {
        Self::new(
            self.target.clone(),
            Browse::new(self.browse.filter.clone(), sort),
        )
        .map(|s| s.with_foreign_keys(self.foreign_keys.clone()))
    }

    /// Some(descending) when sorted by `column`.
    pub fn sort_of(&self, column: &str) -> Option<bool> {
        self.browse
            .sort
            .as_ref()
            .filter(|s| s.column == column)
            .map(|s| s.descending)
    }
}

/// `clause` ANDed onto the `existing` filter, which is parenthesised when it holds an OR
/// token so the AND cannot bind inside it; a blank filter yields `clause` alone. The one-line
/// WHERE bar cannot hold a line break, so a trailing `--` comment stays last with the clause
/// in front. None when an open quote or block comment would swallow the clause.
pub fn and_filter(engine: Engine, existing: Option<&str>, clause: &str) -> Option<String> {
    let Some(existing) = existing.map(str::trim).filter(|e| !e.is_empty()) else {
        return Some(clause.to_owned());
    };
    // Lex so quotes and comments hide their text; a line comment is always "open" to its end.
    let toks = lex(engine, existing);
    let line_comment = |t: &Tok| existing[t.range.clone()].starts_with("--");
    if toks.iter().any(|t| t.open && !line_comment(t)) {
        return None;
    }
    let or = toks.iter().any(|t| t.is_word(existing, "or"));
    let Some(note) = toks.last().filter(|t| line_comment(t)) else {
        return Some(if or {
            format!("({existing}) AND {clause}")
        } else {
            format!("{existing} AND {clause}")
        });
    };
    // Close the parenthesis before the comment, keeping the user's spacing in front of it.
    let code = &existing[..note.range.start];
    let body = code.trim_end();
    Some(match (or, body.is_empty()) {
        (_, true) => format!("{clause} {existing}"),
        (true, false) => format!("{clause} AND ({body}){}", &existing[body.len()..]),
        (false, false) => format!("{clause} AND {existing}"),
    })
}

/// Whether [`and_filter`] can add a clause to `existing`.
pub fn joinable(engine: Engine, existing: Option<&str>) -> bool {
    and_filter(engine, existing, "").is_some()
}

/// Ascending, then descending, then none; another column starts ascending.
pub fn next_sort(current: Option<&Sort>, column: &str) -> Option<Sort> {
    match current {
        Some(s) if s.column == column && s.descending => None,
        Some(s) if s.column == column => Some(Sort::new(column, true)),
        _ => Some(Sort::new(column, false)),
    }
}

/// Why a browse result cannot be edited, in the order the reasons are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditBlock {
    /// The connection is read-only.
    ReadOnly,
    /// The table has no primary key and no row locator.
    NoIdentity,
    /// The last run was not this state's browse SQL, for example a selection run.
    NotBrowseRun,
    /// A SET or RESET ran on the Postgres session, so its values may print unlike apply's.
    SettingsChanged,
    /// The browse statement ended failed or skipped.
    RunFailed,
}

impl EditBlock {
    /// The short text the not-editable chip shows.
    pub fn reason(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only connection",
            Self::NoIdentity => "no row identity",
            Self::NotBrowseRun => "run the table query to edit",
            Self::SettingsChanged => "session settings changed; reconnect to edit",
            Self::RunFailed => "the table query failed",
        }
    }
}

/// The first reason, in variant order, that a browse result cannot be edited.
/// `last_run` is the last run's text; it must equal `state.sql()` after trimming.
pub fn edit_block(
    state: &BrowseState,
    read_only: bool,
    last_run: Option<&str>,
) -> Option<EditBlock> {
    if read_only {
        Some(EditBlock::ReadOnly)
    } else if !state.target.has_identity() {
        Some(EditBlock::NoIdentity)
    } else if last_run.map(str::trim) != Some(state.sql.trim()) {
        Some(EditBlock::NotBrowseRun)
    } else {
        None
    }
}

/// The notice a tab shows when its table could not open as a browse tab, carrying `reason`.
pub fn unavailable_text(reason: &str) -> String {
    format!("Opened as a plain query, so rows cannot be edited here: {reason}")
}

/// What `TabModel` asks its tab to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseEvent {
    /// Write this browse SQL into the editor and run it.
    Run(String),
    /// The tab stopped browsing, its browse state, pending or deferred target dropped.
    Cleared,
    /// The table could not be described, with why; the tab runs its plain SQL instead.
    Unavailable(String),
    /// A save committed `changes` staged rows to `relation`.
    Committed {
        /// The staged rows the commit carried, as the pending pill counted them.
        changes: usize,
        /// The table they landed in.
        relation: RelationRef,
    },
    /// A sorted continuation re-ran the browse and dropped the `rows` already loaded.
    Skipped {
        /// The rows already loaded, dropped from the re-run's start.
        rows: u64,
    },
}
