use std::time::Duration;

use crate::support::{pg_fixture, run, text};
use dbento_core::{ConnectError, ExecuteError, Limits, Secret, SessionOptions, Workspace, connect};

#[tokio::test]
async fn settings_become_startup_parameters() {
    let fx =
        pg_fixture("settings = { search_path = \"app, public\", TimeZone = \"Asia/Tokyo\" }").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SHOW search_path; SELECT '2024-01-02 03:04:05+00'::timestamptz; SHOW application_name; SHOW bytea_output",
        Limits::default(),
    )
    .await;
    let got: Vec<_> = r
        .iter()
        .map(|x| x.as_ref().unwrap().rows[0][0].clone())
        .collect();
    assert_eq!(
        got,
        vec![
            text("app, public"),
            text("2024-01-02 12:04:05+09"),
            text("dbento"),
            text("hex")
        ]
    );
}

#[tokio::test]
async fn a_setting_value_cannot_smuggle_another_option() {
    let fx =
        pg_fixture("settings = { search_path = \"x -c default_transaction_read_only=on\" }").await;
    let err = connect(
        &fx.ws,
        fx.conn(),
        Some(fx.secret()),
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("invalid search_path");
    assert!(
        err.to_string()
            .contains("invalid value for parameter \"search_path\""),
        "{err}"
    );
}

#[tokio::test]
async fn the_statement_timeout_comes_from_the_config_or_the_options() {
    let fx = pg_fixture("statement_timeout = \"250ms\"").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SHOW statement_timeout; SELECT pg_sleep(5)",
        Limits::default(),
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("250ms")]]);
    assert!(
        matches!(r[1], Err(ExecuteError::Timeout)),
        "{:?}",
        r[1].as_ref().err()
    );
    let mut opts = SessionOptions::default();
    opts.statement_timeout = Some(Duration::from_secs(7));
    let mut s = fx.session(opts).await;
    let r = run(&mut s, "SHOW statement_timeout", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("7s")]]);
}

#[tokio::test]
async fn a_rejected_password_reports_the_server_message_without_the_secret() {
    let fx = pg_fixture("").await;
    let err = connect(
        &fx.ws,
        fx.conn(),
        Some(Secret::new("wrong-secret")),
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("bad password");
    let msg = err.to_string();
    assert!(msg.contains("password authentication failed"), "{msg}");
    assert!(!msg.contains("wrong-secret"), "{msg}");
}

#[tokio::test]
async fn an_unreachable_server_names_the_target() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = {port}\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n"),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let err = connect(
        &ws,
        ws.connection("pg").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("refused");
    assert!(
        err.to_string().contains(&format!("u@127.0.0.1:{port}/d")),
        "{err}"
    );
}

#[tokio::test]
async fn an_out_of_range_timeout_option_fails_before_any_io() {
    // 10.255.255.1 drops SYNs, so reaching the network would hang into the bound below.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"10.255.255.1\"\n\
         database = \"d\"\nuser = \"u\"\n",
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    for t in [Duration::ZERO, Duration::from_millis(i32::MAX as u64 + 1)] {
        let mut opts = SessionOptions::default();
        opts.statement_timeout = Some(t);
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            connect(
                &ws,
                ws.connection("pg").unwrap(),
                Some(Secret::new("p")),
                opts,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{t:?}: connect reached the network"))
        .err()
        .expect("out of range");
        assert!(
            matches!(err, ConnectError::InvalidTimeout { .. }),
            "{t:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_host_that_drops_packets_times_out_naming_host_and_port() {
    // 10.255.255.1 drops SYNs; the outer bound turns a missing timeout into a failure.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"10.255.255.1\"\n\
         port = 5439\ndatabase = \"d\"\nuser = \"u\"\n",
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        connect(
            &ws,
            ws.connection("pg").unwrap(),
            Some(Secret::new("hunter2-secret")),
            SessionOptions::default(),
        ),
    )
    .await
    .expect("connect hung past the outer bound")
    .err()
    .expect("timed out");
    assert!(started.elapsed() < Duration::from_secs(15), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("did not answer within 10 s") && msg.contains("10.255.255.1:5439"),
        "{msg}"
    );
    assert!(!msg.contains("hunter2-secret"), "{msg}");
}

#[tokio::test]
async fn a_server_that_accepts_but_never_answers_the_startup_times_out() {
    // A listener that accepts and then stays silent, like a paused container: TCP succeeds,
    // so only a bound over the whole handshake can end the wait.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = {port}\ndatabase = \"d\"\nuser = \"u\"\n"),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        connect(
            &ws,
            ws.connection("pg").unwrap(),
            Some(Secret::new("p")),
            SessionOptions::default(),
        ),
    )
    .await
    .expect("connect hung past the outer bound")
    .err()
    .expect("timed out");
    assert!(started.elapsed() < Duration::from_secs(15), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("host 127.0.0.1:{port} did not answer within 10 s")),
        "{msg}"
    );
}

#[tokio::test]
async fn closed_resolves_once_an_idle_sessions_backend_is_terminated() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(run(&mut s, "SELECT 1", Limits::default()).await[0].is_ok());
    let live = tokio::time::timeout(Duration::from_millis(200), s.closed()).await;
    assert!(live.is_err(), "a live session must not read as closed");
    fx.db
        .scalar(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE application_name = 'dbento' AND datname = current_database()",
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), s.closed())
        .await
        .expect("closed() resolves once the backend is gone");
    s.close().await.unwrap();
}
