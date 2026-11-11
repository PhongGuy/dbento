//! The catalog runner: read-only loads in request order, connect retries, and closing.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::support::{fixture, here, pg_spec, spec};
use async_channel::Receiver;
use dbento_app::catalog::{
    CATALOG_TIMEOUT, CatalogData, CatalogQuery, CatalogReply, catalog_options, spawn_catalog,
};
use dbento_core::{ObjectRef, RelationKind, RelationRef};

const T: &str =
    "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT NOT NULL); CREATE VIEW v AS SELECT a FROM t;";

async fn reply(rx: &Receiver<CatalogReply>) -> CatalogReply {
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("a reply within 20 s")
        .expect("the runner is alive")
}

fn rel_names(data: &CatalogData) -> Vec<(String, RelationKind)> {
    match data {
        CatalogData::Relations(l) => l
            .items()
            .iter()
            .map(|r| (r.name().to_owned(), r.kind()))
            .collect(),
        other @ (CatalogData::Schemas(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Definition(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
}

#[test]
fn catalog_sessions_are_read_only_with_a_capped_timeout() {
    let o = catalog_options(None);
    assert!(o.force_read_only);
    assert!(o.trusted_sql, "catalog sessions run only SQL Dbento wrote");
    assert_eq!(o.statement_timeout, Some(CATALOG_TIMEOUT));
    assert_eq!(
        catalog_options(Some(Duration::from_secs(5))).statement_timeout,
        Some(Duration::from_secs(5))
    );
    assert_eq!(
        catalog_options(Some(Duration::from_secs(60))).statement_timeout,
        Some(CATALOG_TIMEOUT)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn loads_each_level_in_request_order() {
    let fx = fixture(T, "");
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    r.load(7, CatalogQuery::Schemas);
    r.load(
        7,
        CatalogQuery::Relations {
            schema: "main".into(),
        },
    );
    r.load(
        8,
        CatalogQuery::Columns {
            rel: RelationRef::new("main", "t"),
        },
    );
    let first = reply(&rx).await;
    assert_eq!(
        (first.generation, &first.query),
        (7, &CatalogQuery::Schemas)
    );
    match first.result.unwrap() {
        CatalogData::Schemas(s) => {
            assert_eq!(s.iter().map(|x| x.name()).collect::<Vec<_>>(), ["main"])
        }
        other @ (CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Definition(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
    let second = reply(&rx).await;
    assert_eq!(
        rel_names(&second.result.unwrap()),
        [
            ("t".to_owned(), RelationKind::Table),
            ("v".to_owned(), RelationKind::View)
        ]
    );
    let third = reply(&rx).await;
    assert_eq!(third.generation, 8);
    match third.result.unwrap() {
        CatalogData::Columns(c) => {
            let got: Vec<_> = c.iter().map(|c| (c.name(), c.primary_key())).collect();
            assert_eq!(got, [("a", Some(1)), ("b", None)]);
        }
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Definition(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(5), r.close().wait())
        .await
        .expect("closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connect_is_an_error_reply_and_the_next_load_retries() {
    let fx = fixture(T, "");
    let db = fx.dir.path().join("data/app.db");
    std::fs::remove_file(&db).unwrap();
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    let failed = reply(&rx).await;
    let message = failed.result.unwrap_err();
    assert!(message.contains("does not exist"), "{message}");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(T)
        .unwrap();
    r.load(0, CatalogQuery::Schemas);
    assert!(reply(&rx).await.result.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_secret_is_an_error_reply_with_the_hint() {
    let fx = fixture("", "");
    let r = spawn_catalog(&here(), spec(&fx, "nopg"));
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    let message = reply(&rx).await.result.unwrap_err();
    assert!(message.contains("DBENTO_SECRET_NOPG"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_runner_ends_it() {
    let fx = fixture(T, "");
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    reply(&rx).await;
    drop(r);
    let end = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("ended");
    assert!(end.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_catalog_load_leaves_no_transaction_open() {
    let (db, _dir, spec) = pg_spec("").await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(x int)")
        .await
        .unwrap();
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    r.load(
        0,
        CatalogQuery::Relations {
            schema: "public".into(),
        },
    );
    reply(&rx).await.result.unwrap();
    assert_eq!(
        rel_names(&reply(&rx).await.result.unwrap()),
        [("t".to_owned(), RelationKind::Table)]
    );
    let state = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
    assert_eq!(db.scalar(state).await.as_deref(), Some("idle"));
    tokio::time::timeout(Duration::from_secs(5), r.close().wait())
        .await
        .expect("closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_postgres_session_is_replaced_on_the_next_load() {
    let (db, _dir, spec) = pg_spec("").await;
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    reply(&rx).await.result.unwrap();
    db.scalar("SELECT pg_terminate_backend(pid)::text FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()")
        .await;
    // The load that meets the dead session fails; a runner that kept the session would fail
    // every later load too, so a success within a few tries proves the reconnect.
    let mut ok = false;
    for _ in 0..5 {
        r.load(0, CatalogQuery::Schemas);
        if reply(&rx).await.result.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ok, "the catalog runner never reconnected");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_abandons_a_blocked_postgres_load_and_its_queue() {
    let (db, _dir, spec) = pg_spec("").await;
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    reply(&rx).await.result.unwrap();
    // A held lock on pg_namespace blocks the next schema load until its 30 s timeout; the
    // lock holder's own connection watches for the wait, since a new one could block too.
    let admin = db.admin().await;
    admin
        .batch_execute("BEGIN; LOCK TABLE pg_catalog.pg_namespace IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    r.load(0, CatalogQuery::Schemas);
    r.load(0, CatalogQuery::Schemas);
    let waiting = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' \
                   AND datname = current_database() AND wait_event_type = 'Lock'";
    let mut blocked = false;
    for _ in 0..100 {
        let n: i64 = admin.query_one(waiting, &[]).await.unwrap().get(0);
        if n == 1 {
            blocked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(blocked, "the catalog load never blocked on the lock");
    tokio::time::timeout(Duration::from_secs(5), r.close().wait())
        .await
        .expect("the close waited on the blocked load");
    admin.batch_execute("ROLLBACK").await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn count_counts_the_relation_and_fails_on_a_missing_one() {
    let fx = fixture(&format!("{T} INSERT INTO t(b) VALUES ('x'), ('y');"), "");
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    let rel = RelationRef::new("main", "t");
    r.load(3, CatalogQuery::Count { rel: rel.clone() });
    let got = reply(&rx).await;
    assert_eq!(got.query, CatalogQuery::Count { rel });
    assert_eq!(got.result, Ok(CatalogData::Count(2)));
    r.load(
        4,
        CatalogQuery::Count {
            rel: RelationRef::new("main", "gone"),
        },
    );
    assert!(reply(&rx).await.result.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_count_reads_the_bigint() {
    let (db, _dir, spec) = pg_spec("").await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE \"Big\"(x int); INSERT INTO \"Big\" SELECT generate_series(1, 1234)",
        )
        .await
        .unwrap();
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    let rel = RelationRef::new("public", "Big");
    r.load(0, CatalogQuery::Count { rel });
    assert_eq!(reply(&rx).await.result, Ok(CatalogData::Count(1234)));
    tokio::time::timeout(Duration::from_secs(5), r.close().wait())
        .await
        .expect("closed");
}

/// A TCP proxy to `host:port` on a free local port. Bytes pass both ways until `cut` is
/// set; from then on every byte is swallowed while the sockets stay open, as a network
/// partition looks to the client. Clearing `cut` heals new connections only.
async fn partition_proxy(host: String, port: u16, cut: Arc<AtomicBool>) -> u16 {
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    async fn pump(
        mut from: impl AsyncRead + Unpin,
        mut to: impl AsyncWrite + Unpin,
        cut: Arc<AtomicBool>,
    ) {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = from.read(&mut buf).await {
            if !cut.load(Ordering::SeqCst) && to.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let (host, cut) = (host.clone(), Arc::clone(&cut));
            tokio::spawn(async move {
                if let Ok(server) = tokio::net::TcpStream::connect((host.as_str(), port)).await {
                    let ((cr, cw), (sr, sw)) = (client.into_split(), server.into_split());
                    tokio::join!(pump(cr, sw, Arc::clone(&cut)), pump(sr, cw, cut));
                }
            });
        }
    });
    local
}

#[tokio::test(flavor = "multi_thread")]
async fn a_load_cut_off_by_a_partition_fails_and_the_retry_reconnects() {
    let (db, dir, mut spec) = pg_spec("statement_timeout = \"1s\"").await;
    let cut = Arc::new(AtomicBool::new(false));
    let local = partition_proxy(db.host().to_owned(), db.port(), Arc::clone(&cut)).await;
    let direct = format!("host = \"{}\"\nport = {}\n", db.host(), db.port());
    let path = dir.path().join("dbento.toml");
    let toml = std::fs::read_to_string(&path).unwrap();
    assert!(toml.contains(&direct), "{toml}");
    let toml = toml.replace(&direct, &format!("host = \"127.0.0.1\"\nport = {local}\n"));
    std::fs::write(&path, toml).unwrap();
    spec.workspace = Arc::new(dbento_core::Workspace::open(dir.path()).unwrap());

    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    r.load(0, CatalogQuery::Schemas);
    reply(&rx).await.result.unwrap();
    // The server's 1 s timeout fires, but its error never crosses the partition, so only a
    // client-side timeout can end this load.
    cut.store(true, Ordering::SeqCst);
    r.load(1, CatalogQuery::Schemas);
    let failed = reply(&rx).await;
    assert_eq!(failed.generation, 1);
    assert!(failed.result.is_err(), "{:?}", failed.result);
    // The retry must not queue behind the dead request: it reconnects and succeeds.
    cut.store(false, Ordering::SeqCst);
    r.load(2, CatalogQuery::Schemas);
    let retried = reply(&rx).await;
    assert_eq!(retried.generation, 2);
    assert!(retried.result.is_ok(), "{:?}", retried.result);
    tokio::time::timeout(Duration::from_secs(5), r.close().wait())
        .await
        .expect("closed");
}

const OBJ: &str = "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT NOT NULL); \
                   CREATE INDEX t_b ON t(b); CREATE VIEW v AS SELECT a FROM t;";

// Loads `rel`'s objects and returns its first index's reference.
async fn first_index(
    r: &dbento_app::catalog::CatalogRunner,
    rx: &Receiver<CatalogReply>,
    rel: &RelationRef,
) -> ObjectRef {
    r.load(1, CatalogQuery::RelationObjects { rel: rel.clone() });
    let got = reply(rx).await;
    assert_eq!(
        (got.generation, &got.query),
        (1, &CatalogQuery::RelationObjects { rel: rel.clone() }),
        "the reply answers the relation-objects load"
    );
    match got.result.unwrap() {
        CatalogData::RelationObjects(o) => {
            let names: Vec<_> = o.indexes().iter().map(|i| i.name()).collect();
            assert_eq!(names, ["t_b"], "the relation has exactly its one index");
            o.indexes()[0].object_ref()
        }
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Definition(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_and_definitions_load_in_request_order() {
    let fx = fixture(OBJ, "");
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    let t = RelationRef::new("main", "t");
    let index = first_index(&r, &rx, &t).await;
    r.load(
        1,
        CatalogQuery::SchemaObjects {
            schema: "main".into(),
        },
    );
    match reply(&rx).await.result.unwrap() {
        CatalogData::SchemaObjects(s) => {
            assert!(s.types().is_empty() && s.routines().is_empty() && !s.truncated())
        }
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::Definition(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
    r.load(
        2,
        CatalogQuery::Definition {
            object: index.clone(),
        },
    );
    let got = reply(&rx).await;
    assert_eq!(got.query, CatalogQuery::Definition { object: index });
    match got.result.unwrap() {
        CatalogData::Definition(d) => {
            assert_eq!(d.text(), "CREATE INDEX t_b ON t(b);");
            assert!(!d.truncated() && !d.composed());
        }
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
    let view = ObjectRef::relation(RelationRef::new("main", "v"));
    r.load(2, CatalogQuery::Definition { object: view });
    match reply(&rx).await.result.unwrap() {
        CatalogData::Definition(d) => assert_eq!(d.text(), "CREATE VIEW v AS SELECT a FROM t;"),
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_object_says_so_and_suggests_a_refresh() {
    let fx = fixture(OBJ, "");
    let r = spawn_catalog(&here(), spec(&fx, "lite"));
    let rx = r.replies();
    let index = first_index(&r, &rx, &RelationRef::new("main", "t")).await;
    rusqlite::Connection::open(fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DROP INDEX t_b")
        .unwrap();
    r.load(0, CatalogQuery::Definition { object: index });
    let err = reply(&rx).await.result.unwrap_err();
    assert!(
        err.contains("no longer exists; refresh the explorer to see the current objects"),
        "{err}"
    );
    // A missing object is no lost session: the next listing still answers.
    r.load(0, CatalogQuery::Schemas);
    assert!(reply(&rx).await.result.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_postgres_session_is_replaced_after_a_definition_load() {
    let (db, _dir, spec) = pg_spec("").await;
    db.admin()
        .await
        .batch_execute("CREATE VIEW v AS SELECT 1 AS one")
        .await
        .unwrap();
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    let view = ObjectRef::relation(RelationRef::new("public", "v"));
    r.load(
        0,
        CatalogQuery::Definition {
            object: view.clone(),
        },
    );
    match reply(&rx).await.result.unwrap() {
        CatalogData::Definition(d) => {
            assert!(
                d.text().starts_with("CREATE VIEW public.v AS"),
                "{}",
                d.text()
            )
        }
        other @ (CatalogData::Schemas(_)
        | CatalogData::Relations(_)
        | CatalogData::Columns(_)
        | CatalogData::Count(_)
        | CatalogData::RelationObjects(_)
        | CatalogData::SchemaObjects(_)
        | CatalogData::Keys(_)
        | CatalogData::CommandDocs(_)
        | CatalogData::KeyColumns { .. }) => panic!("{other:?}"),
    }
    db.scalar("SELECT pg_terminate_backend(pid)::text FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()")
        .await;
    // Only definition loads meet the dead session, so only the definition arm can drop it.
    let mut ok = false;
    for _ in 0..5 {
        r.load(
            0,
            CatalogQuery::Definition {
                object: view.clone(),
            },
        );
        if reply(&rx).await.result.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ok, "a definition load on a dead session never reconnected");
}
