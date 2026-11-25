//! Dbento's known_hosts: its own trusted host keys beside the user's `~/.ssh/known_hosts`,
//! read with strict rules. Only Dbento's file is ever written; the user's is only read.
//!
//! russh's known_hosts helpers are not used: they split on single spaces, skip `@revoked`,
//! compare same-algorithm keys only and give up on the whole file at one bad line.

use super::SshHome;
use crate::paths::StatePaths;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{Algorithm, HashAlg, PublicKey};
use sha1::Sha1;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

const DEFAULT_PORT: u16 = 22;

/// A server's host key and the host and port it was presented for.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    host: String,
    port: u16,
    key: PublicKey,
}

impl HostKey {
    /// The host the key was presented for, as resolved.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The SSH port the key was presented on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The key's SHA-256 fingerprint in OpenSSH's `SHA256:...` form.
    pub fn fingerprint(&self) -> String {
        self.key.fingerprint(HashAlg::Sha256).to_string()
    }

    /// The key's algorithm name, such as `ssh-ed25519`.
    pub fn algorithm(&self) -> String {
        self.key.algorithm().to_string()
    }

    /// A key for `host`:`port` from OpenSSH public key text; `None` when it does not parse.
    #[doc(hidden)]
    pub fn from_openssh(host: &str, port: u16, key: &str) -> Option<HostKey> {
        Some(HostKey {
            host: host.to_owned(),
            port,
            key: PublicKey::from_openssh(key).ok()?,
        })
    }
}

/// Why a presented host key is refused although known_hosts names its host.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyProblem {
    /// The host is known with a different key.
    Changed,
    /// The key is listed under `@revoked`.
    Revoked,
    /// A line for the host, or a `@revoked` line, holds a key that cannot be read.
    Unreadable,
}

/// What known_hosts says about a server's host key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// A known_hosts file lists the presented key for this host.
    Trusted,
    /// No file knows this host; the key can be trusted after the user checks it.
    Unknown(HostKey),
    /// The key is refused; `file` and `line` name the line that refuses it and
    /// `fingerprint` is the presented key's.
    Changed {
        /// The host the key was presented for.
        host: String,
        /// The SSH port.
        port: u16,
        /// The presented key's SHA-256 fingerprint.
        fingerprint: String,
        /// The known_hosts file holding the refusing line.
        file: PathBuf,
        /// The refusing line's number, from 1.
        line: usize,
        /// Why the key is refused.
        problem: HostKeyProblem,
    },
}

/// Why [`trust_host_key`] failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TrustHostKeyError {
    /// Dbento's known_hosts could not be read or written.
    #[error("cannot write {}: {message}", path.display())]
    Io {
        /// Dbento's known_hosts file.
        path: PathBuf,
        /// The operating system's reason.
        message: String,
    },
}

/// Why [`forget_host_key`] failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ForgetHostKeyError {
    /// Dbento's known_hosts could not be read or rewritten.
    #[error("cannot rewrite {}: {message}", path.display())]
    Io {
        /// Dbento's known_hosts file.
        path: PathBuf,
        /// The operating system's reason.
        message: String,
    },
}

/// Appends `key` to `paths.ssh_known_hosts` unless that identity already lists it.
pub fn trust_host_key(paths: &StatePaths, key: &HostKey) -> Result<(), TrustHostKeyError> {
    let path = &paths.ssh_known_hosts;
    let io = |e: std::io::Error| TrustHostKeyError::Io {
        path: path.clone(),
        message: e.to_string(),
    };
    let text = read_or_empty(path).map_err(io)?;
    let id = Identity {
        host: &key.host,
        port: key.port,
    };
    let updated = with_trusted(&text, &id, &key.key);
    if updated == text {
        return Ok(());
    }
    replace_file(path, &updated).map_err(io)
}

/// Removes every line of Dbento's file naming `host`:`port`; returns how many.
pub fn forget_host_key(
    paths: &StatePaths,
    host: &str,
    port: u16,
) -> Result<usize, ForgetHostKeyError> {
    let path = &paths.ssh_known_hosts;
    let io = |e: std::io::Error| ForgetHostKeyError::Io {
        path: path.clone(),
        message: e.to_string(),
    };
    let text = read_or_empty(path).map_err(io)?;
    let (left, removed) = without_identity(&text, &Identity { host, port });
    if removed > 0 {
        replace_file(path, &left).map_err(io)?;
    }
    Ok(removed)
}

/// Whether `<home>/.ssh/known_hosts` has a line naming `host`:`port`; reads only.
pub fn user_known_hosts_lists(home: &SshHome, host: &str, port: u16) -> bool {
    let path = home.dir().join(".ssh").join("known_hosts");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let id = Identity { host, port };
    lines(&text).any(|line| line.marker != Some(Marker::CertAuthority) && line.names(&id))
}

/// The host and port a key is looked up under.
pub(crate) struct Identity<'a> {
    pub(crate) host: &'a str,
    pub(crate) port: u16,
}

impl Identity<'_> {
    /// The known_hosts name: the lower-cased host at port 22, else `[host]:port`.
    pub(crate) fn name(&self) -> String {
        let host = self.host.to_ascii_lowercase();
        if self.port == DEFAULT_PORT {
            host
        } else {
            format!("[{host}]:{}", self.port)
        }
    }
}

/// One known_hosts file's path and text, in lookup order.
pub(crate) struct KnownFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

/// The outcome of checking a presented key against the known_hosts files.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Trusted,
    Unknown,
    Refused {
        file: PathBuf,
        line: usize,
        problem: HostKeyProblem,
    },
}

/// Judges `presented` for `id`. A revoked key refuses first, then an unreadable line that
/// could concern this host, then an equal key trusts, then any other known key means changed.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the SSH link's host-key handler calls it")
)]
pub(crate) fn check(files: &[KnownFile], id: &Identity<'_>, presented: &PublicKey) -> Verdict {
    let all = || {
        files
            .iter()
            .flat_map(|f| lines(&f.text).map(move |line| (f.path.as_path(), line)))
    };
    let refused = |file: &Path, line: &Line<'_>, problem| Verdict::Refused {
        file: file.to_path_buf(),
        line: line.number,
        problem,
    };
    let revoked = |line: &Line<'_>| line.marker == Some(Marker::Revoked);
    if let Some((file, line)) =
        all().find(|(_, l)| revoked(l) && l.key().is_some_and(|k| same_key(&k, presented)))
    {
        return refused(file, &line, HostKeyProblem::Revoked);
    }
    if let Some((file, line)) =
        all().find(|(_, l)| (revoked(l) || l.is_entry_for(id)) && l.key().is_none())
    {
        return refused(file, &line, HostKeyProblem::Unreadable);
    }
    let mut changed = None;
    for (file, line) in all().filter(|(_, l)| l.is_entry_for(id)) {
        match line.key() {
            Some(k) if same_key(&k, presented) => return Verdict::Trusted,
            Some(_) | None => {
                changed.get_or_insert_with(|| refused(file, &line, HostKeyProblem::Changed));
            }
        }
    }
    changed.unwrap_or(Verdict::Unknown)
}

/// The algorithms of the keys known for `id`, in file order without repeats.
#[allow(
    dead_code,
    reason = "the SSH link orders its preferred host-key algorithms by it"
)]
pub(crate) fn known_algorithms(files: &[KnownFile], id: &Identity<'_>) -> Vec<Algorithm> {
    let mut found = Vec::new();
    for key in files
        .iter()
        .flat_map(|f| lines(&f.text))
        .filter(|l| l.is_entry_for(id))
        .filter_map(|l| l.key())
    {
        let algorithm = key.algorithm();
        if !found.contains(&algorithm) {
            found.push(algorithm);
        }
    }
    found
}

/// `text` with a line for `id` and `key` appended, unless `id` already lists that key.
pub(crate) fn with_trusted(text: &str, id: &Identity<'_>, key: &PublicKey) -> String {
    let listed = lines(text)
        .filter(|l| l.is_entry_for(id))
        .any(|l| l.key().is_some_and(|k| same_key(&k, key)));
    // The comment is dropped so the line carries only what identifies the key. Encoding a
    // key that was decoded cannot fail in practice; if it did, nothing is appended.
    let bare = PublicKey::new(key.key_data().clone(), "");
    let encoded = match bare.to_openssh() {
        Ok(encoded) if !listed => encoded,
        Ok(_) | Err(_) => return text.to_owned(),
    };
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{} {encoded}\n", id.name()));
    out
}

/// `text` without the unmarked lines naming `id`, and how many were removed. Other lines
/// are kept as written; a `@revoked` line stays, so forgetting never lifts a revocation.
pub(crate) fn without_identity(text: &str, id: &Identity<'_>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    for raw in text.lines() {
        let gone = parse_line(0, raw).is_some_and(|l| l.marker.is_none() && l.names(id));
        if gone {
            removed += 1;
        } else {
            out.push_str(raw);
            out.push('\n');
        }
    }
    (out, removed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Revoked,
    CertAuthority,
    Other,
}

/// One entry line, whitespace collapsed: its 1-based number, marker, host field and key text.
struct Line<'a> {
    number: usize,
    marker: Option<Marker>,
    hosts: &'a str,
    key: String,
}

impl Line<'_> {
    /// A plain entry whose host field names `id`; marked lines never vouch for a host.
    fn is_entry_for(&self, id: &Identity<'_>) -> bool {
        self.marker.is_none() && self.names(id)
    }

    /// Whether the host field names `id` exactly. A wildcard pattern never matches, a
    /// negation makes the whole line not match, and an unreadable hashed name skips the line.
    fn names(&self, id: &Identity<'_>) -> bool {
        let name = id.name();
        let mut hit = false;
        for pattern in self.hosts.split(',') {
            if pattern.starts_with('!') {
                return false;
            }
            if let Some(hashed) = pattern.strip_prefix("|1|") {
                match hashed_names(hashed, &name) {
                    Some(true) => hit = true,
                    Some(false) => {}
                    None => return false,
                }
            } else if !pattern.contains(['*', '?']) && pattern.eq_ignore_ascii_case(&name) {
                hit = true;
            }
        }
        hit
    }

    fn key(&self) -> Option<PublicKey> {
        PublicKey::from_openssh(&self.key).ok()
    }
}

/// Whether the `salt|hash` of a `|1|` name is HMAC-SHA1 of `name`; `None` when unreadable.
fn hashed_names(hashed: &str, name: &str) -> Option<bool> {
    let (salt, hash) = hashed.split_once('|')?;
    let salt = STANDARD.decode(salt).ok()?;
    let hash = STANDARD.decode(hash).ok()?;
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt).ok()?;
    mac.update(name.as_bytes());
    Some(mac.verify_slice(&hash).is_ok())
}

fn same_key(a: &PublicKey, b: &PublicKey) -> bool {
    a.key_data() == b.key_data()
}

/// The entry lines of `text`, numbered from 1, without blanks and comments.
fn lines(text: &str) -> impl Iterator<Item = Line<'_>> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| parse_line(i + 1, raw))
}

fn parse_line(number: usize, raw: &str) -> Option<Line<'_>> {
    let mut words = raw.split_whitespace();
    let mut first = words.next().filter(|w| !w.starts_with('#'))?;
    let marker = match first.strip_prefix('@') {
        Some("revoked") => Some(Marker::Revoked),
        Some("cert-authority") => Some(Marker::CertAuthority),
        Some(_) => Some(Marker::Other),
        None => None,
    };
    if marker.is_some() {
        first = words.next()?;
    }
    Some(Line {
        number,
        marker,
        hosts: first,
        key: words.collect::<Vec<_>>().join(" "),
    })
}

/// The file's text, or empty when it does not exist yet.
fn read_or_empty(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Replaces `path` with `text` through a temporary file in the same folder and a rename,
/// so a reader never sees half a file. The temporary file is created owner-only (0600).
fn replace_file(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, EcdsaCurve, PublicKey};
    use std::path::PathBuf;

    const A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEpl7XIrO05PhGnrhKwH5QLVhB9hgippRySbUyJFSjRd";
    const B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILY84+Y52dK/APwNyrey10YmkTK3XSb2Yat+Dg2fiIr5";
    const C: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOmyhsyFCAd8Gt+y7BUI2rA3ox6iEgcB+GL43Ke1tTam6G5RvUIgWAfKTIbKA60M9w+Jd2eFubQ9LNBd4FEfDNw=";
    /// `[db.example]:2222 A` after `ssh-keygen -H`.
    const HASHED_A: &str = "|1|q/klWJPn3OzreVZ88X+6H2po3N0=|IApEin9ZoTzCTXC0cefjE7W3PxI= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEpl7XIrO05PhGnrhKwH5QLVhB9hgippRySbUyJFSjRd";
    const ID: Identity<'static> = Identity {
        host: "db.example",
        port: 2222,
    };
    const USER: &str = "/h/.ssh/known_hosts";
    const OWN: &str = "/s/ssh_known_hosts";

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    fn files(user: &str, own: &str) -> Vec<KnownFile> {
        vec![
            KnownFile {
                path: PathBuf::from(USER),
                text: user.to_owned(),
            },
            KnownFile {
                path: PathBuf::from(OWN),
                text: own.to_owned(),
            },
        ]
    }

    fn verdict(user: &str, own: &str, presented: &str) -> Verdict {
        check(&files(user, own), &ID, &key(presented))
    }

    fn refused(file: &str, line: usize, problem: HostKeyProblem) -> Verdict {
        Verdict::Refused {
            file: PathBuf::from(file),
            line,
            problem,
        }
    }

    #[test]
    fn an_identity_names_its_port_unless_it_is_22() {
        assert_eq!(ID.name(), "[db.example]:2222");
        assert_eq!(
            Identity {
                host: "DB.Example",
                port: 22
            }
            .name(),
            "db.example"
        );
        assert_eq!(
            crate::paths::StatePaths::in_dir(std::path::Path::new("/s")).ssh_known_hosts,
            PathBuf::from(OWN)
        );
    }

    #[test]
    fn no_line_for_the_identity_is_unknown() {
        assert_eq!(verdict("", "", A), Verdict::Unknown);
        let others = format!("other.example {A}\n[db.example]:22 {A}\ndb.example {A}\n");
        assert_eq!(verdict(&others, "", A), Verdict::Unknown);
    }

    #[test]
    fn an_equal_key_in_either_file_is_trusted_with_comments_and_whitespace() {
        assert_eq!(
            verdict(&format!("[db.example]:2222 {A} me@laptop\n"), "", A),
            Verdict::Trusted
        );
        let own = format!("# mine\n\n[db.example]:2222\t  {A}\n");
        assert_eq!(verdict("", &own, A), Verdict::Trusted);
        assert_eq!(
            verdict(&format!("old.example,[DB.example]:2222 {A}\n"), "", A),
            Verdict::Trusted
        );
        assert_eq!(verdict(&format!("{HASHED_A}\n"), "", A), Verdict::Trusted);
    }

    #[test]
    fn a_different_key_of_any_algorithm_is_changed_naming_file_and_line() {
        let own = format!("# header\n[db.example]:2222 {B}\n");
        assert_eq!(
            verdict("", &own, A),
            refused(OWN, 2, HostKeyProblem::Changed)
        );
        let user = format!("[db.example]:2222 {C}\n");
        assert_eq!(
            verdict(&user, "", A),
            refused(USER, 1, HostKeyProblem::Changed)
        );
        // A host listing several keys is trusted when the presented one is among them.
        let own = format!("[db.example]:2222 {A}\n");
        assert_eq!(verdict(&user, &own, A), Verdict::Trusted);
    }

    #[test]
    fn a_revoked_key_refuses_whatever_its_host_field_says() {
        let trusted = format!("[db.example]:2222 {A}\n");
        for user in [
            format!("@revoked * {A}\n"),
            format!("@revoked [db.example]:2222 {A}\n"),
            format!("@revoked other.example {A}\n"),
        ] {
            assert_eq!(
                verdict(&user, &trusted, A),
                refused(USER, 1, HostKeyProblem::Revoked),
                "{user}"
            );
        }
        assert_eq!(
            verdict(&format!("@revoked * {B}\n"), &trusted, A),
            Verdict::Trusted
        );
    }

    #[test]
    fn wildcards_negations_and_cert_authorities_never_match() {
        for line in [
            format!("*.example {B}"),
            format!("[db.example]:* {B}"),
            format!("db.examp?e {B}"),
            format!("!other.example,[db.example]:2222 {B}"),
            format!("@cert-authority [db.example]:2222 {B}"),
        ] {
            assert_eq!(
                verdict(&format!("{line}\n"), "", A),
                Verdict::Unknown,
                "{line}"
            );
        }
    }

    #[test]
    fn an_unreadable_line_refuses_only_for_its_identity() {
        let bad = "[db.example]:2222 ssh-ed25519 !!!\n";
        let trusted = format!("[db.example]:2222 {A}\n");
        assert_eq!(
            verdict("", bad, A),
            refused(OWN, 1, HostKeyProblem::Unreadable)
        );
        assert_eq!(
            verdict(&trusted, bad, A),
            refused(OWN, 1, HostKeyProblem::Unreadable)
        );
        let elsewhere = "other.example ssh-ed25519 !!!\n|1|notbase64|x ssh-ed25519 AAAA\n";
        assert_eq!(verdict(elsewhere, &trusted, A), Verdict::Trusted);
        assert_eq!(
            verdict("@revoked * ssh-ed25519 !!!\n", &trusted, A),
            refused(USER, 1, HostKeyProblem::Unreadable)
        );
    }

    #[test]
    fn known_algorithms_follow_the_files_for_the_identity_only() {
        let user = format!("[db.example]:2222 {C}\nother.example {B}\n");
        let own = format!("[db.example]:2222 {A}\n[db.example]:2222 {B}\n");
        assert_eq!(
            known_algorithms(&files(&user, &own), &ID),
            vec![
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256
                },
                Algorithm::Ed25519
            ]
        );
    }

    #[test]
    fn trusting_appends_once_and_forgetting_removes_the_identity_only() {
        let text = with_trusted("# kept\nother.example ssh-ed25519 x", &ID, &key(A));
        assert_eq!(
            text,
            format!("# kept\nother.example ssh-ed25519 x\n[db.example]:2222 {A}\n")
        );
        assert_eq!(with_trusted(&text, &ID, &key(A)), text);
        let (left, removed) = without_identity(&format!("{text}{HASHED_A}\n"), &ID);
        assert_eq!(removed, 2);
        assert_eq!(left, "# kept\nother.example ssh-ed25519 x\n");
    }
}
