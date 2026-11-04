//! Drafts of edited SQL: one private JSON file per edited tab, so a crash, a forced quit or
//! a window close loses at most the last pause's worth of typing.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::persist::save_json;

/// How long a tab's text must rest before its draft is written.
pub const DRAFT_DEBOUNCE: Duration = Duration::from_millis(500);

// The format version a draft file carries; any other is skipped on load.
const VERSION: u32 = 1;

/// One edited tab's text and where it belongs. These fields are all a draft file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// The format version, 1.
    pub v: u32,
    /// The tab's draft id, also the file's name.
    pub id: String,
    /// The workspace folder the tab belongs to.
    pub workspace: PathBuf,
    /// The connection's name in that workspace.
    pub connection: String,
    /// The saved query the tab edits, relative to the workspace.
    pub file: Option<PathBuf>,
    /// The editor's text.
    pub sql: String,
    /// When the draft was made, in milliseconds since the Unix epoch.
    pub saved_at_ms: u64,
    /// The name the user gave the tab; absent from the file when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Draft {
    /// A version 1 draft stamped with the current time.
    pub fn new(
        id: &str,
        workspace: &Path,
        connection: &str,
        file: Option<&Path>,
        sql: &str,
    ) -> Draft {
        Draft {
            v: VERSION,
            id: id.to_owned(),
            workspace: workspace.to_path_buf(),
            connection: connection.to_owned(),
            file: file.map(Path::to_path_buf),
            sql: sql.to_owned(),
            saved_at_ms: since_epoch().as_millis().try_into().unwrap_or(u64::MAX),
            label: None,
        }
    }

    /// This draft carrying the tab's user-given name.
    pub fn with_label(self, label: Option<&str>) -> Draft {
        Draft {
            label: label.map(str::to_owned),
            ..self
        }
    }
}

/// The drafts folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Drafts {
    dir: PathBuf,
}

impl Drafts {
    /// Drafts kept in `dir`, which is created on the first write.
    pub fn new(dir: PathBuf) -> Drafts {
        Drafts { dir }
    }

    /// A fresh draft id, unique within and across runs, made only of `[0-9a-f-]`.
    pub fn new_id() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{:x}-{:x}-{:x}",
            since_epoch().as_nanos(),
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The file of draft `id`, or None unless `id` is 1 to 64 of `[0-9a-z-]`, so no id can
    /// name a path outside the folder.
    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.dir.join(format!("{id}.json")))
    }

    /// Writes `draft` atomically as a 0600 file in a 0700 folder. An invalid id is
    /// `InvalidInput`.
    pub fn write(&self, draft: &Draft) -> io::Result<()> {
        let path = self.path_of(&draft.id).ok_or_else(invalid_id)?;
        let mut folder = std::fs::DirBuilder::new();
        folder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
        folder.create(&self.dir)?;
        // save_json's temporary file is 0600 and its rename replaces a link, never follows it.
        save_json(&path, draft)?;
        // Sync the folder so the rename itself survives a crash. The draft is already in place,
        // so a failed sync is logged rather than reported as a draft that was not kept.
        #[cfg(unix)]
        if let Err(e) = std::fs::File::open(&self.dir).and_then(|d| d.sync_all()) {
            eprintln!("dbento: cannot sync the drafts folder: {e}");
        }
        Ok(())
    }

    /// Removes draft `id`; a missing file is Ok. An invalid id is `InvalidInput`.
    pub fn remove(&self, id: &str) -> io::Result<()> {
        let path = self.path_of(id).ok_or_else(invalid_id)?;
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The drafts of `workspace`, oldest first. Anything that is not a regular
    /// `<id>.json` file holding a version 1 draft with that id is skipped.
    pub fn load(&self, workspace: &Path) -> Vec<Draft> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut drafts: Vec<Draft> = entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let id = name.strip_suffix(".json").filter(|id| valid_id(id))?;
                read_regular(&self.dir.join(&name))
                    .and_then(|text| serde_json::from_str::<Draft>(&text).ok())
                    .filter(|d| d.v == VERSION && d.id == id && d.workspace == workspace)
            })
            .collect();
        drafts.sort_by_key(|d| d.saved_at_ms);
        drafts
    }
}

/// One tab's handle on its draft, ordering its writes and removals: each operation takes a
/// number when made, and one that runs after a later operation was made does nothing.
#[derive(Clone)]
pub struct DraftSlot {
    drafts: Drafts,
    id: String,
    latest: Arc<AtomicU64>,
    io: Arc<Mutex<()>>,
}

impl DraftSlot {
    /// The slot of draft `id` in `drafts`.
    pub fn new(drafts: Drafts, id: String) -> DraftSlot {
        DraftSlot {
            drafts,
            id,
            latest: Arc::default(),
            io: Arc::default(),
        }
    }

    /// The draft id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// An operation that writes `draft` unless a later one was made before it runs.
    pub fn write_later(&self, draft: Draft) -> impl FnOnce() -> io::Result<()> + Send + 'static {
        self.op(move |drafts, _| drafts.write(&draft))
    }

    /// An operation that removes the draft unless a later one was made before it runs.
    pub fn remove_later(&self) -> impl FnOnce() -> io::Result<()> + Send + 'static {
        self.op(|drafts, id| drafts.remove(id))
    }

    fn op(
        &self,
        f: impl FnOnce(&Drafts, &str) -> io::Result<()> + Send + 'static,
    ) -> impl FnOnce() -> io::Result<()> + Send + 'static {
        let me = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        move || {
            // The check and the file operation share one lock, so a newer operation cannot
            // finish between them and then be undone.
            let _io = this.io.lock().unwrap_or_else(|e| e.into_inner());
            if this.latest.load(Ordering::SeqCst) != me {
                return Ok(());
            }
            f(&this.drafts, &this.id)
        }
    }
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

fn invalid_id() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid draft id")
}

fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

// Reads `path` only when it is a regular file both before and after the open. As in core's
// history, O_NOFOLLOW refuses a link swapped in between and O_NONBLOCK keeps a FIFO from
// blocking the open before the check rejects it.
fn read_regular(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.file_type().is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    let after = file.metadata().ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return None;
        }
    }
    if !after.file_type().is_file() {
        return None;
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Some(text)
}
