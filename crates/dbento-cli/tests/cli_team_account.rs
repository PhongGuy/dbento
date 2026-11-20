use crate::common::team::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn stub(api: u32) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/server"))
        .respond_with(info(api))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn login_records_the_account_and_keeps_the_token_off_disk() {
    let s = stub(1).await;
    let tok = fresh_token();
    Mock::given(method("POST")).and(path("/api/v1/session"))
        .and(body_partial_json(json!({"username": "ada", "password": "pw", "token_name": "Dbento"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": tok, "user": user("ada"),
            "token_meta": {"id": 42, "name": "Dbento", "prefix": "plk_AAAA", "created_at": "x", "last_used_at": null, "expires_at": null}})))
        .mount(&s).await;
    let state = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = team(
        state.path(),
        None,
        &["login", &s.uri(), "--user", "ada", "--password-stdin"],
        "pw\n",
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("signed in as ada"), "{stderr}");
    assert_eq!(
        team_file(state.path())["accounts"][0]["token_id"],
        "42",
        "the token id is recorded"
    );
    assert!(
        !any_file_contains(state.path(), &tok) && !stdout.contains(&tok) && !stderr.contains(&tok),
        "the token stays off disk and out of the output"
    );
}

#[tokio::test]
async fn login_refusals_send_no_password_and_store_nothing() {
    let (ok, old) = (stub(1).await, stub(2).await);
    let (ok_uri, old_uri) = (ok.uri(), old.uri());
    let state = tempfile::tempdir().unwrap();
    for (args, needle) in [
        (
            vec![
                "login",
                "http://192.0.2.1",
                "--user",
                "ada",
                "--password-stdin",
            ],
            "--insecure-http",
        ),
        (
            vec!["login", ok_uri.as_str(), "--user", "ada"],
            "--password-stdin",
        ),
        (
            vec![
                "login",
                old_uri.as_str(),
                "--user",
                "ada",
                "--password-stdin",
            ],
            "",
        ),
    ] {
        let (code, _, stderr) = team(state.path(), None, &args, "pw\n").await;
        assert_eq!(code, 1, "{args:?}: {stderr}");
        assert!(stderr.contains(needle), "{stderr}");
    }
    assert_eq!(
        hits(&ok, "POST", "/api/v1/session").await + hits(&old, "POST", "/api/v1/session").await,
        0,
        "no password was sent"
    );
    assert!(
        !state.path().join("team.json").exists(),
        "nothing was stored"
    );
}

#[tokio::test]
async fn workspaces_lists_sorted_rows_and_exits_5_when_signed_out() {
    let s = stub(1).await;
    let tok = fresh_token();
    Mock::given(method("GET"))
        .and(path("/api/v1/workspaces"))
        .and(header("authorization", format!("Bearer {tok}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            ws_json(
                "https://t.example/git/beta/x.git",
                "beta",
                "x",
                "read",
                "dev"
            ),
            ws_json(
                "https://t.example/git/acme/shared.git",
                "acme",
                "shared",
                "write",
                "main"
            )
        ])))
        .mount(&s)
        .await;
    let state = tempfile::tempdir().unwrap();
    signed_in(state.path(), &s.uri());
    let (code, stdout, stderr) =
        team(state.path(), Some(&tok), &["workspaces", &s.uri()], "").await;
    assert_eq!(
        (code, stdout.as_str()),
        (0, "acme/shared  write  main\nbeta/x  read  dev\n"),
        "{stderr}"
    );
    let (code, _, stderr) = team(state.path(), None, &["workspaces", &s.uri()], "").await;
    assert_eq!(code, 5, "{stderr}");
    assert!(stderr.contains("dbento team login"), "{stderr}");
    // Any other token gets no matching mock, so the stub's 404 must not read as signed out.
    let (code, _, stderr) = team(
        state.path(),
        Some(&fresh_token()),
        &["workspaces", &s.uri()],
        "",
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
}

#[tokio::test]
async fn logout_revokes_with_the_kept_token_and_forgets_the_account() {
    let s = stub(1).await;
    let tok = fresh_token();
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tokens/42"))
        .and(header("authorization", format!("Bearer {tok}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&s)
        .await;
    let state = tempfile::tempdir().unwrap();
    signed_in(state.path(), &s.uri());
    let (code, _, stderr) = team(state.path(), Some(&tok), &["logout", &s.uri()], "").await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        hits(&s, "DELETE", "/api/v1/tokens/42").await,
        1,
        "the token was revoked once"
    );
    assert_eq!(
        team_file(state.path())["accounts"],
        json!([]),
        "the account is forgotten"
    );
}

#[tokio::test]
async fn a_usage_error_never_echoes_a_stray_argument() {
    let state = tempfile::tempdir().unwrap();
    let stray = fresh_token();
    let args = [
        "login",
        "https://t.example",
        "--user",
        "ada",
        stray.as_str(),
    ];
    let (code, stdout, stderr) = team(state.path(), None, &args, "").await;
    assert_eq!(code, 3, "{stderr}");
    assert!(
        !stdout.contains(&stray) && !stderr.contains(&stray),
        "{stderr}"
    );
    assert!(stderr.contains("dbento team"), "{stderr}");
}
