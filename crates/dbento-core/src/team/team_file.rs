//! The team file: the accounts signed in to team servers and the folders bound to their
//! workspaces. It never holds a token; tokens live in a [`super::TokenStore`].

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Binding, Role, ServerUrl, TeamAccount};

/// Why the team file could not be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TeamFileError {
    /// The file, its lock or its folder failed, or the file is not valid team JSON.
    #[error("the team file {path} could not be used: {message}")]
    Io {
        /// The team file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
}

/// The `team.json` at one path, read whole and rewritten whole under a cross-process lock.
#[derive(Debug, Clone)]
pub struct TeamFile {
    path: PathBuf,
}

impl TeamFile {
    /// The team file at `path`, usually `StatePaths::team`.
    pub fn new(path: impl Into<PathBuf>) -> TeamFile {
        TeamFile { path: path.into() }
    }

    /// Where the file lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's contents; a missing file is empty. Takes no lock, since every write is an
    /// atomic rename and a reader sees either the old file or the new one.
    pub fn load(&self) -> Result<TeamData, TeamFileError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(TeamData::default()),
            Err(e) => return Err(self.failed(e)),
        };
        serde_json::from_slice(&bytes).map_err(|e| self.failed(e))
    }

    /// Applies `change` to the current contents and writes the result back, holding the lock
    /// on `<name>.lock` throughout so no other process or thread loses its own change. A file
    /// that does not load is left untouched and `change` never runs.
    pub fn update<T>(&self, change: impl FnOnce(&mut TeamData) -> T) -> Result<T, TeamFileError> {
        let dir = match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            Some(_) | None => Path::new("."),
        };
        std::fs::create_dir_all(dir).map_err(|e| self.failed(e))?;
        // A fresh handle per call: flock conflicts per open file, so two updates in one
        // process exclude each other just as two processes do.
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
            .map_err(|e| self.failed(e))?;
        lock.lock().map_err(|e| self.failed(e))?;
        let mut data = self.load()?;
        let out = change(&mut data);
        let bytes = serde_json::to_vec_pretty(&data).map_err(|e| self.failed(e))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| self.failed(e))?;
        tmp.write_all(&bytes).map_err(|e| self.failed(e))?;
        tmp.as_file().sync_all().map_err(|e| self.failed(e))?;
        tmp.persist(&self.path).map_err(|e| self.failed(e.error))?;
        Ok(out)
    }

    /// `<name>.lock` beside the file.
    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    fn failed(&self, e: impl std::fmt::Display) -> TeamFileError {
        TeamFileError::Io {
            path: self.path.clone(),
            message: e.to_string(),
        }
    }
}

/// Everything `team.json` holds: signed-in accounts and bound folders.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamData {
    accounts: Vec<AccountRecord>,
    bindings: BTreeMap<String, BindingRecord>,
}

impl TeamData {
    /// Every signed-in account.
    pub fn accounts(&self) -> &[AccountRecord] {
        &self.accounts
    }

    /// The account `username` on `server`, or with no name the server's only account.
    pub fn account(&self, server: &ServerUrl, username: Option<&str>) -> Option<&AccountRecord> {
        let mut on_server = self.accounts.iter().filter(|a| a.server == server.as_str());
        match username {
            Some(name) => on_server.find(|a| a.username == name),
            None => match (on_server.next(), on_server.next()) {
                (Some(only), None) => Some(only),
                (Some(_), Some(_)) | (None, _) => None,
            },
        }
    }

    /// Adds `account`, replacing the one with the same server and username in place.
    pub fn put_account(&mut self, account: AccountRecord) {
        let same =
            |a: &&mut AccountRecord| a.server == account.server && a.username == account.username;
        match self.accounts.iter_mut().find(same) {
            Some(slot) => *slot = account,
            None => self.accounts.push(account),
        }
    }

    /// Removes `username` on `server`; false when there was no such account.
    pub fn remove_account(&mut self, server: &ServerUrl, username: &str) -> bool {
        let before = self.accounts.len();
        self.accounts
            .retain(|a| !(a.server == server.as_str() && a.username == username));
        self.accounts.len() != before
    }

    /// Every binding, keyed by its folder's canonical path.
    pub fn bindings(&self) -> &BTreeMap<String, BindingRecord> {
        &self.bindings
    }

    /// The binding of `dir` or of its nearest bound ancestor, with that bound folder.
    pub fn binding_for(&self, dir: &Path) -> Option<(PathBuf, BindingRecord)> {
        let start = canonical_or_given(dir);
        start.ancestors().find_map(|folder| {
            let record = self.bindings.get(folder.to_string_lossy().as_ref())?;
            Some((folder.to_path_buf(), record.clone()))
        })
    }

    /// Binds `dir`, which must exist, replacing any binding it had.
    pub fn put_binding(&mut self, dir: &Path, binding: BindingRecord) -> std::io::Result<()> {
        let key = std::fs::canonicalize(dir)?.to_string_lossy().into_owned();
        self.bindings.insert(key, binding);
        Ok(())
    }

    /// Unbinds `dir` itself; false when it was not bound.
    pub fn remove_binding(&mut self, dir: &Path) -> bool {
        let key = canonical_or_given(dir).to_string_lossy().into_owned();
        self.bindings.remove(&key).is_some()
    }
}

/// `dir` canonicalized, or as given when that fails because the folder moved or is gone, so
/// a stale binding can still be found and removed by its recorded path.
fn canonical_or_given(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// One signed-in account; its token is kept in a token store, never here.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRecord {
    /// The server's normalized address.
    pub server: String,
    /// The sign-in name.
    pub username: String,
    /// The server's id for the token, used to revoke it; `None` when it was given by hand.
    pub token_id: Option<String>,
    /// Whether plain `http://` was allowed at sign-in, and so for every later call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure_http: bool,
}

impl AccountRecord {
    /// `username` on `server`.
    pub fn new(
        server: &ServerUrl,
        username: &str,
        token_id: Option<String>,
        insecure_http: bool,
    ) -> AccountRecord {
        AccountRecord {
            server: server.as_str().to_owned(),
            username: username.to_owned(),
            token_id,
            insecure_http,
        }
    }

    /// The token store key for this account; `None` when the stored server no longer parses.
    pub fn account(&self) -> Option<TeamAccount> {
        let server = ServerUrl::parse(&self.server).ok()?;
        Some(TeamAccount::new(server, self.username.as_str()))
    }
}

/// One bound folder: which server workspace and branch it syncs, as whom and with what role.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingRecord {
    /// The server the account belongs to.
    pub server: String,
    /// The URL git talks to.
    pub public_url: String,
    /// The account name.
    pub username: String,
    /// The team's name on the server.
    pub team: String,
    /// The workspace's name within the team.
    pub workspace: String,
    /// The one branch the folder syncs.
    pub branch: String,
    /// The server's role text, such as `read`, `write` or `admin`.
    pub role: String,
    /// When the folder last synced, in unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced: Option<u64>,
}

impl BindingRecord {
    /// A binding never synced yet.
    pub fn new(
        server: &str,
        public_url: &str,
        username: &str,
        team: &str,
        workspace: &str,
        branch: &str,
        role: &str,
    ) -> BindingRecord {
        BindingRecord {
            server: server.to_owned(),
            public_url: public_url.to_owned(),
            username: username.to_owned(),
            team: team.to_owned(),
            workspace: workspace.to_owned(),
            branch: branch.to_owned(),
            role: role.to_owned(),
            last_synced: None,
        }
    }

    /// The sync binding for folder `dir`. Only `write` and `admin` may push; any other role
    /// text, including one this version does not know, is read-only.
    pub fn to_binding(&self, dir: &Path) -> Binding {
        let role = match self.role.as_str() {
            "write" | "admin" => Role::Write,
            _ => Role::Read,
        };
        Binding::new(dir, self.public_url.as_str(), self.branch.as_str())
            .account(self.server.as_str(), self.username.as_str())
            .in_workspace(self.team.as_str(), self.workspace.as_str())
            .with_role(role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(s: &str) -> ServerUrl {
        ServerUrl::parse(s).unwrap()
    }
    fn binding() -> BindingRecord {
        BindingRecord::new(
            "https://t.example",
            "https://t.example",
            "ada",
            "acme",
            "shared",
            "main",
            "write",
        )
    }
    fn ada(s: &ServerUrl, id: Option<&str>) -> AccountRecord {
        AccountRecord::new(s, "ada", id.map(str::to_owned), false)
    }

    #[test]
    fn a_missing_file_is_empty_and_update_writes_the_spec_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let file = TeamFile::new(tmp.path().join("sub/team.json"));
        assert!(
            file.load().unwrap().accounts().is_empty(),
            "a missing file loads as empty"
        );
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        file.update(|d| {
            d.put_account(ada(&server("https://t.example"), Some("42")));
            d.put_binding(&ws, binding())
        })
        .unwrap()
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(
            json["accounts"][0],
            serde_json::json!({"server": "https://t.example", "username": "ada", "token_id": "42"}),
            "the account is written in the spec's shape"
        );
        let key = std::fs::canonicalize(&ws)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            json["bindings"][key.as_str()]["public_url"],
            "https://t.example",
            "the binding is keyed by its canonical folder"
        );
        assert_eq!(
            json["bindings"][key.as_str()]["role"],
            "write",
            "the binding keeps the server's role text"
        );
        let names: Vec<String> = std::fs::read_dir(tmp.path().join("sub"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| n == "team.json" || n == "team.json.lock"),
            "{names:?}"
        );
    }

    #[test]
    fn concurrent_updates_never_lose_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("team.json");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..5 {
                        let s = server(&format!("https://s{t}-{i}.example"));
                        TeamFile::new(&path)
                            .update(|d| d.put_account(ada(&s, None)))
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            TeamFile::new(&path).load().unwrap().accounts().len(),
            40,
            "every thread's every account survives"
        );
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("team.json");
        std::fs::write(&path, b"{not json").unwrap();
        let file = TeamFile::new(&path);
        assert!(file.load().is_err(), "a corrupt file does not load");
        assert!(
            file.update(|d| d.put_account(ada(&server("https://t.example"), None)))
                .is_err(),
            "a corrupt file is not updated"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{not json",
            "a corrupt file is left as it was"
        );
    }

    #[test]
    fn a_binding_is_found_from_a_subfolder_and_removed_by_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("queries/pg")).unwrap();
        let mut data = TeamData::default();
        data.put_binding(&ws, binding()).unwrap();
        let (dir, found) = data.binding_for(&ws.join("queries/pg")).unwrap();
        assert_eq!(
            (dir, found.workspace.as_str()),
            (std::fs::canonicalize(&ws).unwrap(), "shared"),
            "a subfolder finds its workspace's binding"
        );
        assert!(
            data.binding_for(tmp.path()).is_none(),
            "a parent of the folder is not bound"
        );
        assert!(data.remove_binding(&ws), "the folder's binding is removed");
        assert!(data.binding_for(&ws).is_none(), "the binding is gone");
    }

    #[test]
    fn a_moved_folder_can_still_be_unbound() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let canonical = std::fs::canonicalize(&ws).unwrap();
        let mut data = TeamData::default();
        data.put_binding(&ws, binding()).unwrap();
        std::fs::rename(&ws, tmp.path().join("moved")).unwrap();
        assert!(
            data.binding_for(&canonical).is_some(),
            "the old path still finds the binding"
        );
        assert!(
            data.remove_binding(&canonical),
            "the old path still unbinds"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_finds_the_same_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::os::unix::fs::symlink(&ws, tmp.path().join("link")).unwrap();
        let mut data = TeamData::default();
        data.put_binding(&tmp.path().join("link"), binding())
            .unwrap();
        assert!(
            data.binding_for(&ws).is_some(),
            "a link and its target share a binding"
        );
    }

    #[test]
    fn an_account_is_picked_by_name_or_as_the_only_one() {
        let mut data = TeamData::default();
        let s = server("https://t.example");
        data.put_account(ada(&s, Some("1")));
        assert_eq!(
            data.account(&s, None).unwrap().username,
            "ada",
            "the only account is picked"
        );
        data.put_account(AccountRecord::new(&s, "bob", Some("2".into()), false));
        assert!(data.account(&s, None).is_none(), "two accounts need a name");
        data.put_account(AccountRecord::new(&s, "bob", Some("3".into()), false));
        assert_eq!(data.accounts().len(), 2, "the same account is replaced");
        assert_eq!(
            data.account(&s, Some("bob")).unwrap().token_id.as_deref(),
            Some("3"),
            "the replacement keeps the new token id"
        );
        assert!(data.remove_account(&s, "ada"), "ada is removed");
        assert_eq!(
            data.account(&s, None).unwrap().username,
            "bob",
            "the one left is picked"
        );
    }
}
