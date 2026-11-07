#![allow(dead_code)]
use dbento_core::{
    Applied, ApplyError, CellRef, Change, ChangeOutcome, ConnectError, ConnectionConfig, EditBatch,
    EditTarget, EditValue, ExecuteError, Limits, Outcome, Pending, RelationRef, RowKey, Secret,
    Session, SessionOptions, Workspace, apply, connect,
};
#[cfg(feature = "mongodb")]
use dbento_testkit::{MgDb, MgUser};

/// 5,000 rows: enough to fill several batches.
pub const MANY: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r";
/// Never finishes on its own.
pub const LONG: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT count(*) FROM r";

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

/// A workspace with one SQLite connection `lite` over `data/app.db`, created by running
/// `setup`; `extra_toml` lines are appended to the connection's table.
pub fn sqlite_fixture(setup: &str, extra_toml: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    rusqlite::Connection::open(dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch(setup)
        .unwrap();
    let toml = format!(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n{extra_toml}"
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    Fixture { dir, ws }
}

impl Fixture {
    pub async fn session(&self, opts: SessionOptions) -> Session {
        let conn = self.ws.connection("lite").unwrap();
        connect(&self.ws, conn, None, opts).await.unwrap()
    }
    /// A scalar from the database file through a separate read-write connection.
    pub fn count(&self, sql: &str) -> i64 {
        let c = rusqlite::Connection::open(self.dir.path().join("data/app.db")).unwrap();
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }
    pub fn db_path(&self) -> std::path::PathBuf {
        self.dir.path().join("data/app.db")
    }
    /// A text scalar through a separate connection.
    pub fn text(&self, sql: &str) -> String {
        let c = rusqlite::Connection::open(self.db_path()).unwrap();
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }
    pub async fn target(&self, name: &str) -> EditTarget {
        let mut s = self.session(SessionOptions::default()).await;
        let t = s
            .edit_target(&RelationRef::new("main", name))
            .await
            .unwrap();
        s.close().await.unwrap();
        t
    }
    pub async fn apply_changes(
        &self,
        target: &EditTarget,
        changes: Vec<Change>,
    ) -> Result<Applied, ApplyError> {
        let batch = EditBatch::new(target.clone(), changes).unwrap();
        apply(&self.ws, self.ws.connection("lite").unwrap(), None, batch).await
    }
    /// Every row of `sql` through a Dbento session, as the grid would hold it.
    pub async fn rows(&self, sql: &str) -> Vec<Vec<Cell>> {
        let mut s = self.session(SessionOptions::default()).await;
        let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        s.close().await.unwrap();
        ran.rows
    }
    pub async fn row(&self, sql: &str) -> Vec<Cell> {
        self.rows(sql).await.remove(0)
    }
}

pub struct PgFixture {
    pub db: dbento_testkit::PgDb,
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

/// A workspace with one Postgres connection `pg` on a fresh database; `extra_toml` joins its table.
pub async fn pg_fixture(extra_toml: &str) -> PgFixture {
    let db = dbento_testkit::pg_db().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", extra_toml)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    PgFixture { db, dir, ws }
}

impl PgFixture {
    pub fn conn(&self) -> &ConnectionConfig {
        self.ws.connection("pg").unwrap()
    }
    pub fn secret(&self) -> Secret {
        Secret::new(self.db.password())
    }
    pub async fn session(&self, opts: SessionOptions) -> Session {
        connect(&self.ws, self.conn(), Some(self.secret()), opts)
            .await
            .unwrap()
    }
    pub async fn target(&self, schema: &str, name: &str) -> EditTarget {
        let mut s = self.session(SessionOptions::default()).await;
        let t = s
            .edit_target(&RelationRef::new(schema, name))
            .await
            .unwrap();
        s.close().await.unwrap();
        t
    }
    pub async fn apply_changes(
        &self,
        target: &EditTarget,
        changes: Vec<Change>,
    ) -> Result<Applied, ApplyError> {
        let batch = EditBatch::new(target.clone(), changes).unwrap();
        apply(&self.ws, self.conn(), Some(self.secret()), batch).await
    }
}

pub fn val(s: &str) -> EditValue {
    EditValue::Text(s.to_owned())
}

pub fn pairs(p: &[(&str, EditValue)]) -> Vec<(String, EditValue)> {
    p.iter()
        .map(|(c, v)| ((*c).to_owned(), v.clone()))
        .collect()
}

pub fn row_key(p: &[(&str, EditValue)]) -> RowKey {
    RowKey::new(pairs(p))
}

/// The outcomes of an apply that must have been rejected.
pub fn rejected(r: Result<Applied, ApplyError>) -> Vec<ChangeOutcome> {
    match r {
        Err(ApplyError::Rejected { outcomes }) => outcomes,
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

/// A delivered cell as the value the grid would stage for it.
pub fn edit_value(cell: &Cell) -> EditValue {
    match cell {
        Cell::Null => EditValue::Null,
        Cell::Text(t) => EditValue::Text(t.clone()),
        Cell::Bytes(b) => EditValue::Bytes(b.clone()),
    }
}

impl PgFixture {
    /// The first row of `sql` through a Dbento session, as the grid would hold it.
    pub async fn row(&self, sql: &str) -> Vec<Cell> {
        let mut s = self.session(SessionOptions::default()).await;
        let mut ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        s.close().await.unwrap();
        ran.rows.remove(0)
    }
}

pub fn text(s: &str) -> Cell {
    Cell::Text(s.to_owned())
}

pub struct Ran {
    pub sql: String,
    pub columns: Vec<(String, String)>,
    pub rows: Vec<Vec<Cell>>,
    pub outcome: Outcome,
}

/// Execute one pending statement and collect everything it returns.
pub async fn collect(p: Pending<'_>, limits: Limits) -> Result<Ran, ExecuteError> {
    let sql = p.sql().to_owned();
    let mut stream = p.execute(limits).await?;
    let columns = stream
        .columns()
        .iter()
        .map(|c| (c.name().to_owned(), c.type_name().to_owned()))
        .collect();
    let mut rows = Vec::new();
    while let Some(batch) = stream.next_batch().await? {
        for r in 0..batch.len() {
            rows.push(
                (0..batch.column_count())
                    .map(|c| match batch.cell(r, c) {
                        CellRef::Null => Cell::Null,
                        CellRef::Text(t) => Cell::Text(t.to_owned()),
                        CellRef::Bytes(b) => Cell::Bytes(b.to_vec()),
                        other => panic!("unexpected cell {other:?}"),
                    })
                    .collect(),
            );
        }
    }
    let outcome = stream
        .outcome()
        .cloned()
        .expect("an outcome after the last batch");
    Ok(Ran {
        sql,
        columns,
        rows,
        outcome,
    })
}

/// Run every statement of `sql` in order, stopping after the first error.
pub async fn run(
    session: &mut Session,
    sql: &str,
    limits: Limits,
) -> Vec<Result<Ran, ExecuteError>> {
    let mut out = Vec::new();
    let mut script = session.script(sql);
    while let Some(next) = script.next().await {
        let result = match next {
            Ok(p) => collect(p, limits).await,
            Err(e) => Err(e),
        };
        let failed = result.is_err();
        out.push(result);
        if failed {
            break;
        }
    }
    out
}

pub struct MyFixture {
    pub db: dbento_testkit::MyDb,
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

/// A workspace with one MySQL connection `my` on `db`; `extra_toml` joins its table.
pub fn my_fixture(db: dbento_testkit::MyDb, extra_toml: &str) -> MyFixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("my", extra_toml)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    MyFixture { db, dir, ws }
}

/// One fixture per MySQL-family matrix server, labelled as `my_matrix` labels it.
pub async fn my_fixtures(extra_toml: &str) -> Vec<(String, MyFixture)> {
    dbento_testkit::my_matrix()
        .await
        .into_iter()
        .map(|(label, db)| (label, my_fixture(db, extra_toml)))
        .collect()
}

impl MyFixture {
    pub fn conn(&self) -> &ConnectionConfig {
        self.ws.connection("my").unwrap()
    }
    pub fn secret(&self) -> Secret {
        Secret::new(self.db.password())
    }
    pub async fn try_session(&self, opts: SessionOptions) -> Result<Session, ConnectError> {
        connect(&self.ws, self.conn(), Some(self.secret()), opts).await
    }
    pub async fn session(&self, opts: SessionOptions) -> Session {
        self.try_session(opts).await.unwrap()
    }
}

/// What `tcp_proxy` does with each connection after the first.
#[derive(Debug, Clone, Copy)]
pub enum Later {
    /// Accept and close at once, so a side connection cannot connect.
    Refuse,
    /// Hold this long before connecting through.
    Delay(std::time::Duration),
}

/// A TCP proxy on a free local port to `host:port`. The first connection, the session's own,
/// passes at once; every later one (a KILL side connection) is handled per `later`.
pub async fn tcp_proxy(host: String, port: u16, later: Later) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut first = true;
        while let Ok((mut client, _)) = listener.accept().await {
            let delay = match (std::mem::take(&mut first), later) {
                (true, _) => std::time::Duration::ZERO,
                (false, Later::Refuse) => continue,
                (false, Later::Delay(d)) => d,
            };
            let host = host.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if let Ok(mut server) = tokio::net::TcpStream::connect((host.as_str(), port)).await
                {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
            });
        }
    });
    local
}

impl MyFixture {
    /// Routes connection `my` through `tcp_proxy`; `db.admin()` stays direct.
    pub async fn proxied(mut self, later: Later) -> MyFixture {
        let (host, port) = (self.db.host().to_owned(), self.db.port());
        let local = tcp_proxy(host.clone(), port, later).await;
        let direct = format!("host = \"{host}\"\nport = {port}\n");
        let toml = self.db.connection_toml("my", "");
        assert!(toml.contains(&direct), "{toml}");
        let toml = toml.replace(&direct, &format!("host = \"127.0.0.1\"\nport = {local}\n"));
        std::fs::write(
            self.dir.path().join("dbento.toml"),
            format!("dbento = 1\n{toml}"),
        )
        .unwrap();
        self.ws = Workspace::open(self.dir.path()).unwrap();
        self
    }

    /// True once server thread `id` runs no statement (or is gone), polled for up to `within`.
    pub async fn quiet_within(&self, id: &str, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        let sql = format!(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id} AND INFO IS NOT NULL"
        );
        while std::time::Instant::now() < deadline {
            if self.db.scalar(&sql).await.as_deref() == Some("0") {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }
}

/// The server thread id of `s`'s connection.
pub async fn my_thread_id(s: &mut Session) -> String {
    let mut ran = run(s, "SELECT CONNECTION_ID()", Limits::default()).await;
    match ran.remove(0).unwrap().rows.remove(0).remove(0) {
        Cell::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

#[cfg(feature = "duckdb")]
pub struct DuckFixture {
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

/// A workspace with DuckDB connection `duck` over `data/app.duckdb`, built from `setup` and
/// closed again; read data back through Dbento sessions, never a second crate-level open.
#[cfg(feature = "duckdb")]
pub fn duckdb_fixture(setup: &str, extra_toml: &str) -> DuckFixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    let cfg = duckdb::Config::default()
        .enable_autoload_extension(false)
        .unwrap()
        .with("home_directory", dir.path().join("home").to_str().unwrap())
        .unwrap();
    let c = duckdb::Connection::open_with_flags(dir.path().join("data/app.duckdb"), cfg).unwrap();
    c.execute_batch(setup).unwrap();
    drop(c);
    let toml = format!(
        "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n{extra_toml}"
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    DuckFixture { dir, ws }
}

#[cfg(feature = "duckdb")]
impl DuckFixture {
    pub async fn try_session_of(
        &self,
        name: &str,
        opts: SessionOptions,
    ) -> Result<Session, ConnectError> {
        connect(&self.ws, self.ws.connection(name).unwrap(), None, opts).await
    }
    pub async fn session(&self, opts: SessionOptions) -> Session {
        self.try_session_of("duck", opts).await.unwrap()
    }
    pub fn db_path(&self) -> std::path::PathBuf {
        self.dir.path().join("data/app.duckdb")
    }
    /// The first cell of `sql` through a fresh Dbento session.
    pub async fn scalar(&self, sql: &str) -> Cell {
        let mut s = self.session(SessionOptions::default()).await;
        let mut ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        s.close().await.unwrap();
        ran.rows.remove(0).remove(0)
    }
}

pub struct MsFixture {
    pub db: dbento_testkit::MsDb,
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

/// A workspace with one SQL Server connection `ms` on `db`; `extra_toml` joins its table.
pub fn ms_fixture(db: dbento_testkit::MsDb, extra_toml: &str) -> MsFixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("ms", extra_toml)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    MsFixture { db, dir, ws }
}

/// `mssql_db()` reached with `tls = "off"`, as its stock certificate needs.
pub async fn ms_off() -> MsFixture {
    ms_fixture(dbento_testkit::mssql_db().await, "tls = \"off\"")
}

impl MsFixture {
    pub fn conn(&self) -> &ConnectionConfig {
        self.ws.connection("ms").unwrap()
    }
    pub fn secret(&self) -> Secret {
        Secret::new(self.db.password())
    }
    pub async fn try_session(&self, opts: SessionOptions) -> Result<Session, ConnectError> {
        connect(&self.ws, self.conn(), Some(self.secret()), opts).await
    }
    pub async fn session(&self, opts: SessionOptions) -> Session {
        self.try_session(opts).await.unwrap()
    }
}

#[cfg(feature = "mongodb")]
pub struct MgFixture {
    pub db: MgDb,
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

#[cfg(feature = "mongodb")]
fn mg_name(user: MgUser) -> &'static str {
    match user {
        MgUser::Root => "root",
        MgUser::Reader => "reader",
        MgUser::Writer => "writer",
        MgUser::Anonymous => "anonymous",
    }
}

/// A fresh seeded database with one connection per user; `extra_toml` joins each table.
#[cfg(feature = "mongodb")]
pub async fn mg(extra_toml: &str) -> MgFixture {
    let db = dbento_testkit::mongo_db().await;
    let dir = tempfile::tempdir().unwrap();
    let mut toml = "dbento = 1\n".to_owned();
    for user in [
        MgUser::Root,
        MgUser::Reader,
        MgUser::Writer,
        MgUser::Anonymous,
    ] {
        toml.push_str(&db.connection_toml(mg_name(user), user, extra_toml));
    }
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    MgFixture { db, dir, ws }
}

#[cfg(feature = "mongodb")]
impl MgFixture {
    pub fn conn(&self, user: MgUser) -> &ConnectionConfig {
        self.ws.connection(mg_name(user)).unwrap()
    }
    pub async fn session(&self, user: MgUser, opts: SessionOptions) -> Session {
        let secret = self.db.password(user).map(Secret::new);
        connect(&self.ws, self.conn(user), secret, opts)
            .await
            .unwrap()
    }
}

/// Options that force a read-only session.
#[cfg(feature = "mongodb")]
pub fn forced() -> SessionOptions {
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    o
}

/// One statement that must succeed.
#[cfg(any(feature = "mongodb", feature = "redis"))]
pub async fn one(s: &mut Session, text: &str, limits: Limits) -> Ran {
    run(s, text, limits).await.remove(0).unwrap()
}

/// `one` with no limits.
#[cfg(any(feature = "mongodb", feature = "redis"))]
pub async fn read(s: &mut Session, text: &str) -> Ran {
    one(s, text, Limits::default()).await
}

/// One statement that must fail.
#[cfg(any(feature = "mongodb", feature = "redis"))]
pub async fn failure(s: &mut Session, text: &str) -> ExecuteError {
    match run(s, text, Limits::default()).await.remove(0) {
        Ok(ran) => panic!("{text}: {:?}", ran.outcome),
        Err(e) => e,
    }
}

/// A `Database` error's code.
#[cfg(any(feature = "mongodb", feature = "redis"))]
pub fn code_of(e: ExecuteError) -> String {
    match e {
        ExecuteError::Database { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

/// Waits until the server's open cursors fall back to `baseline`.
#[cfg(feature = "mongodb")]
pub async fn cursors_back_to(db: &MgDb, baseline: i64) {
    for _ in 0..100 {
        if db.open_cursors().await == baseline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!(
        "{} cursors open, wanted {baseline}",
        db.open_cursors().await
    );
}

#[cfg(feature = "redis")]
pub struct RdFixture {
    pub db: dbento_testkit::RedisDb,
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

#[cfg(feature = "redis")]
fn rd_name(user: dbento_testkit::RdUser) -> &'static str {
    match user {
        dbento_testkit::RdUser::Admin => "admin",
        dbento_testkit::RdUser::Reader => "ro",
        dbento_testkit::RdUser::Writer => "writer",
    }
}

/// Connections `admin`, `ro` and `writer` on a fresh `redis_db()`.
#[cfg(feature = "redis")]
pub async fn rd(extra_toml: &str) -> RdFixture {
    use dbento_testkit::RdUser::{Admin, Reader, Writer};
    let db = dbento_testkit::redis_db().await;
    let dir = tempfile::tempdir().unwrap();
    let mut toml = "dbento = 1\n".to_owned();
    for user in [Admin, Reader, Writer] {
        toml.push_str(&db.connection_toml(rd_name(user), user, extra_toml));
    }
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    RdFixture { db, dir, ws }
}

#[cfg(feature = "redis")]
impl RdFixture {
    pub fn conn(&self, user: dbento_testkit::RdUser) -> &ConnectionConfig {
        self.ws.connection(rd_name(user)).unwrap()
    }
    pub async fn session(&self, user: dbento_testkit::RdUser, opts: SessionOptions) -> Session {
        let secret = self.db.password(user).map(Secret::new);
        connect(&self.ws, self.conn(user), secret, opts)
            .await
            .unwrap()
    }
    /// A session as an ad-hoc `login`, in a workspace of its own.
    pub async fn session_for(
        &self,
        login: &str,
        password: &str,
        opts: SessionOptions,
    ) -> Result<Session, ConnectError> {
        let dir = tempfile::tempdir().unwrap();
        let toml = format!(
            "dbento = 1\n{}",
            self.db.connection_toml_as("adhoc", login, "")
        );
        std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        connect(
            &ws,
            ws.connection("adhoc").unwrap(),
            Some(Secret::new(password)),
            opts,
        )
        .await
    }
}

/// One `field=value` of the session's own `CLIENT INFO` line.
#[cfg(feature = "redis")]
pub async fn client_field(s: &mut Session, field: &str) -> String {
    let got = read(s, "CLIENT INFO").await;
    let Cell::Text(line) = &got.rows[0][0] else {
        panic!("{:?}", got.rows)
    };
    let prefix = format!("{field}=");
    line.split_whitespace()
        .find_map(|f| f.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no {field} in {line}"))
        .to_owned()
}

/// The session's server process id as text.
pub async fn ms_spid(s: &mut Session) -> String {
    let mut ran = run(
        s,
        "SELECT CAST(@@SPID AS nvarchar(10)) AS spid",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    match ran.rows.remove(0).remove(0) {
        Cell::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

/// SQL that counts `s`'s own session on the server. A probe's fresh login may reuse a closed
/// session's spid, so the login time tells the two apart.
pub async fn ms_this_session(s: &mut Session) -> String {
    let mut ran = run(
        s,
        "SELECT CONCAT(N'SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE session_id = ', \
         session_id, N' AND login_time = ''', CONVERT(nvarchar(30), login_time, 126), N'''') \
         AS q FROM sys.dm_exec_sessions WHERE session_id = @@SPID",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    match ran.rows.remove(0).remove(0) {
        Cell::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

/// Polls `sql` on fresh login clients until its first cell is `want`, for up to 10 s.
pub async fn ms_until(db: &dbento_testkit::MsDb, sql: &str, want: &str) -> bool {
    for _ in 0..100 {
        if db.scalar(sql).await.as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// The spec's reader principal, created in the current database.
pub const MS_READER_SETUP: &str = "CREATE USER dbento_reader WITHOUT LOGIN; \
    ALTER ROLE db_datareader ADD MEMBER dbento_reader; \
    ALTER ROLE db_denydatawriter ADD MEMBER dbento_reader; \
    GRANT VIEW DEFINITION TO dbento_reader; \
    DENY ALTER ANY USER TO dbento_reader";

/// A fresh database with each of `setup` run as the login, then `dbento_reader`; the
/// connection is `tls = "off"` plus `extra`.
pub async fn ms_reader(setup: &[&str], extra: &str) -> MsFixture {
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    for sql in setup.iter().copied().chain([MS_READER_SETUP]) {
        c.simple_query(sql)
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap();
    }
    drop(c);
    ms_fixture(db, &format!("tls = \"off\"\n{extra}"))
}

/// A session on `db` as `login` (user, auth_level), or anonymous for `None`, through a workspace
/// that lasts only for the connect; `extra` joins the connection's table.
pub async fn sr_try(
    db: &dbento_testkit::SrDb,
    login: Option<(&str, &str)>,
    extra: &str,
    opts: SessionOptions,
) -> Result<Session, ConnectError> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("sr", login, extra)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("sr").unwrap();
    let secret = login.map(|_| Secret::new(db.password()));
    connect(&ws, conn, secret, opts).await
}

/// [`sr_try`] that must connect.
pub async fn sr_session(
    db: &dbento_testkit::SrDb,
    login: Option<(&str, &str)>,
    extra: &str,
    opts: SessionOptions,
) -> Session {
    sr_try(db, login, extra, opts).await.unwrap()
}

/// A session as the server's own root user.
pub async fn sr_root(db: &dbento_testkit::SrDb, extra: &str, opts: SessionOptions) -> Session {
    sr_session(db, Some(("root", "root")), extra, opts).await
}

/// The local proxy behind [`sr_proxied`].
pub struct SrPipe {
    tasks: tokio::task::JoinSet<()>,
    frozen: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SrPipe {
    /// Cuts the session's socket.
    pub fn cut(&mut self) {
        self.tasks.abort_all();
    }
    /// The socket stays open and nothing more passes either way: a server gone quiet.
    pub fn freeze(&self) {
        self.frozen
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

// Holds a chunk it has read until the pipe thaws, so a freeze drops nothing and passes nothing.
async fn relay(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    frozen: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut buffer = vec![0u8; 64 * 1024];
    while let Ok(read @ 1..) = from.read(&mut buffer).await {
        while frozen.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if to.write_all(&buffer[..read]).await.is_err() {
            return;
        }
    }
}

/// A root session on `db`, with `extra` and `opts`, through a local proxy the test controls.
pub async fn sr_proxied(
    db: &dbento_testkit::SrDb,
    extra: &str,
    opts: SessionOptions,
) -> (Session, SrPipe) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    let (host, port) = (db.host().to_owned(), db.port());
    let frozen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tasks = tokio::task::JoinSet::new();
    let thaw = frozen.clone();
    tasks.spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .unwrap();
        let (client_read, client_write) = client.into_split();
        let (server_read, server_write) = server.into_split();
        tokio::join!(
            relay(client_read, server_write, thaw.clone()),
            relay(server_read, client_write, thaw),
        );
    });
    let dir = tempfile::tempdir().unwrap();
    let toml = db
        .connection_toml("sr", Some(("root", "root")), extra)
        .replace(&format!("port = {port}"), &format!("port = {local}"));
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{toml}"),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("sr").unwrap();
    let secret = Some(Secret::new(db.password()));
    let session = connect(&ws, conn, secret, opts).await.unwrap();
    (session, SrPipe { tasks, frozen })
}

/// The namespaces and this namespace's databases, as a root connection of the testkit sees them.
pub async fn sr_targets(db: &dbento_testkit::SrDb) -> (serde_json::Value, serde_json::Value) {
    let info = db.admin("INFO FOR ROOT; INFO FOR NS").await;
    (info[0]["namespaces"].clone(), info[1]["databases"].clone())
}
