use crate::potluck::{answer, blocking, info, session};
use dbento_core::{ApiError, PotluckClient, ServerUrl, TeamToken, public_url_of};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn token() -> TeamToken {
    TeamToken::parse(&format!("plk_{}", "T".repeat(43))).unwrap()
}

fn client(s: &MockServer) -> PotluckClient {
    PotluckClient::new(&ServerUrl::parse(&s.uri()).unwrap(), false).unwrap()
}

fn error(status: u16, code: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({"error": {"code": code, "message": message, "field": null}}))
}

// ApiError is #[non_exhaustive], so a catch-all arm is required.
#[allow(clippy::wildcard_enum_match_arm)]
fn kind(e: &ApiError) -> String {
    match e {
        ApiError::Unauthenticated => "unauthenticated".into(),
        ApiError::Forbidden(m) => format!("forbidden:{m}"),
        ApiError::NotFound(_) => "not_found".into(),
        ApiError::RateLimited { retry_after_secs } => format!("rate_limited:{retry_after_secs:?}"),
        ApiError::Server { code, message } => format!("server:{code}:{message}"),
        ApiError::Protocol(_) => "protocol".into(),
        ApiError::Offline(_) => "offline".into(),
        ApiError::Incompatible(_) => "incompatible".into(),
        other => format!("other:{other:?}"),
    }
}

#[tokio::test]
async fn server_info_needs_api_1_and_the_dbento_kind() {
    for (api, kinds, expected) in [
        (1, json!({"apinata": {}, "dbento": {"formats": []}}), "ok"),
        (2, json!({"dbento": {"formats": []}}), "incompatible"),
        (1, json!({"apinata": {}}), "incompatible"),
    ] {
        let s = MockServer::start().await;
        answer(&s, "GET", "/api/v1/server", info(api, kinds)).await;
        let c = client(&s);
        let got = blocking(move || c.server_info())
            .await
            .map_or_else(|e| kind(&e), |_| "ok".into());
        assert_eq!(got, expected, "api {api}");
    }
}

#[tokio::test]
async fn sign_in_sends_the_dbento_token_name_and_reads_the_token() {
    let s = MockServer::start().await;
    let tok = token();
    Mock::given(method("POST"))
        .and(path("/api/v1/session"))
        .and(body_partial_json(
            json!({"username": "ada", "password": "pw", "token_name": "Dbento"}),
        ))
        .respond_with(session(&tok, 42, "ada"))
        .mount(&s)
        .await;
    let c = client(&s);
    let signed = blocking(move || c.sign_in("ada", "pw")).await.unwrap();
    assert_eq!(signed.token.expose(), tok.expose(), "token");
    assert_eq!(
        (signed.token_id.as_str(), signed.username.as_str()),
        ("42", "ada"),
        "token id and user"
    );
}

#[tokio::test]
async fn error_answers_map_by_status_and_never_carry_a_token() {
    let leaked = format!("bad plk_{}", "Z".repeat(43));
    let cases = [
        (
            error(401, "unauthenticated", "no")
                .insert_header("www-authenticate", "Basic realm=\"potluck\""),
            "unauthenticated",
        ),
        (error(403, "forbidden", "not yours"), "forbidden:not yours"),
        (error(404, "not_found", "gone"), "not_found"),
        (
            error(429, "rate_limited", "slow").insert_header("retry-after", "30"),
            "rate_limited:Some(30)",
        ),
        (error(500, "internal", "boom"), "server:internal:boom"),
        (
            error(500, "internal", &leaked),
            "server:internal:bad plk_[redacted]",
        ),
        (
            ResponseTemplate::new(502).set_body_string("<html>"),
            "server::",
        ),
        (
            ResponseTemplate::new(200).set_body_string("not json"),
            "protocol",
        ),
        (
            ResponseTemplate::new(302).insert_header("location", "https://elsewhere.example/"),
            "protocol",
        ),
    ];
    for (response, expected) in cases {
        let s = MockServer::start().await;
        answer(&s, "GET", "/api/v1/me", response).await;
        let c = client(&s);
        assert_eq!(
            kind(&blocking(move || c.me(&token())).await.unwrap_err()),
            expected,
            "case {expected}"
        );
    }
}

#[tokio::test]
async fn workspaces_asks_for_the_dbento_kind_with_the_bearer_token() {
    let s = MockServer::start().await;
    let tok = token();
    Mock::given(method("GET"))
        .and(path("/api/v1/workspaces"))
        .and(query_param("kind", "dbento"))
        .and(header("authorization", format!("Bearer {}", tok.expose())))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([crate::potluck::ws(
                "https://t.example/git/acme/shared.git",
                "acme",
                "shared",
                "write"
            )])),
        )
        .mount(&s)
        .await;
    let c = client(&s);
    let list = blocking(move || c.workspaces(&tok)).await.unwrap();
    let w = &list[0];
    assert_eq!(
        (
            list.len(),
            w.team.as_str(),
            w.slug.as_str(),
            w.default_branch.as_str(),
            w.role.as_str()
        ),
        (1, "acme", "shared", "main", "write"),
        "the one workspace"
    );
}

#[tokio::test]
async fn revoke_deletes_by_id_and_a_closed_port_is_offline() {
    // A pooled server keeps listening after drop; a bare one closes its port.
    let s = MockServer::builder().start().await;
    answer(
        &s,
        "DELETE",
        "/api/v1/tokens/42",
        ResponseTemplate::new(204),
    )
    .await;
    let c = client(&s);
    blocking(move || c.revoke(&token(), "42")).await.unwrap();
    let url = s.uri();
    drop(s);
    let c = PotluckClient::new(&ServerUrl::parse(&url).unwrap(), false).unwrap();
    assert_eq!(
        kind(&blocking(move || c.server_info()).await.unwrap_err()),
        "offline",
        "closed port"
    );
}

#[test]
fn plain_http_to_a_remote_host_is_refused_unless_allowed() {
    let remote = ServerUrl::parse("http://team.example.com").unwrap();
    assert!(
        matches!(
            PotluckClient::new(&remote, false),
            Err(ApiError::InsecureHttp)
        ),
        "remote http refused"
    );
    assert!(
        PotluckClient::new(&remote, true).is_ok(),
        "remote http allowed"
    );
    assert!(
        PotluckClient::new(&ServerUrl::parse("http://127.0.0.1:9").unwrap(), false).is_ok(),
        "loopback http"
    );
}

#[test]
fn the_public_url_is_everything_before_the_last_git_segment() {
    let p = |u: &str| public_url_of(u).unwrap();
    assert_eq!(
        p("https://team.example.com/git/acme/shared.git"),
        "https://team.example.com",
        "root"
    );
    assert_eq!(
        p("https://h.example:8443/potluck/git/acme/shared.git"),
        "https://h.example:8443/potluck",
        "prefix"
    );
    assert!(
        public_url_of("https://team.example.com/acme/shared.git").is_err(),
        "no git segment"
    );
    assert!(
        public_url_of("ssh://git@team.example.com/git/a/b.git").is_err(),
        "ssh"
    );
    if cfg!(debug_assertions) {
        assert_eq!(
            p("file:///home/u/git/tmp/git/a/b.git"),
            "file:///home/u/git/tmp",
            "file in debug"
        );
    }
}

/// A signal landing while a call waits for its answer, as a git child's exit does, is not a
/// lost connection.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_signal_during_a_call_is_not_offline() {
    use std::os::unix::thread::JoinHandleExt;
    extern "C" fn ignore(_: libc::c_int) {}
    // The default SIGUSR1 action ends the process, so a no-op handler goes in first, once.
    static HANDLER: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    HANDLER.get_or_init(|| {
        // SAFETY: a zeroed sigaction with a handler that does nothing is a valid action.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = ignore as extern "C" fn(libc::c_int) as libc::sighandler_t;
            act.sa_flags = libc::SA_RESTART;
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &act, std::ptr::null_mut()),
                0
            );
        }
    });
    let s = MockServer::start().await;
    let slow = crate::potluck::me("ada").set_delay(std::time::Duration::from_millis(300));
    answer(&s, "GET", "/api/v1/me", slow).await;
    let c = client(&s);
    let call = std::thread::spawn(move || c.me(&token()));
    // Signal the caller until it has its answer, so signals land while it waits to read.
    while !call.is_finished() {
        // SAFETY: the thread is not joined yet, so its id is still valid.
        unsafe { libc::pthread_kill(call.as_pthread_t(), libc::SIGUSR1) };
        std::thread::yield_now();
    }
    assert_eq!(
        call.join().unwrap().map_err(|e| kind(&e)),
        Ok("ada".to_owned())
    );
}
