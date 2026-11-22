//! A loopback Potluck stand-in for team-mode tests: canned replies by method and path, event
//! streams held open until closed, and a seeded `file://` origin repository. Std only, so app
//! tests and pilot scenarios can share it without a runtime.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;

pub const TEAM: &str = "acme";
pub const WORKSPACE: &str = "shared";
pub const USER: &str = "ada";
pub const TOKEN_ID: i64 = 42;

/// What a route answers: a JSON body with its status, a bare status, or a held event stream.
#[derive(Clone)]
pub enum Reply {
    Json(u16, String),
    Status(u16),
    Stream,
}

#[derive(Default)]
struct Inner {
    routes: HashMap<(String, String), Reply>,
    streams: Vec<TcpStream>,
    hits: Vec<(String, String)>,
    closed: bool,
}

/// A listener on 127.0.0.1 answering each connection on its own thread.
pub struct Stub {
    uri: String,
    addr: SocketAddr,
    inner: Arc<Mutex<Inner>>,
}

impl Stub {
    pub fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let inner = Arc::new(Mutex::new(Inner::default()));
        let shared = Arc::clone(&inner);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if lock(&shared).closed {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(conn, &shared));
            }
        });
        Stub {
            uri: format!("http://{addr}"),
            addr,
            inner,
        }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Routes `verb path` to `reply`, replacing an earlier route for it.
    pub fn reply(&self, verb: &str, path: &str, reply: Reply) {
        lock(&self.inner)
            .routes
            .insert((verb.into(), path.into()), reply);
    }

    /// Writes `frame` to every open stream, dropping the ones that went away.
    pub fn send(&self, frame: &str) {
        lock(&self.inner)
            .streams
            .retain_mut(|s| s.write_all(frame.as_bytes()).is_ok());
    }

    pub fn close_streams(&self) {
        for s in lock(&self.inner).streams.drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    /// Requests answered for `verb path`, query ignored; a stream counts once it is registered
    /// to receive `send`.
    pub fn hits(&self, verb: &str, path: &str) -> usize {
        lock(&self.inner)
            .hits
            .iter()
            .filter(|(v, p)| v == verb && p == path)
            .count()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        lock(&self.inner).closed = true;
        self.close_streams();
        // Wake the accept loop so it sees `closed` and ends.
        let _ = TcpStream::connect(self.addr);
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads one request and answers it. The body is read in full first: closing a socket with
/// unread bytes sends a reset, and the client may lose the reply.
fn serve(mut conn: TcpStream, inner: &Mutex<Inner>) {
    let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
    while !head.ends_with(b"\r\n\r\n") {
        match conn.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if std::io::copy(&mut (&conn).take(length), &mut std::io::sink()).ok() != Some(length) {
        return;
    }
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let verb = first.next().unwrap_or("").to_owned();
    let path = first
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .to_owned();
    let reply = lock(inner)
        .routes
        .get(&(verb.clone(), path.clone()))
        .cloned();
    let (code, ctype, body) = match reply {
        Some(Reply::Stream) => {
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            );
            let mut g = lock(inner);
            g.streams.push(conn);
            g.hits.push((verb, path));
            return;
        }
        Some(Reply::Json(code, body)) => (code, "application/json", body),
        Some(Reply::Status(code)) => (code, "text/plain", String::new()),
        None => (404, "text/plain", String::new()),
    };
    lock(inner).hits.push((verb, path));
    let _ = write!(
        conn,
        "HTTP/1.1 {code} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = conn.shutdown(Shutdown::Write);
}

/// A token of Potluck's shape, `plk_` and 43 digits: the clock's nanos and a process-wide
/// counter, so two calls never match.
pub fn fresh_token() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = COUNT.fetch_add(1, Ordering::Relaxed);
    format!("plk_{nanos:024}{count:019}")
}

pub fn events_path() -> String {
    format!("/api/v1/workspaces/{TEAM}/{WORKSPACE}/events")
}

fn user() -> Value {
    json!({"username": USER, "display_name": USER, "email": null, "server_admin": false,
           "disabled": false, "created_at": "x"})
}

pub fn server_json() -> String {
    json!({"version": "0.3.0", "api": 1, "auth": {"password": true, "oidc": false},
           "kinds": {"dbento": {"formats": []}}})
    .to_string()
}

pub fn session_json(token: &str) -> String {
    json!({"token": token, "user": user(),
           "token_meta": {"id": TOKEN_ID, "name": "Dbento", "prefix": "plk_AAAA",
                          "created_at": "x", "last_used_at": null, "expires_at": null}})
    .to_string()
}

pub fn me_json() -> String {
    json!({"user": user(), "identity": {"name": USER, "email": "x@x"}, "workspaces": []})
        .to_string()
}

pub fn workspaces_json(git_url: &str, role: &str) -> String {
    json!([{"team": TEAM, "slug": WORKSPACE, "name": WORKSPACE, "kind": "dbento",
            "default_branch": "main", "role": role, "git_url": git_url,
            "events_url": format!("https://t.example{}", events_path())}])
    .to_string()
}

/// Routes a login, a sync and a logout need: GET server, POST session (201), GET me,
/// GET workspaces, GET events (Stream), DELETE /api/v1/tokens/42 (204).
pub fn serve_team(stub: &Stub, git_url: &str, role: &str) {
    stub.reply("GET", "/api/v1/server", Reply::Json(200, server_json()));
    stub.reply(
        "POST",
        "/api/v1/session",
        Reply::Json(201, session_json(&fresh_token())),
    );
    stub.reply("GET", "/api/v1/me", Reply::Json(200, me_json()));
    stub.reply(
        "GET",
        "/api/v1/workspaces",
        Reply::Json(200, workspaces_json(git_url, role)),
    );
    stub.reply("GET", &events_path(), Reply::Stream);
    stub.reply(
        "DELETE",
        &format!("/api/v1/tokens/{TOKEN_ID}"),
        Reply::Status(204),
    );
}

/// Writes `text` to `root/rel`, creating parent folders.
pub fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a file path has a parent")).expect("create folders");
    fs::write(path, text).expect("write the file");
}

/// A bare repository at `<scratch>/git/acme/shared.git`, the Potluck path shape, with every git
/// call isolated from the user's config and home.
pub struct Origin {
    dir: TempDir,
    bare: PathBuf,
    url: String,
}

impl Origin {
    /// The origin with `files` committed on `main`.
    pub fn seeded(files: &[(&str, &str)]) -> Origin {
        let dir = TempDir::new().expect("a scratch folder");
        fs::create_dir(dir.path().join("home")).expect("a scratch home");
        let bare = dir.path().join(format!("git/{TEAM}/{WORKSPACE}.git"));
        let url = format!("file://{}", bare.display());
        let origin = Origin { dir, bare, url };
        let bare = origin.bare.to_string_lossy().into_owned();
        origin.git(
            origin.root(),
            &["init", "-q", "--bare", "-b", "main", bare.as_str()],
        );
        let seed = origin.root().join("seed");
        origin.git(origin.root(), &["init", "-q", "-b", "main", "seed"]);
        for (rel, text) in files {
            write(&seed, rel, text);
        }
        origin.git(&seed, &["add", "-A"]);
        origin.git(&seed, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        origin.git(&seed, &["push", "-q", origin.url(), "main"]);
        origin
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn bare(&self) -> &Path {
        &self.bare
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The variables that keep git off the user's config: a scratch HOME, no global or system
    /// config.
    pub fn git_env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("HOME", self.root().join("home").into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ]
    }

    /// Runs git in `cwd` as `t <t@t.invalid>`, asserts success, returns stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.command(cwd, args).output().expect("git runs");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "git {args:?} failed: {err}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .envs(self.git_env())
            .stdin(Stdio::null());
        for who in ["AUTHOR", "COMMITTER"] {
            cmd.env(format!("GIT_{who}_NAME"), "t");
            cmd.env(format!("GIT_{who}_EMAIL"), "t@t.invalid");
        }
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
        ] {
            cmd.env_remove(name);
        }
        cmd
    }

    /// A clone at `<root>/<name>` on `main`.
    pub fn clone_to(&self, name: &str) -> PathBuf {
        self.git(
            self.root(),
            &["clone", "-q", "-b", "main", self.url(), name],
        );
        self.root().join(name)
    }

    /// `main:<rel>` in the bare repository, `None` when absent.
    pub fn show(&self, rel: &str) -> Option<String> {
        let spec = format!("main:{rel}");
        let out = self
            .command(self.bare(), &["show", spec.as_str()])
            .output()
            .expect("git runs");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `main`'s commit sha.
    pub fn tip(&self) -> String {
        self.git(self.bare(), &["rev-parse", "main"])
            .trim()
            .to_owned()
    }

    /// A teammate writes `rel`, commits and pushes from `<root>/mate`, cloned on first use.
    pub fn teammate(&self, rel: &str, text: &str) {
        let mate = self.root().join("mate");
        if mate.exists() {
            self.git(&mate, &["pull", "-q", "--ff-only", "origin", "main"]);
        } else {
            self.clone_to("mate");
        }
        self.commit_in(&mate, rel, text);
        self.git(&mate, &["push", "-q", "origin", "main"]);
    }

    /// Writes `rel` in clone `dir` and commits it there, without pushing.
    pub fn commit_in(&self, dir: &Path, rel: &str, text: &str) {
        write(dir, rel, text);
        self.git(dir, &["add", "-A"]);
        self.git(dir, &["commit", "-q", "-m", &format!("change {rel}")]);
    }

    /// An executable `#!/bin/sh` hook in the bare repository, written by a child shell so no
    /// descriptor of this process is open on it when another test thread forks.
    pub fn hook(&self, name: &str, body: &str) {
        let path = self.bare().join("hooks").join(name);
        fs::create_dir_all(path.parent().expect("a hook has a folder")).expect("the hooks folder");
        let status = Command::new("sh")
            .args([
                "-c",
                "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
                "sh",
            ])
            .arg(format!("#!/bin/sh\n{body}\n"))
            .arg(&path)
            .stdin(Stdio::null())
            .status()
            .expect("sh runs");
        assert!(status.success(), "the hook is written");
    }
}
