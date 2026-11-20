//! A loopback Potluck stub and a per-run token for the team-mode API tests.
#![allow(dead_code)] // each test module uses a different subset

use std::hash::{BuildHasher, Hasher};

use dbento_core::TeamToken;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A token made fresh for this run, never written to a file.
pub fn fresh_token() -> TeamToken {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(32);
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        bytes.extend_from_slice(&h.finish().to_le_bytes());
    }
    TeamToken::parse(&format!(
        "plk_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
    .unwrap()
}

pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

pub async fn answer(s: &MockServer, verb: &str, at: &str, r: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(r)
        .mount(s)
        .await;
}

pub async fn hits(s: &MockServer, verb: &str, at: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == at)
        .count()
}

pub fn user(name: &str) -> Value {
    json!({"username": name, "display_name": name, "email": null, "server_admin": false,
           "disabled": false, "created_at": "x"})
}

pub fn info(api: u32, kinds: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"version": "0.3.0", "api": api,
        "auth": {"password": true, "oidc": false}, "kinds": kinds}))
}

pub fn session(tok: &TeamToken, id: i64, name: &str) -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({"token": tok.expose(), "user": user(name),
        "token_meta": {"id": id, "name": "Dbento", "prefix": "plk_AAAA", "created_at": "x",
                       "last_used_at": null, "expires_at": null}}))
}

pub fn me(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        json!({"user": user(name), "identity": {"name": name, "email": "x@x"}, "workspaces": []}),
    )
}

pub fn ws(git_url: &str, team: &str, slug: &str, role: &str) -> Value {
    json!({"team": team, "slug": slug, "name": slug, "kind": "dbento", "default_branch": "main",
           "role": role, "git_url": git_url,
           "events_url": format!("https://t.example/api/v1/workspaces/{team}/{slug}/events")})
}
