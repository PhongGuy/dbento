//! Redis harness: every `RedisDb` is a seeded DB index with a reader and a writer, on its own
//! capped Valkey container or on the server `DBENTO_REDIS_URL` names. Mirrors `mongo.rs`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ::redis::aio::MultiplexedConnection;
use ::redis::{AsyncConnectionConfig, Client, Cmd, IntoConnectionInfo, RedisConnectionInfo, Value};
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, ImageExt, runners::AsyncRunner,
};
use testcontainers_modules::valkey::Valkey;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    ATTEMPT_LABEL, NEXT_ATTEMPT, NEXT_DB, engine, remove_labelled, retry_start, sweep_orphans,
};

/// The environment variable the generated connection reads its password from.
pub const RD_SECRET_VAR: &str = "DBENTO_TEST_RD_SECRET";

/// The gate image's tag.
pub const VALKEY_TAG: &str = "9.0.7-alpine";

/// The spec's reader ACL, after name and password.
pub const READER_RULES: &str = "~* &* -@all +@read +ping +echo +time +hello +select +command +multi +discard +client|setname +client|setinfo +client|id +client|info";

/// The key and value of the seed's binary string: neither is UTF-8.
pub const BINARY_KEY: &[u8] = b"bin:\xff\x00name";
pub const BINARY_VALUE: &[u8] = b"\x00\xff\xfeA";
/// `big:list` holds "0".."99999".
pub const BIG_LIST_LEN: usize = 100_000;
/// `big:string` is 'x' repeated 2 MiB.
pub const BIG_STRING_LEN: usize = 2 << 20;
/// `big:hash` f{i}=v{i}, `big:set` m{i}, `big:zset` m{i} scored i, `big:stream` ids 1-1..1-3000.
pub const BIG_LEN: usize = 3_000;
/// `many:{i}` = i.
pub const MANY_KEYS: usize = 2_000;
/// Every key the seed writes: 16 named ones plus `MANY_KEYS`.
pub const SEED_KEYS: u64 = 2_016;

const URL_VAR: &str = "DBENTO_REDIS_URL";
const URL_FORM: &str = "redis://[user[:password]@]host:port[/index]";
const RD_PASSWORD: &str = "DbentoRedis9";
const VALKEY_PORT: u16 = 6379;
// The client's own 500 ms default is too short for the big seed commands.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Who a connection logs in as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdUser {
    /// The default user in a container, else the URL's login.
    Admin,
    /// `READER_RULES`.
    Reader,
    /// `~* &* +@all`.
    Writer,
}

/// Where the Redis tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdSource {
    /// A running server at this URL.
    Url(String),
    /// A container of the gate image.
    Container,
}

/// A non-empty `DBENTO_REDIS_URL` wins; anything else means a container.
pub fn rd_source(env: impl Fn(&str) -> Option<String>) -> RdSource {
    env(URL_VAR)
        .filter(|u| !u.is_empty())
        .map_or(RdSource::Container, RdSource::Url)
}

/// Host, port, user, password and DB index of a test URL.
pub type RdUrl = (String, u16, Option<String>, Option<String>, u32);

/// Parses the parts of `redis://[user[:password]@]host:port[/index]`,
/// taken literally (no percent-decoding); the index defaults to 0.
pub fn rd_url(url: &str) -> Result<RdUrl, String> {
    let bad = || format!("bad Redis test URL: expected {URL_FORM}");
    let rest = url.strip_prefix("redis://").ok_or_else(bad)?;
    let (login, server) = match rest.rsplit_once('@') {
        Some((login, server)) => (Some(login), server),
        None => (None, rest),
    };
    let (user, password) = match login.map(|l| l.split_once(':').unwrap_or((l, ""))) {
        Some((user, password)) => (
            Some(user.to_owned()).filter(|u| !u.is_empty()),
            Some(password.to_owned()).filter(|p| !p.is_empty()),
        ),
        None => (None, None),
    };
    let (address, index) = server.split_once('/').unwrap_or((server, ""));
    let (host, port) = address.rsplit_once(':').ok_or_else(bad)?;
    let port = port.parse::<u16>().map_err(|_| bad())?;
    let index = match index {
        "" => 0,
        i => i.parse::<u32>().map_err(|_| bad())?,
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_owned(), port, user, password, index))
}

/// A seeded DB index. One holding a container must be dropped inside a tokio runtime; on a URL
/// server the index is flushed and this handle's users removed when it drops.
pub struct RedisDb {
    host: String,
    port: u16,
    index: u32,
    admin: Option<String>,
    admin_password: Option<String>,
    reader: String,
    writer: String,
    created: Mutex<Vec<String>>,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<Valkey>>,
    _permit: OwnedSemaphorePermit,
}

impl RedisDb {
    /// The server's host.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// The server's TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
    /// The DB index the seed is on, and the tests' to wipe.
    pub fn index(&self) -> u32 {
        self.index
    }
    /// True in a container, where server-wide state belongs to this handle alone.
    pub fn exclusive(&self) -> bool {
        self.container.is_some()
    }

    /// The login of `u`; `None` is the default user.
    pub fn user(&self, u: RdUser) -> Option<&str> {
        match u {
            RdUser::Admin => self.admin.as_deref(),
            RdUser::Reader => Some(&self.reader),
            RdUser::Writer => Some(&self.writer),
        }
    }

    /// The password of `u`; `None` when it logs in without one.
    pub fn password(&self, u: RdUser) -> Option<&str> {
        match u {
            RdUser::Admin => self.admin_password.as_deref(),
            RdUser::Reader | RdUser::Writer => Some(RD_PASSWORD),
        }
    }

    /// `[connections.<name>]`: engine, host, port, database = "<index>", `user` unless None,
    /// `secret = { env = RD_SECRET_VAR }` or `"none"`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, u: RdUser, extra: &str) -> String {
        let user = self
            .user(u)
            .map(|user| format!("user = \"{user}\"\n"))
            .unwrap_or_default();
        let secret = match self.password(u) {
            Some(_) => format!("secret = {{ env = \"{RD_SECRET_VAR}\" }}\n"),
            None => "secret = \"none\"\n".to_owned(),
        };
        self.table(name, &user, &secret, extra)
    }

    /// `[connections.<name>]` logging in as `login`, its password read from `RD_SECRET_VAR`.
    pub fn connection_toml_as(&self, name: &str, login: &str, extra: &str) -> String {
        let user = format!("user = \"{login}\"\n");
        let secret = format!("secret = {{ env = \"{RD_SECRET_VAR}\" }}\n");
        self.table(name, &user, &secret, extra)
    }

    fn table(&self, name: &str, user: &str, secret: &str, extra: &str) -> String {
        let Self {
            host, port, index, ..
        } = self;
        format!(
            "[connections.{name}]\nengine = \"redis\"\nhost = \"{host}\"\nport = {port}\n\
             database = \"{index}\"\n{user}{secret}{extra}\n"
        )
    }

    /// A connection logged in as `u`, on `index`.
    pub async fn connection(&self, u: RdUser) -> MultiplexedConnection {
        login(
            &self.host,
            self.port,
            self.user(u),
            self.password(u),
            self.index,
        )
        .await
        .unwrap_or_else(|e| panic!("cannot connect to the Redis test server as {u:?}: {e}"))
    }

    /// Runs `args` as Admin; a server error comes back as `Value::ServerError`.
    pub async fn cmd(&self, args: &[&str]) -> Value {
        let raw: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        self.cmd_raw(&raw).await
    }

    /// `cmd` with binary arguments.
    pub async fn cmd_raw(&self, args: &[&[u8]]) -> Value {
        let mut cmd = Cmd::new();
        for arg in args {
            cmd.arg(*arg);
        }
        self.connection(RdUser::Admin)
            .await
            .send_packed_command(&cmd)
            .await
            .unwrap_or_else(|e| panic!("Redis test command failed: {e}"))
    }

    /// The integer reply of `args`; panics on any other reply.
    pub async fn int(&self, args: &[&str]) -> i64 {
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "Value has too many variants to list; the rest share one arm"
        )]
        match self.cmd(args).await {
            Value::Int(n) => n,
            other => panic!("{args:?} gave {other:?}, not an integer"),
        }
    }

    /// The text of a bulk, simple, verbatim or OK reply; `None` for Nil; panics on any other.
    pub async fn text(&self, args: &[&str]) -> Option<String> {
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "Value has too many variants to list; the rest share one arm"
        )]
        match self.cmd(args).await {
            Value::BulkString(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => Some(s),
            Value::Okay => Some("OK".to_owned()),
            Value::Nil => None,
            other => panic!("{args:?} gave {other:?}, not text"),
        }
    }

    /// `ACL SETUSER <base><suffix> reset on >pw <rules>`; removed when the handle drops.
    pub async fn create_user(&self, base: &str, rules: &str) -> (String, String) {
        let name = format!("{base}{}", suffix());
        self.set_user(&name, rules).await;
        self.created
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(name.clone());
        (name, RD_PASSWORD.to_owned())
    }

    async fn set_user(&self, name: &str, rules: &str) {
        let pw = format!(">{RD_PASSWORD}");
        let mut args = vec!["ACL", "SETUSER", name, "reset", "on", &pw];
        args.extend(rules.split_whitespace());
        if let Value::ServerError(e) = self.cmd(&args).await {
            panic!("cannot create Redis user {name}: {e:?}");
        }
    }

    /// Writes the type seed with one Admin pipeline per kind.
    async fn seed(&self) {
        let mut admin = self.connection(RdUser::Admin).await;
        let mut named = ::redis::pipe();
        named
            .cmd("FLUSHDB")
            .cmd("SET")
            .arg("str")
            .arg("hello")
            .cmd("SET")
            .arg("utf8")
            .arg("grüße ✓")
            .cmd("SET")
            .arg("int")
            .arg(42)
            .cmd("SET")
            .arg(BINARY_KEY)
            .arg(BINARY_VALUE)
            .cmd("HSET")
            .arg("hash")
            .arg(&["name", "Ada", "age", "36"])
            .cmd("RPUSH")
            .arg("list")
            .arg(&["a", "b", "c"])
            .cmd("SADD")
            .arg("set")
            .arg(&["x", "y", "z"])
            .cmd("ZADD")
            .arg("zset")
            .arg(1.5)
            .arg("a")
            .arg(2.5)
            .arg("b")
            .cmd("XADD")
            .arg("stream")
            .arg("1-1")
            .arg("f")
            .arg("v")
            .cmd("XADD")
            .arg("stream")
            .arg("1-2")
            .arg("g")
            .arg("w")
            .cmd("SET")
            .arg("ttl")
            .arg("t")
            .arg("PX")
            .arg(3_600_000)
            .cmd("RPUSH")
            .arg("big:list")
            .arg((0..BIG_LIST_LEN).collect::<Vec<_>>())
            .cmd("SET")
            .arg("big:string")
            .arg("x".repeat(BIG_STRING_LEN));
        let mut big = ::redis::pipe();
        let hash = big.cmd("HSET").arg("big:hash");
        for i in 0..BIG_LEN {
            hash.arg(format!("f{i}")).arg(format!("v{i}"));
        }
        let set = big.cmd("SADD").arg("big:set");
        for i in 0..BIG_LEN {
            set.arg(format!("m{i}"));
        }
        let zset = big.cmd("ZADD").arg("big:zset");
        for i in 0..BIG_LEN {
            zset.arg(i).arg(format!("m{i}"));
        }
        let many = big.cmd("MSET");
        for i in 0..MANY_KEYS {
            many.arg(format!("many:{i}")).arg(i);
        }
        let mut stream = ::redis::pipe();
        for i in 1..=BIG_LEN {
            stream
                .cmd("XADD")
                .arg("big:stream")
                .arg(format!("1-{i}"))
                .arg("n")
                .arg(i);
        }
        for pipe in [named, big, stream] {
            pipe.exec_async(&mut admin)
                .await
                .unwrap_or_else(|e| panic!("cannot seed the Redis test index: {e}"));
        }
    }
}

impl Drop for RedisDb {
    // Runs on its own thread and runtime and never panics. A container is killed, which takes
    // everything in it along; on a URL server the index is flushed and the users removed.
    fn drop(&mut self) {
        let container = self.container.as_ref().map(|c| c.id().to_owned());
        let (host, port, index) = (self.host.clone(), self.port, self.index);
        let (admin, password) = (self.admin.clone(), self.admin_password.clone());
        let mut users = vec![self.reader.clone(), self.writer.clone()];
        users.extend(std::mem::take(
            &mut *self.created.lock().unwrap_or_else(|p| p.into_inner()),
        ));
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
                let mut c = login(&host, port, admin.as_deref(), password.as_deref(), index)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut cleanup = ::redis::pipe();
                cleanup.cmd("FLUSHDB").cmd("ACL").arg("DELUSER").arg(users);
                cleanup.exec_async(&mut c).await.map_err(|e| e.to_string())
            })
        })
        .join();
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop Redis index {index}: {e}"),
            Err(_) => eprintln!("dbento-testkit: dropping Redis index {index} panicked"),
        }
    }
}

/// `_<pid>_<n>`, unique within this process.
fn suffix() -> String {
    format!(
        "_{}_{}",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed)
    )
}

async fn login(
    host: &str,
    port: u16,
    user: Option<&str>,
    password: Option<&str>,
    index: u32,
) -> ::redis::RedisResult<MultiplexedConnection> {
    let mut settings = RedisConnectionInfo::default().set_db(index.into());
    if let Some(user) = user {
        settings = settings.set_username(user);
    }
    if let Some(password) = password {
        settings = settings.set_password(password);
    }
    let info = (host, port)
        .into_connection_info()?
        .set_redis_settings(settings);
    let config = AsyncConnectionConfig::new()
        .set_response_timeout(Some(RESPONSE_TIMEOUT))
        .set_connection_timeout(Some(CONNECT_TIMEOUT));
    Client::open(info)?
        .get_multiplexed_async_connection_with_config(&config)
        .await
}

/// A seeded index with users `dbento_ro` (`READER_RULES`) and `writer` (`~* &* +@all`), both
/// suffixed on a URL server; see the constants for the seed.
pub async fn redis_db() -> RedisDb {
    match rd_source(|k| std::env::var(k).ok()) {
        RdSource::Url(url) => {
            let (host, port, admin, password, index) =
                rd_url(&url).unwrap_or_else(|e| panic!("{e}"));
            let permit = url_slot()
                .acquire_owned()
                .await
                .expect("the URL semaphore is never closed");
            let suffix = suffix();
            seeded(host, port, admin, password, index, &suffix, None, permit).await
        }
        RdSource::Container => container().await,
    }
}

/// Two Valkey containers at a time in this process.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(2))))
}

/// One handle at a time on a URL server, since they all share its one index.
fn url_slot() -> Arc<Semaphore> {
    static SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOT.get_or_init(|| Arc::new(Semaphore::new(1))))
}

async fn container() -> RedisDb {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let c = start().await;
    let port = c
        .get_host_port_ipv4(VALKEY_PORT)
        .await
        .expect("no mapped Valkey port");
    seeded(
        "127.0.0.1".to_owned(),
        port,
        None,
        None,
        0,
        "",
        Some(c),
        permit,
    )
    .await
}

/// The gate image, memory-capped, with persistence off.
fn capped() -> ContainerRequest<Valkey> {
    Valkey::default()
        .with_tag(VALKEY_TAG)
        .with_cmd(["valkey-server", "--save", "", "--appendonly", "no"])
        .with_host_config_modifier(|h| h.memory = Some(512 << 20))
}

/// Starts the gate image labelled, retried when its host port turns out to be taken.
async fn start() -> ContainerAsync<Valkey> {
    retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = capped()
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
            "cannot start the Valkey test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    })
}

/// Creates the users and seeds the index; user names carry `suffix`.
#[allow(clippy::too_many_arguments)]
async fn seeded(
    host: String,
    port: u16,
    admin: Option<String>,
    admin_password: Option<String>,
    index: u32,
    suffix: &str,
    container: Option<ContainerAsync<Valkey>>,
    permit: OwnedSemaphorePermit,
) -> RedisDb {
    let db = RedisDb {
        host,
        port,
        index,
        admin,
        admin_password,
        reader: format!("dbento_ro{suffix}"),
        writer: format!("writer{suffix}"),
        created: Mutex::new(Vec::new()),
        container,
        _permit: permit,
    };
    db.set_user(&db.reader, READER_RULES).await;
    db.set_user(&db.writer, "~* &* +@all").await;
    db.seed().await;
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
        let url = "redis://u:p@h:1/2";
        assert_eq!(
            rd_source(env(&[("DBENTO_REDIS_URL", url)])),
            RdSource::Url(url.to_owned())
        );
        assert_eq!(
            rd_source(env(&[("DBENTO_REDIS_URL", "")])),
            RdSource::Container
        );
        assert_eq!(rd_source(env(&[])), RdSource::Container);
    }

    #[test]
    fn a_url_gives_its_parts_and_defaults_the_index() {
        assert_eq!(
            rd_url("redis://admin:pw@db.test:6390/4").unwrap(),
            (
                "db.test".to_owned(),
                6390,
                Some("admin".to_owned()),
                Some("pw".to_owned()),
                4
            )
        );
        assert_eq!(
            rd_url("redis://h:6379").unwrap(),
            ("h".to_owned(), 6379, None, None, 0)
        );
        assert!(rd_url("mongodb://h:1").is_err());
        assert!(rd_url("redis://h:notaport").is_err());
    }
}
