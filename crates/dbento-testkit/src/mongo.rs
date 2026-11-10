//! MongoDB harness: every `MgDb` is a seeded database with a reader and a writer, on its own
//! capped container or on the server `DBENTO_MONGO_URL` names. Mirrors the SQL Server harness
//! beside it.

use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use futures_util::TryStreamExt;
use mongodb::Client;
use mongodb::options::{ClientOptions, Credential, ServerAddress};
use testcontainers_modules::mongo::Mongo;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, ImageExt,
    core::{ExecCommand, IntoContainerPort, Mount},
    runners::AsyncRunner,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    ATTEMPT_LABEL, NEXT_ATTEMPT, NEXT_DB, engine, remove_labelled, retry_start, sweep_orphans,
};

/// The environment variable the generated connection reads its password from.
pub const MG_SECRET_VAR: &str = "DBENTO_TEST_MG_SECRET";

/// The gate image's tag.
pub const MONGO_TAG: &str = "8.0";

/// One canonical Extended JSON document per line, one per BSON type.
pub const TORTURE: &str = include_str!("../fixtures/mongo_torture.jsonl");

const URL_VAR: &str = "DBENTO_MONGO_URL";
const URL_FORM: &str = "mongodb://user:password@host:port";
const MG_ROOT_PASSWORD: &str = "DbentoMongoRoot8";
const MG_USER_PASSWORD: &str = "DbentoMongoUser8";
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const REPLICA_SET: &str = "rs0";

/// Who a client or URI logs in as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgUser {
    /// The server's administrator.
    Root,
    /// The `read` role on this database.
    Reader,
    /// The `readWrite` role on this database.
    Writer,
    /// No credential at all.
    Anonymous,
}

/// Where the MongoDB tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MgSource {
    /// A running server at this URL.
    Url(String),
    /// A container of the gate image.
    Container,
}

/// A non-empty `DBENTO_MONGO_URL` wins; anything else means a container.
pub fn mg_source(env: impl Fn(&str) -> Option<String>) -> MgSource {
    env(URL_VAR)
        .filter(|u| !u.is_empty())
        .map_or(MgSource::Container, MgSource::Url)
}

/// Host, port, user and password of `mongodb://user:password@host:port[/...]`, taken literally
/// (no percent-decoding).
pub fn mg_url(url: &str) -> Result<(String, u16, String, String), String> {
    let bad = || format!("bad MongoDB test URL: expected {URL_FORM}");
    let rest = url.strip_prefix("mongodb://").ok_or_else(bad)?;
    let (login, server) = rest.rsplit_once('@').ok_or_else(bad)?;
    let (user, password) = login.split_once(':').ok_or_else(bad)?;
    let server = server.split('/').next().unwrap_or_default();
    let (host, port) = server.rsplit_once(':').ok_or_else(bad)?;
    let port = port.parse::<u16>().map_err(|_| bad())?;
    if user.is_empty() || host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_owned(), port, user.to_owned(), password.to_owned()))
}

/// A seeded database. One holding a container must be dropped inside a tokio runtime; on a URL
/// server the database and both users are removed when the handle drops.
pub struct MgDb {
    host: String,
    port: u16,
    database: String,
    root: String,
    root_password: String,
    reader: String,
    writer: String,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<Mongo>>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl MgDb {
    /// The server's host.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// The server's TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
    /// This test database's name; read it from here, since a URL server suffixes it.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// The login name of `user`; `None` for `Anonymous`.
    pub fn user(&self, user: MgUser) -> Option<&str> {
        match user {
            MgUser::Root => Some(&self.root),
            MgUser::Reader => Some(&self.reader),
            MgUser::Writer => Some(&self.writer),
            MgUser::Anonymous => None,
        }
    }

    /// The password of `user`; `None` for `Anonymous`.
    pub fn password(&self, user: MgUser) -> Option<&str> {
        match user {
            MgUser::Root => Some(&self.root_password),
            MgUser::Reader | MgUser::Writer => Some(MG_USER_PASSWORD),
            MgUser::Anonymous => None,
        }
    }

    /// No password in the text: `mongodb://reader@host:port/app?authSource=admin`;
    /// `Anonymous` is `mongodb://host:port/app`.
    pub fn uri(&self, user: MgUser) -> String {
        let Self { host, port, .. } = self;
        let database = &self.database;
        match self.user(user) {
            Some(name) => format!("mongodb://{name}@{host}:{port}/{database}?authSource=admin"),
            None => format!("mongodb://{host}:{port}/{database}"),
        }
    }

    /// `[connections.<name>]` with engine "mongodb", `uri`, `secret = { env = MG_SECRET_VAR }`
    /// unless `Anonymous`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, user: MgUser, extra: &str) -> String {
        let secret = match user {
            MgUser::Anonymous => String::new(),
            MgUser::Root | MgUser::Reader | MgUser::Writer => {
                format!("secret = {{ env = \"{MG_SECRET_VAR}\" }}\n")
            }
        };
        format!(
            "[connections.{name}]\nengine = \"mongodb\"\nuri = \"{}\"\n{secret}{extra}\n",
            self.uri(user)
        )
    }

    /// A client logged in as `user`.
    pub async fn client(&self, user: MgUser) -> Client {
        let credential = self
            .user(user)
            .zip(self.password(user))
            .map(|(name, password)| credential(name, password));
        Client::with_options(options(&self.host, self.port, credential))
            .expect("cannot build the MongoDB test client")
    }

    /// Runs `command` on this database as root; panics on a command error, or on `writeErrors`
    /// or `writeConcernError`, which an `ok: 1` reply carries as a success.
    pub async fn run(&self, command: Document) -> Document {
        let database = self.client(MgUser::Root).await.database(&self.database);
        let reply = database
            .run_command(command)
            .await
            .unwrap_or_else(|e| panic!("MongoDB test command failed: {}", e.kind));
        for key in ["writeErrors", "writeConcernError"] {
            if let Some(failure) = reply.get(key) {
                panic!("MongoDB test command failed: {key}: {failure}");
            }
        }
        reply
    }

    /// The number of documents in `collection`, as root.
    pub async fn count(&self, collection: &str) -> u64 {
        let database = self.client(MgUser::Root).await.database(&self.database);
        database
            .collection::<Document>(collection)
            .count_documents(doc! {})
            .await
            .unwrap_or_else(|e| panic!("cannot count {collection}: {}", e.kind))
    }

    /// The filesystem type at `/data/db` in the container; `None` on a URL server.
    pub async fn data_filesystem(&self) -> Option<String> {
        let command = ExecCommand::new(["stat", "-f", "-c", "%T", "/data/db"]);
        let mut result = self
            .container
            .as_ref()?
            .exec(command)
            .await
            .expect("cannot run stat in the MongoDB test container");
        let stdout = result.stdout_to_vec().await.expect("no stdout from stat");
        Some(String::from_utf8_lossy(&stdout).trim().to_owned())
    }

    /// The cursors open on this database's collections, whoever owns them. Counted from
    /// `$currentOp`, not `serverStatus`, whose total spans a shared `DBENTO_MONGO_URL` server.
    pub async fn open_cursors(&self) -> i64 {
        self.cursor_ops().await.len() as i64
    }

    /// `$currentOp`'s entries for this database that hold a cursor, idle or in a `getMore`.
    async fn cursor_ops(&self) -> Vec<Document> {
        // Root's `$currentOp` lists only root's own cursors unless `allUsers` widens it.
        let admin = self.client(MgUser::Root).await.database("admin");
        let pipeline = [
            doc! { "$currentOp": { "idleCursors": true, "allUsers": true } },
            doc! { "$match": { "cursor": { "$exists": true } } },
        ];
        fn failed<T>(e: mongodb::error::Error) -> T {
            panic!("cannot list cursors: {}", e.kind)
        }
        let mut listed = admin.aggregate(pipeline).await.unwrap_or_else(failed);
        let prefix = format!("{}.", self.database);
        let mut ops = Vec::new();
        while let Some(op) = listed.try_next().await.unwrap_or_else(failed) {
            if op.get_str("ns").is_ok_and(|ns| ns.starts_with(&prefix)) {
                ops.push(op);
            }
        }
        ops
    }

    /// Kills every idle cursor on this database's collections, whoever owns it; returns how many.
    pub async fn kill_idle_cursors(&self) -> usize {
        let prefix = format!("{}.", self.database);
        let mut killed = 0;
        for op in self.cursor_ops().await {
            let namespace = op.get_str("ns").unwrap_or_default();
            let (Ok("idleCursor"), Some(collection)) =
                (op.get_str("type"), namespace.strip_prefix(&prefix))
            else {
                continue;
            };
            let id = op
                .get_document("cursor")
                .and_then(|c| c.get_i64("cursorId"))
                .unwrap_or_else(|e| panic!("no cursor id in {op:?}: {e}"));
            let reply = self
                .run(doc! { "killCursors": collection, "cursors": [id] })
                .await;
            killed += reply.get_array("cursorsKilled").map_or(0, Vec::len);
        }
        killed
    }
}

impl Drop for MgDb {
    // Runs on its own thread and runtime and never panics. A container is killed, which takes
    // everything in it along; on a URL server the database and both users are removed.
    fn drop(&mut self) {
        let container = self.container.as_ref().map(|c| c.id().to_owned());
        let options = options(
            &self.host,
            self.port,
            Some(credential(&self.root, &self.root_password)),
        );
        let database = self.database.clone();
        let users = [self.reader.clone(), self.writer.clone()];
        let done = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                if let Some(id) = container {
                    return engine()?
                        .kill_container(&id, None)
                        .await
                        .map_err(|e| format!("cannot kill container {id}: {e}"));
                }
                let client = Client::with_options(options).map_err(|e| e.kind.to_string())?;
                client
                    .database(&database)
                    .run_command(doc! { "dropDatabase": 1 })
                    .await
                    .map_err(|e| e.kind.to_string())?;
                let admin = client.database("admin");
                for user in users {
                    admin
                        .run_command(doc! { "dropUser": user })
                        .await
                        .map_err(|e| e.kind.to_string())?;
                }
                Ok(())
            })
        })
        .join();
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop {}: {e}", self.database),
            Err(_) => eprintln!("dbento-testkit: dropping {} panicked", self.database),
        }
    }
}

fn credential(user: &str, password: &str) -> Credential {
    Credential::builder()
        .username(user.to_owned())
        .password(password.to_owned())
        .source("admin".to_owned())
        .build()
}

fn options(host: &str, port: u16, credential: Option<Credential>) -> ClientOptions {
    let mut options = ClientOptions::builder()
        .hosts(vec![ServerAddress::Tcp {
            host: host.to_owned(),
            port: Some(port),
        }])
        .build();
    options.credential = credential;
    options
}

/// The `TORTURE` lines as documents.
pub(crate) fn torture_documents() -> Vec<Document> {
    TORTURE
        .lines()
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            match Bson::try_from(value) {
                Ok(Bson::Document(document)) => document,
                other => panic!("{line}: {other:?}"),
            }
        })
        .collect()
}

/// A seeded database; the `reader` (read) and `writer` (readWrite) users; collections `many`
/// (10,000 `{_id: i, n: i, s: "row <i>"}`, `i` an Int32 from 0) and `torture` (the `TORTURE`
/// lines).
pub async fn mongo_db() -> MgDb {
    match mg_source(|k| std::env::var(k).ok()) {
        MgSource::Url(url) => {
            let (host, port, user, password) = mg_url(&url).unwrap_or_else(|e| panic!("{e}"));
            let suffix = format!(
                "_{}_{}",
                std::process::id(),
                NEXT_DB.fetch_add(1, Ordering::Relaxed)
            );
            seeded(&host, port, &user, &password, &suffix).await
        }
        MgSource::Container => container().await,
    }
}

/// A one-node replica set with no users, always in a container. Its only member is the primary,
/// so a read that must reach a secondary has nowhere to go, and it can open change streams.
pub struct MgReplicaSet {
    port: u16,
    // Declared before the permit, so the container is gone before another may start.
    _container: ContainerAsync<Mongo>,
    _permit: OwnedSemaphorePermit,
}

impl MgReplicaSet {
    /// `mongodb://host:port/app?replicaSet=..&<query>`, which discovers the set and its roles.
    pub fn uri(&self, query: &str) -> String {
        format!(
            "mongodb://127.0.0.1:{}/app?replicaSet={REPLICA_SET}&{query}",
            self.port
        )
    }

    /// Runs `command` on `app`; panics on a command error.
    pub async fn run(&self, command: Document) -> Document {
        let client =
            Client::with_options(direct(self.port)).expect("cannot build the MongoDB test client");
        client
            .database("app")
            .run_command(command)
            .await
            .unwrap_or_else(|e| panic!("MongoDB test command failed: {}", e.kind))
    }
}

/// Starts a one-node replica set. The member's address must work both inside the container and
/// from the host, so mongod listens inside on the very port the host maps.
pub async fn mongo_replica_set() -> MgReplicaSet {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let port = Cell::new(0);
    let container = start(|| {
        port.set(free_port());
        let mut args = cache_args();
        args.extend(["--replSet", REPLICA_SET, "--port"].map(str::to_owned));
        args.push(port.get().to_string());
        capped(args).with_mapped_port(port.get(), port.get().tcp())
    })
    .await;
    let port = port.get();
    wait_primary(port).await;
    MgReplicaSet {
        port,
        _container: container,
        _permit: permit,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("no free local port")
        .port()
}

// Talks to the one member itself, since a set's name means nothing before it is initiated.
fn direct(port: u16) -> ClientOptions {
    let mut options = options("127.0.0.1", port, None);
    options.direct_connection = Some(true);
    options.server_selection_timeout = Some(Duration::from_secs(2));
    options
}

/// Initiates the set, then waits until its member has elected itself primary.
async fn wait_primary(port: u16) {
    let client = Client::with_options(direct(port)).expect("cannot build the MongoDB test client");
    let admin = client.database("admin");
    let member = format!("127.0.0.1:{port}");
    let config = doc! { "_id": REPLICA_SET, "members": [{ "_id": 0, "host": member }] };
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        // A repeat of `replSetInitiate` only fails with `AlreadyInitialized`.
        let _ = admin
            .run_command(doc! { "replSetInitiate": config.clone() })
            .await;
        if let Ok(hello) = admin.run_command(doc! { "hello": 1 }).await
            && hello.get_bool("isWritablePrimary").unwrap_or(false)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the MongoDB replica set never elected a primary");
}

/// Two MongoDB containers at a time in this process; each takes 1 GB.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(2))))
}

async fn container() -> MgDb {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let c = start(|| {
        capped(cache_args())
            .with_env_var("MONGO_INITDB_ROOT_USERNAME", "root")
            .with_env_var("MONGO_INITDB_ROOT_PASSWORD", MG_ROOT_PASSWORD)
    })
    .await;
    let port = c
        .get_host_port_ipv4(27017)
        .await
        .expect("no mapped MongoDB port");
    let host = "127.0.0.1";
    wait_ready(host, port).await;
    let mut db = seeded(host, port, "root", MG_ROOT_PASSWORD, "").await;
    db.container = Some(c);
    db._permit = Some(permit);
    db
}

/// The gate image, memory-capped and on tmpfs, running mongod with `args`.
fn capped(args: Vec<String>) -> ContainerRequest<Mongo> {
    Mongo::default()
        .with_tag(MONGO_TAG)
        // The image turns glibc's rseq off so TCMalloc takes it for per-CPU caches, which MongoDB
        // 8.0 refuses on Linux 6.19+ (SERVER-121912); letting glibc keep rseq selects per-thread caches.
        .with_env_var("GLIBC_TUNABLES", "glibc.pthread.rseq=1")
        .with_cmd(args)
        // Sized, because an unsized tmpfs reports 0 bytes free and index builds then refuse.
        .with_mount(
            Mount::tmpfs_mount("/data/db")
                .with_mode(0o1777)
                .with_size_bytes(1 << 30),
        )
        .with_host_config_modifier(|h| h.memory = Some(1 << 30))
}

fn cache_args() -> Vec<String> {
    vec!["--wiredTigerCacheSizeGB".to_owned(), "0.25".to_owned()]
}

/// Starts `request` labelled, retried when its host port turns out to be taken.
async fn start(mut request: impl FnMut() -> ContainerRequest<Mongo>) -> ContainerAsync<Mongo> {
    retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = request()
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
        async move {
            match request.start().await {
                Ok(container) => Ok(container),
                Err(e) => {
                    remove_labelled(&format!("{ATTEMPT_LABEL}={attempt}")).await;
                    Err(e.to_string())
                }
            }
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "cannot start the MongoDB test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    })
}

/// Pings as root until the real server answers. The module's log wait fires at the image's init
/// server, which binds only inside the container and is then restarted.
async fn wait_ready(host: &str, port: u16) {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let mut options = options(host, port, Some(credential("root", MG_ROOT_PASSWORD)));
        options.server_selection_timeout = Some(Duration::from_secs(2));
        let client = Client::with_options(options).expect("cannot build the MongoDB test client");
        let admin = client.database("admin");
        match admin.run_command(doc! { "ping": 1 }).await {
            Ok(_) => return,
            Err(e) if Instant::now() >= deadline => {
                panic!("the MongoDB test container never answered: {}", e.kind)
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Creates the users and seeds the collections; every name carries `suffix`.
async fn seeded(host: &str, port: u16, root: &str, password: &str, suffix: &str) -> MgDb {
    let db = MgDb {
        host: host.to_owned(),
        port,
        database: format!("app{suffix}"),
        root: root.to_owned(),
        root_password: password.to_owned(),
        reader: format!("reader{suffix}"),
        writer: format!("writer{suffix}"),
        container: None,
        _permit: None,
    };
    let admin = db.client(MgUser::Root).await.database("admin");
    for (user, role) in [(&db.reader, "read"), (&db.writer, "readWrite")] {
        let command = doc! {
            "createUser": user,
            "pwd": MG_USER_PASSWORD,
            "roles": [{ "role": role, "db": &db.database }],
        };
        admin
            .run_command(command)
            .await
            .unwrap_or_else(|e| panic!("cannot create {user}: {}", e.kind));
    }
    let many: Vec<Document> = (0..10_000)
        .map(|i: i32| doc! { "_id": i, "n": i, "s": format!("row {i}") })
        .collect();
    db.run(doc! { "insert": "many", "documents": many }).await;
    db.run(doc! { "insert": "torture", "documents": torture_documents() })
        .await;
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn a_url_wins_and_anything_else_is_a_container() {
        let url = "mongodb://root:pw@127.0.0.1:27017";
        assert_eq!(
            mg_source(env(&[("DBENTO_MONGO_URL", url)])),
            MgSource::Url(url.to_owned())
        );
        assert_eq!(
            mg_source(env(&[("DBENTO_MONGO_URL", "")])),
            MgSource::Container
        );
        assert_eq!(mg_source(env(&[])), MgSource::Container);
    }

    #[test]
    fn a_url_gives_host_port_user_and_password() {
        assert_eq!(
            mg_url("mongodb://root:pw@127.0.0.1:27117/admin").unwrap(),
            (
                "127.0.0.1".to_owned(),
                27117,
                "root".to_owned(),
                "pw".to_owned()
            )
        );
        for bad in [
            "postgres://a:b@h:1",
            "mongodb://root@h:1",
            "mongodb://r:p@h",
            "mongodb://r:p@h:x",
        ] {
            assert!(
                mg_url(bad)
                    .unwrap_err()
                    .contains("mongodb://user:password@host:port"),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_bad_url_never_repeats_its_password() {
        let failure = mg_url("mongodb://root:s3cret@h").unwrap_err();
        assert!(!failure.contains("s3cret"), "{failure}");
    }

    #[test]
    fn every_fixture_line_parses_and_ids_ascend() {
        let docs = torture_documents();
        assert_eq!(docs.len(), TORTURE.lines().count());
        for (i, doc) in docs.iter().enumerate() {
            assert_eq!(doc.get_i32("_id").unwrap(), i as i32 + 1);
            assert!(doc.get_str("type").is_ok());
        }
    }
}
