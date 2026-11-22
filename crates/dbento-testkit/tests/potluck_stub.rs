//! The loopback Potluck stub: canned routes, a read request body, held event streams.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use std::process::{Command, Stdio};

use dbento_testkit::potluck::{self, Origin, Reply, Stub};

fn call(stub: &Stub, verb: &str, path: &str, body: &str) -> (u16, String) {
    let mut c = TcpStream::connect(stub.uri().trim_start_matches("http://")).unwrap();
    write!(
        c,
        "{verb} {path} HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let code = out.split(' ').nth(1).unwrap().parse().unwrap();
    let body = out
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (code, body)
}

#[test]
fn a_route_answers_its_reply_and_any_other_path_is_404() {
    let stub = Stub::start();
    stub.reply("GET", "/api/v1/me", Reply::Json(200, potluck::me_json()));
    stub.reply("DELETE", "/api/v1/tokens/42", Reply::Status(204));
    let (code, body) = call(&stub, "GET", "/api/v1/me?x=1", "");
    assert_eq!(code, 200);
    assert!(body.contains("\"ada\""), "{body}");
    assert_eq!(call(&stub, "DELETE", "/api/v1/tokens/42", "").0, 204);
    assert_eq!(call(&stub, "GET", "/nope", "").0, 404);
    assert_eq!(stub.hits("GET", "/api/v1/me"), 1);
}

#[test]
fn a_post_body_is_read_before_the_answer() {
    let stub = Stub::start();
    let token = potluck::fresh_token();
    stub.reply(
        "POST",
        "/api/v1/session",
        Reply::Json(201, potluck::session_json(&token)),
    );
    let big = "x".repeat(64 * 1024);
    let (code, body) = call(&stub, "POST", "/api/v1/session", &big);
    assert_eq!(code, 201);
    assert!(body.contains(&token));
}

#[test]
fn the_answer_waits_for_the_whole_post_body() {
    let stub = Stub::start();
    stub.reply("POST", "/api/v1/session", Reply::Json(201, "{}".into()));
    let mut c = TcpStream::connect(stub.uri().trim_start_matches("http://")).unwrap();
    write!(
        c,
        "POST /api/v1/session HTTP/1.1\r\nhost: x\r\ncontent-length: 4\r\n\r\n"
    )
    .unwrap();
    c.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut early = [0u8; 1];
    let read = c.read(&mut early);
    assert!(
        read.is_err(),
        "the stub answered before the body arrived: {read:?}"
    );
    c.set_read_timeout(None).unwrap();
    c.write_all(b"body").unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 201 "), "{out}");
}

#[test]
fn a_stream_gets_frames_sent_after_it_opened_until_closed() {
    let stub = Stub::start();
    stub.reply("GET", &potluck::events_path(), Reply::Stream);
    let mut c = TcpStream::connect(stub.uri().trim_start_matches("http://")).unwrap();
    write!(
        c,
        "GET {} HTTP/1.1\r\nhost: x\r\n\r\n",
        potluck::events_path()
    )
    .unwrap();
    let mut r = BufReader::new(c);
    let mut line = String::new();
    loop {
        line.clear();
        r.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while stub.hits("GET", &potluck::events_path()) == 0 {
        assert!(Instant::now() < deadline, "the stream was never counted");
        std::thread::sleep(Duration::from_millis(5));
    }
    stub.send("event: ref\ndata: {}\n\n");
    line.clear();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "event: ref\n");
    stub.close_streams();
    let mut rest = String::new();
    r.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "data: {}\n\n");
}

#[test]
fn serve_team_answers_what_a_login_a_sync_and_a_logout_ask() {
    let stub = Stub::start();
    potluck::serve_team(&stub, "file:///tmp/x/git/acme/shared.git", "write");
    let (code, info) = call(&stub, "GET", "/api/v1/server", "");
    assert_eq!(code, 200);
    let info: serde_json::Value = serde_json::from_str(&info).unwrap();
    assert_eq!(info["api"], 1);
    assert!(info["kinds"]["dbento"].is_object());
    assert_eq!(call(&stub, "POST", "/api/v1/session", "{}").0, 201);
    let (_, list) = call(&stub, "GET", "/api/v1/workspaces?kind=dbento", "");
    let list: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list[0]["git_url"], "file:///tmp/x/git/acme/shared.git");
    assert_eq!(list[0]["role"], "write");
    assert_eq!(list[0]["default_branch"], "main");
    assert_eq!(call(&stub, "DELETE", "/api/v1/tokens/42", "").0, 204);
}

#[test]
fn fresh_tokens_have_the_potluck_shape_and_differ() {
    let (a, b) = (potluck::fresh_token(), potluck::fresh_token());
    assert_eq!(a.len(), 47);
    assert!(a.starts_with("plk_"));
    assert!(a[4..].bytes().all(|c| c.is_ascii_alphanumeric()));
    assert_ne!(a, b);
}

#[test]
fn an_origin_seeds_main_and_a_teammate_push_moves_it() {
    let o = Origin::seeded(&[
        ("dbento.toml", "dbento = 1\n"),
        ("queries/a.sql", "select 1;\n"),
    ]);
    assert!(
        o.url().starts_with("file://") && o.url().ends_with("/git/acme/shared.git"),
        "{}",
        o.url()
    );
    assert_eq!(o.show("queries/a.sql").as_deref(), Some("select 1;\n"));
    let before = o.tip();
    o.teammate("queries/a.sql", "select 3;\n");
    assert_ne!(o.tip(), before);
    assert_eq!(o.show("queries/a.sql").as_deref(), Some("select 3;\n"));
}

#[test]
fn a_local_commit_stays_local_and_a_hook_refuses_the_push() {
    let o = Origin::seeded(&[("queries/a.sql", "select 1;\n")]);
    let ws = o.clone_to("ws");
    o.commit_in(&ws, "queries/a.sql", "select 2;\n");
    assert_eq!(o.show("queries/a.sql").as_deref(), Some("select 1;\n"));
    o.hook("pre-receive", "cat >/dev/null\necho no >&2\nexit 1");
    let pushed = Command::new("git")
        .current_dir(&ws)
        .envs(o.git_env())
        .args(["push", "-q", "origin", "main"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!pushed.status.success());
    assert!(String::from_utf8_lossy(&pushed.stderr).contains("remote: no"));
}
