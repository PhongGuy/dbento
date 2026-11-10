//! Deciding what an import does to a workspace, then doing it.
//!
//! `plan` reads the workspace and composes the new `dbento.toml` text without writing anything;
//! `ImportPlan::apply` writes it through a temporary file and a rename.

use super::model::{ConnectionDraft, ImportSet, SecretSetting, SkipReason, Warning, WarningKind};
use crate::edit::WriteError;
use crate::workspace::ConnectionConfig;
use crate::workspace::{TEMPLATE, color_word};
use crate::{Engine, OpenError, TlsMode, Workspace};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod tests;

/// What to do when a draft's name is taken by a different connection.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Leave the existing connection alone and skip the draft.
    Skip,
    /// Import the draft under a free numbered name.
    Rename,
}

/// One decision of a plan.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A new connection table is appended.
    AddConnection {
        /// The Dbento name.
        name: String,
        /// The name in the source tool.
        source_name: String,
    },
    /// An identical connection already exists.
    ConnectionUnchanged {
        /// The Dbento name.
        name: String,
        /// The name in the source tool.
        source_name: String,
    },
    /// The name is taken by a different connection; the draft is skipped.
    ConnectionConflict {
        /// The Dbento name.
        name: String,
        /// The name in the source tool.
        source_name: String,
    },
    /// A query file is created.
    CreateQuery {
        /// The path, relative to the workspace.
        path: PathBuf,
        /// The file it comes from.
        origin: PathBuf,
    },
    /// The query file already exists with the same bytes.
    QueryUnchanged {
        /// The path, relative to the workspace.
        path: PathBuf,
        /// The file it comes from.
        origin: PathBuf,
    },
    /// The query is not imported.
    SkipQuery {
        /// The file it comes from.
        origin: PathBuf,
        /// Why.
        reason: SkipReason,
    },
}

impl Action {
    /// The short code used in reports.
    pub fn code(&self) -> &'static str {
        match self {
            Action::AddConnection { .. } => "add",
            Action::ConnectionUnchanged { .. } | Action::QueryUnchanged { .. } => "unchanged",
            Action::ConnectionConflict { .. } => "conflict",
            Action::CreateQuery { .. } => "create",
            Action::SkipQuery { .. } => "skip",
        }
    }
}

/// Why an import could not be planned. Nothing was written.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The existing workspace is invalid or unreadable.
    #[error(transparent)]
    Workspace(#[from] OpenError),
    /// The new tables cannot be appended to the existing file.
    #[error(
        "{}: `connections` is an inline table; rewrite it as [connections.<name>] tables to import",
        path.display()
    )]
    Unmergeable {
        /// The `dbento.toml` path.
        path: PathBuf,
    },
    /// `dbento.toml` is a symlink.
    #[error("{} is a symlink; import never writes through one", path.display())]
    Symlink {
        /// The `dbento.toml` path.
        path: PathBuf,
    },
}

/// Why applying a plan stopped. `written` lists the files already written.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// A file or folder could not be written.
    #[error("cannot write {}: {message}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error text.
        message: String,
        /// The files written before the failure.
        written: Vec<PathBuf>,
    },
    /// `dbento.toml` changed after it was planned against.
    #[error("dbento.toml changed during the import; nothing more was written, run it again")]
    Changed {
        /// The files written before the change was noticed.
        written: Vec<PathBuf>,
    },
}

impl ApplyError {
    /// The files written before the failure, absolute.
    pub fn written(&self) -> &[PathBuf] {
        match self {
            ApplyError::Io { written, .. } | ApplyError::Changed { written } => written,
        }
    }
}

/// The result of a successful `apply`.
#[non_exhaustive]
#[derive(Debug)]
pub struct Applied {
    written: Vec<PathBuf>,
}

impl Applied {
    /// The files written, absolute, in write order.
    pub fn written(&self) -> &[PathBuf] {
        &self.written
    }
}

/// A set connection that ends up in the workspace, under its final Dbento name.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    source_id: String,
    name: String,
    added: bool,
}

impl Imported {
    /// The connection's id in the source tool.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The final Dbento name, after any rename.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// True when the plan adds the connection, false when an identical one is already there.
    pub fn added(&self) -> bool {
        self.added
    }
}

/// Every decision of an import against one workspace, ready to apply.
#[derive(Debug)]
pub struct ImportPlan {
    dir: PathBuf,
    file: PathBuf,
    // The `dbento.toml` text the plan was made against; None when the file was absent.
    seen: Option<String>,
    new_text: Option<String>,
    // Query files to create, absolute, with their bytes; written before `dbento.toml`.
    writes: Vec<(PathBuf, Vec<u8>)>,
    actions: Vec<Action>,
    warnings: Vec<Warning>,
    imported: Vec<Imported>,
}

/// Decides every action of importing `set` into the workspace at `ws_dir`, writing nothing.
///
/// A missing workspace folder or `dbento.toml` is fine; `apply` creates them. Fails when the
/// existing file is invalid, a symlink, or cannot take appended tables.
pub fn plan(
    ws_dir: &Path,
    set: &ImportSet,
    on_conflict: OnConflict,
) -> Result<ImportPlan, PlanError> {
    let io = |path: &Path, e: std::io::Error| OpenError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let dir = match std::fs::canonicalize(ws_dir) {
        Ok(dir) => dir,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            std::path::absolute(ws_dir).map_err(|e| io(ws_dir, e))?
        }
        Err(e) => return Err(io(ws_dir, e).into()),
    };
    let file = dir.join("dbento.toml");
    let seen = match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(PlanError::Symlink { path: file }),
        Ok(_) => Some(std::fs::read_to_string(&file).map_err(|e| io(&file, e))?),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(io(&file, e).into()),
    };
    let base = seen.as_deref().unwrap_or(TEMPLATE);
    let workspace = Workspace::from_text(dir.clone(), &file, base)?;

    // Every draft name is reserved so a Rename never lands on a name the set itself will use.
    let reserved: BTreeSet<&str> = set.connections().iter().map(|c| c.name()).collect();
    let mut actions = Vec::new();
    let mut warnings = Vec::new();
    let mut added = BTreeMap::new();
    let mut imported = Vec::new();
    // Dbento name per source id, None when the connection was skipped as a conflict.
    let mut folders: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for c in set.connections() {
        let source_name = c.source_name().to_owned();
        let mut draft = DraftToml::from(c.draft());
        let relative = relative_sqlite_path(&dir, c.draft());
        if let Some(rel) = &relative {
            draft.path = Some(rel.clone());
        }

        let candidates = candidate_names(&workspace, c.name(), on_conflict);
        if let Some(name) = candidates.iter().find(|n| {
            workspace
                .connection(n)
                .is_some_and(|e| same_server(&dir, e, c.draft()))
        }) {
            folders.insert(c.source_id(), Some(name.clone()));
            imported.push(Imported {
                source_id: c.source_id().to_owned(),
                name: name.clone(),
                added: false,
            });
            actions.push(Action::ConnectionUnchanged {
                name: name.clone(),
                source_name,
            });
            continue;
        }

        let taken = |n: &str| workspace.connection(n).is_some() || added.contains_key(n);
        let name = match on_conflict {
            OnConflict::Skip if taken(c.name()) => {
                let name = c.name().to_owned();
                folders.insert(c.source_id(), None);
                actions.push(Action::ConnectionConflict { name, source_name });
                continue;
            }
            OnConflict::Skip => c.name().to_owned(),
            OnConflict::Rename if !taken(c.name()) => c.name().to_owned(),
            #[allow(clippy::expect_used)] // finitely many names are taken, so one is free
            OnConflict::Rename => (2..)
                .map(|i| format!("{}-{i}", c.name()))
                .find(|n| !taken(n) && !reserved.contains(n.as_str()))
                .expect("an unbounded range always yields a free name"),
        };
        if c.draft().engine() == Engine::Sqlite && relative.is_none() {
            warnings.push(Warning::new(
                c.source_name().to_owned(),
                WarningKind::MachineSpecificPath,
            ));
        }
        added.insert(name.clone(), draft);
        folders.insert(c.source_id(), Some(name.clone()));
        imported.push(Imported {
            source_id: c.source_id().to_owned(),
            name: name.clone(),
            added: true,
        });
        actions.push(Action::AddConnection { name, source_name });
    }

    let mut writes = Vec::new();
    plan_queries(
        &dir,
        set,
        &folders,
        &mut actions,
        &mut warnings,
        &mut writes,
    )?;

    let new_text = if added.is_empty() {
        None
    } else {
        #[allow(clippy::expect_used)] // plain strings and tables, which TOML can always hold
        let tables = toml::to_string(&RootToml { connections: added })
            .expect("draft tables always serialize");
        // A CRLF file stays CRLF, so the append never mixes line endings.
        let eol = if base.contains("\r\n") { "\r\n" } else { "\n" };
        let mut text = base.to_owned();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push_str(eol);
        }
        text.push_str(eol);
        text.push_str(&tables.replace('\n', eol));
        // The re-parse applies every workspace rule to the result, inline tables included.
        // When the drafts fail on their own, their error is the one worth reporting.
        if Workspace::from_text(dir.clone(), &file, &text).is_err() {
            Workspace::from_text(dir.clone(), &file, &format!("dbento = 1\n\n{tables}"))?;
            return Err(PlanError::Unmergeable { path: file });
        }
        Some(text)
    };

    Ok(ImportPlan {
        dir,
        file,
        seen,
        new_text,
        writes,
        actions,
        warnings,
        imported,
    })
}

/// Places every query of the set: skip, leave in place, or create under its connection folder.
fn plan_queries(
    dir: &Path,
    set: &ImportSet,
    folders: &BTreeMap<&str, Option<String>>,
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
    writes: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<(), PlanError> {
    let io = |path: &Path, e: std::io::Error| {
        PlanError::Workspace(OpenError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    };
    // A missing path or a regular file where a folder belongs both mean "nothing here yet".
    let absent =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory);
    let queries_root = dir.join("queries");
    let mut planned: BTreeMap<PathBuf, &[u8]> = BTreeMap::new();

    for q in set.queries() {
        let origin = q.origin().to_path_buf();
        let skip = |reason| Action::SkipQuery {
            origin: origin.clone(),
            reason,
        };
        let relative = q.relative();
        let name = relative.file_name().and_then(|n| n.to_str());
        let inside = relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        let (Some(name), true) = (name, inside) else {
            actions.push(skip(SkipReason::OutsideRoot));
            continue;
        };

        let folder = match q.connection().and_then(|id| folders.get(id)) {
            Some(Some(conn)) => Some(conn.as_str()),
            Some(None) => {
                actions.push(skip(SkipReason::ConnectionConflict));
                continue;
            }
            None => None,
        };

        // A file already under queries/ is where it should be; only a stray binding is worth a note.
        if let Ok(real) = std::fs::canonicalize(&origin)
            && let Ok(rel) = real.strip_prefix(&queries_root)
        {
            let path = Path::new("queries").join(rel);
            if folder.is_some() {
                warnings.push(Warning::new(
                    path.to_string_lossy().into_owned(),
                    WarningKind::BoundByFolder,
                ));
            }
            actions.push(Action::QueryUnchanged { path, origin });
            continue;
        }

        let mut target_dir = queries_root.clone();
        if let Some(conn) = folder {
            target_dir.push(conn);
        }
        if let Some(parent) = relative.parent() {
            target_dir.push(parent);
        }

        // Walk from queries/ down; the first symlink on the way ends the placement.
        let mut cursor = queries_root.clone();
        let tail = target_dir
            .strip_prefix(&queries_root)
            .unwrap_or(Path::new(""));
        let mut linked = false;
        for part in std::iter::once(None).chain(tail.components().map(Some)) {
            if let Some(part) = part {
                cursor.push(part);
            }
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    linked = true;
                    break;
                }
                Ok(_) => {}
                Err(e) if absent(&e) => break,
                Err(e) => return Err(io(&cursor, e)),
            }
        }
        if linked {
            actions.push(skip(SkipReason::Symlink));
            continue;
        }

        let stem = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        let ext = Path::new(name).extension().and_then(|e| e.to_str());
        let candidates = std::iter::once(name.to_owned()).chain((2..).map(|i| match ext {
            Some(ext) => format!("{stem}-{i}.{ext}"),
            None => format!("{stem}-{i}"),
        }));
        for candidate in candidates {
            let target = target_dir.join(candidate);
            let path = target.strip_prefix(dir).unwrap_or(&target).to_path_buf();
            if let Some(bytes) = planned.get(&target) {
                if *bytes == q.content() {
                    actions.push(Action::QueryUnchanged { path, origin });
                    break;
                }
                continue;
            }
            match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    actions.push(skip(SkipReason::Symlink));
                    break;
                }
                Ok(_) => {
                    if std::fs::read(&target).map_err(|e| io(&target, e))? == q.content() {
                        actions.push(Action::QueryUnchanged { path, origin });
                        break;
                    }
                }
                Err(e) if absent(&e) => {
                    planned.insert(target.clone(), q.content());
                    writes.push((target, q.content().to_vec()));
                    actions.push(Action::CreateQuery { path, origin });
                    break;
                }
                Err(e) => return Err(io(&target, e)),
            }
        }
    }
    Ok(())
}

/// Existing names a draft called `name` may already be imported as: `name`, plus under Rename
/// every `name-N` with N >= 2.
fn candidate_names(ws: &Workspace, name: &str, on_conflict: OnConflict) -> Vec<String> {
    let mut names = vec![name.to_owned()];
    if on_conflict == OnConflict::Rename {
        let prefix = format!("{name}-");
        names.extend(
            ws.connections()
                .iter()
                .map(|e| e.name())
                .filter(|n| {
                    n.strip_prefix(&prefix)
                        .is_some_and(|rest| rest.parse::<u32>().is_ok_and(|i| i >= 2))
                })
                .map(str::to_owned),
        );
    }
    names
}

/// Whether `existing` is the draft's server and at least as restrictive; names, colors, TLS and
/// secrets do not matter.
fn same_server(dir: &Path, existing: &ConnectionConfig, draft: &ConnectionDraft) -> bool {
    if existing.engine() != draft.engine() || !(existing.read_only() || !draft.read_only()) {
        return false;
    }
    match draft.engine() {
        Engine::Sqlite => {
            let (Some(have), Some(path)) = (existing.sqlite_path(), draft.path()) else {
                return false;
            };
            // A Windows path is never normalized: it matches only its verbatim self.
            if is_windows_path(path) {
                return have == dir.join(path);
            }
            normalize(have) == normalize(&dir.join(path))
        }
        engine @ (Engine::Postgres
        | Engine::Mysql
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Surrealdb
        | Engine::Redis) => {
            let default_port = if engine == Engine::Mysql { 3306 } else { 5432 };
            existing
                .host()
                .zip(draft.host())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                && existing.port() == Some(draft.port().unwrap_or(default_port))
                && existing.database() == draft.database()
                && existing.user() == draft.user()
        }
    }
}

/// The SQLite path written relative to the workspace with `/`, or None when it lies outside.
fn relative_sqlite_path(dir: &Path, draft: &ConnectionDraft) -> Option<String> {
    let path = draft.path().filter(|p| !is_windows_path(p))?;
    let rel = normalize(Path::new(path))
        .strip_prefix(dir)
        .ok()?
        .to_path_buf();
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    parts.filter(|p| !p.is_empty()).map(|p| p.join("/"))
}

// A drive path (`C:\`, `C:/`) or UNC path is kept verbatim whatever machine reads it.
pub(crate) fn is_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

// Lexical `.` and `..` removal: the file may not exist yet, so `canonicalize` is not an option.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                out.push(other)
            }
        }
    }
    out
}

impl ImportPlan {
    /// One connection action per set connection, in set order, then the query actions.
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// Warnings raised while planning.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// One entry per AddConnection and ConnectionUnchanged, in set order.
    pub fn imported(&self) -> &[Imported] {
        &self.imported
    }

    /// Writes the plan: new query files first (never overwriting one), then `dbento.toml`
    /// through a temporary file and a rename that keeps its permissions.
    ///
    /// Aborts with `Changed` if `dbento.toml` moved since `plan` read it.
    pub fn apply(self) -> Result<Applied, ApplyError> {
        let mut written = Vec::new();
        // A workspace without dbento.toml gets the template as soon as anything is written.
        let text = match self.new_text {
            Some(text) => text,
            None if !self.writes.is_empty() && self.seen.is_none() => TEMPLATE.to_owned(),
            None if self.writes.is_empty() => return Ok(Applied { written }),
            None => String::new(),
        };
        let io = |path: &Path, e: std::io::Error, written: &[PathBuf]| ApplyError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
            written: written.to_vec(),
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| io(&self.dir, e, &written))?;

        // Query files first: a failure later leaves them behind, and a rerun finds them unchanged.
        for (path, bytes) in &self.writes {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(parent, e, &written))?;
            }
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| io(path, e, &written))?;
            // A half-written file is ours to remove, so a rerun does not see foreign bytes.
            if let Err(e) = out.write_all(bytes) {
                let _ = std::fs::remove_file(path);
                return Err(io(path, e, &written));
            }
            written.push(path.clone());
        }
        if text.is_empty() {
            return Ok(Applied { written });
        }

        crate::write::replace_file(
            &self.dir,
            &self.file,
            self.seen.as_deref(),
            &text,
            ".dbento.toml.import.tmp",
        )
        .map_err(|e| match e {
            WriteError::Changed => ApplyError::Changed {
                written: written.clone(),
            },
            WriteError::Symlink { path } => ApplyError::Io {
                path,
                message: "is a symbolic link".into(),
                written: written.clone(),
            },
            WriteError::Io { path, message } => ApplyError::Io {
                path,
                message,
                written: written.clone(),
            },
        })?;
        written.push(self.file);
        Ok(Applied { written })
    }
}

#[derive(Serialize)]
struct RootToml {
    connections: BTreeMap<String, DraftToml>,
}

// Field order is the key order in the written table; defaults are omitted.
#[derive(Serialize)]
struct DraftToml {
    engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_ca: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_cert: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_key: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<String, String>,
}

impl From<&ConnectionDraft> for DraftToml {
    fn from(d: &ConnectionDraft) -> Self {
        DraftToml {
            engine: d.engine().name(),
            path: d.path().map(str::to_owned),
            host: d.host().map(str::to_owned),
            port: d.port(),
            database: d.database().map(str::to_owned),
            user: d.user().map(str::to_owned),
            secret: match d.secret() {
                Some(SecretSetting::None) => Some("none"),
                Some(SecretSetting::Keychain) | None => None,
            },
            tls: d.tls().map(|t| match t {
                TlsMode::Verify => "verify",
                TlsMode::Off => "off",
            }),
            tls_ca: d.tls_ca().map(str::to_owned),
            tls_cert: d.tls_cert().map(str::to_owned),
            tls_key: d.tls_key().map(str::to_owned),
            read_only: d.read_only(),
            color: d.color().map(color_word),
            folder: d.folder().map(|f| f.as_str().to_owned()),
            settings: d.settings().clone(),
        }
    }
}
