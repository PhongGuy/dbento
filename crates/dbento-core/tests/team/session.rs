use crate::potluck::{answer, blocking, fresh_token, hits, info, me, session, ws};
use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, ApiError, BindingRecord, Credential, LoginError, MemoryTokenStore, RefreshError,
    ServerUrl, TeamAccount, TeamFile, TeamToken, TokenStore, WorkspacesError, refresh_role,
    team_login, team_logout, team_workspaces,
};
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Env {
    _tmp: tempfile::TempDir,
    paths: StatePaths,
    tokens: Arc<MemoryTokenStore>,
    server: ServerUrl,
}

fn env(uri: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = StatePaths::in_dir(tmp.path());
    Env {
        _tmp: tmp,
        paths,
        tokens: Arc::new(MemoryTokenStore::new()),
        server: ServerUrl::parse(uri).unwrap(),
    }
}
impl Env {
    fn ada(&self) -> TeamAccount {
        TeamAccount::new(self.server.clone(), "ada")
    }
    /// ada recorded with token id 42 and, when given, `tok` in the store.
    fn signed_in(&self, tok: Option<&TeamToken>) {
        TeamFile::new(&self.paths.team)
            .update(|d| {
                d.put_account(AccountRecord::new(
                    &self.server,
                    "ada",
                    Some("42".into()),
                    false,
                ))
            })
            .unwrap();
        if let Some(t) = tok {
            self.tokens.set(&self.ada(), t).unwrap();
        }
    }
    async fn login(&self, user: &str, c: Credential) -> Result<AccountRecord, LoginError> {
        let (t, p, s, u) = (
            Arc::clone(&self.tokens),
            self.paths.clone(),
            self.server.clone(),
            user.to_owned(),
        );
        blocking(move || team_login(t.as_ref(), &p, &s, &u, c, false)).await
    }
}
fn dbento() -> serde_json::Value {
    json!({"dbento": {"formats": []}})
}

#[tokio::test]
async fn login_keeps_the_token_in_the_store_and_only_the_account_in_the_file() {
    let s = MockServer::start().await;
    let tok = fresh_token();
    answer(&s, "GET", "/api/v1/server", info(1, dbento())).await;
    answer(&s, "POST", "/api/v1/session", session(&tok, 42, "ada")).await;
    let e = env(&s.uri());
    let account = e
        .login("ada", Credential::Password("pw".into()))
        .await
        .unwrap();
    assert_eq!(account.token_id.as_deref(), Some("42"), "the token id");
    assert_eq!(
        e.tokens.get(&e.ada()).unwrap().unwrap().expose(),
        tok.expose(),
        "the stored token"
    );
    let text = std::fs::read_to_string(&e.paths.team).unwrap();
    assert!(
        !text.contains(tok.expose()) && !text.contains("pw"),
        "{text}"
    );
}

#[tokio::test]
async fn an_incompatible_or_refusing_server_stores_nothing() {
    for (api, kinds, answer_with) in [
        (2, dbento(), None),
        (1, json!({"apinata": {}}), None),
        (1, dbento(), Some(ResponseTemplate::new(401))),
        (
            1,
            dbento(),
            Some(ResponseTemplate::new(429).insert_header("retry-after", "30")),
        ),
    ] {
        let s = MockServer::start().await;
        answer(&s, "GET", "/api/v1/server", info(api, kinds)).await;
        let early = answer_with.is_none();
        if let Some(r) = answer_with {
            answer(&s, "POST", "/api/v1/session", r).await;
        }
        let e = env(&s.uri());
        let err = e
            .login("ada", Credential::Password("pw".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, LoginError::Api(_)), "{err:?}");
        if early {
            assert_eq!(
                hits(&s, "POST", "/api/v1/session").await,
                0,
                "the password was sent"
            );
        }
        assert!(!e.paths.team.exists(), "the team file was written");
        assert!(
            e.tokens.get(&e.ada()).unwrap().is_none(),
            "a token was stored"
        );
    }
}

#[tokio::test]
async fn a_token_login_checks_the_user_with_me() {
    let s = MockServer::start().await;
    answer(&s, "GET", "/api/v1/server", info(1, dbento())).await;
    answer(&s, "GET", "/api/v1/me", me("ada")).await;
    let e = env(&s.uri());
    assert_eq!(
        e.login("ada", Credential::Token(fresh_token()))
            .await
            .unwrap()
            .token_id,
        None,
        "a pasted token has no id"
    );
    let err = e
        .login("bob", Credential::Token(fresh_token()))
        .await
        .unwrap_err();
    assert!(matches!(err, LoginError::UserMismatch { .. }), "{err:?}");
}

#[tokio::test]
async fn logout_revokes_and_forgets_the_account_but_keeps_bindings_even_offline() {
    let s = MockServer::start().await;
    let tok = fresh_token();
    Mock::given(method("DELETE"))
        .and(path("/api/v1/tokens/42"))
        .and(header("authorization", format!("Bearer {}", tok.expose())))
        .respond_with(ResponseTemplate::new(204))
        .mount(&s)
        .await;
    for online in [true, false] {
        let e = env(&s.uri());
        e.signed_in(Some(&tok));
        let dir = e.paths.team.parent().unwrap().join("ws");
        std::fs::create_dir(&dir).unwrap();
        let b = BindingRecord::new(
            e.server.as_str(),
            "https://t.example",
            "ada",
            "acme",
            "shared",
            "main",
            "write",
        );
        TeamFile::new(&e.paths.team)
            .update(|d| d.put_binding(&dir, b))
            .unwrap()
            .unwrap();
        let e = if online {
            e
        } else {
            Env {
                server: ServerUrl::parse("http://127.0.0.1:9").unwrap(),
                ..e
            }
        };
        if !online {
            e.signed_in(Some(&tok));
        }
        let (t, p, sv) = (Arc::clone(&e.tokens), e.paths.clone(), e.server.clone());
        let report = blocking(move || team_logout(t.as_ref(), &p, &sv, Some("ada")))
            .await
            .unwrap();
        assert_eq!(report.revoked, online, "{report:?}");
        assert_eq!(report.note.is_some(), !online, "{report:?}");
        assert!(
            e.tokens.get(&e.ada()).unwrap().is_none(),
            "the token is still stored"
        );
        let data = TeamFile::new(&e.paths.team).load().unwrap();
        assert!(
            data.account(&e.server, Some("ada")).is_none(),
            "the account is still recorded"
        );
        assert_eq!(data.bindings().len(), 1, "the binding was dropped");
    }
}

#[tokio::test]
async fn the_workspace_list_needs_a_token_and_comes_back_sorted() {
    let s = MockServer::start().await;
    answer(
        &s,
        "GET",
        "/api/v1/workspaces",
        ResponseTemplate::new(200).set_body_json(json!([
            ws("https://t.example/git/beta/x.git", "beta", "x", "read"),
            ws(
                "https://t.example/git/acme/shared.git",
                "acme",
                "shared",
                "write"
            )
        ])),
    )
    .await;
    let e = env(&s.uri());
    e.signed_in(None);
    let list = |e: &Env| {
        let (t, p, sv) = (Arc::clone(&e.tokens), e.paths.clone(), e.server.clone());
        blocking(move || team_workspaces(t.as_ref(), &p, &sv, None))
    };
    assert!(
        matches!(
            list(&e).await.unwrap_err(),
            WorkspacesError::SignedOut { .. }
        ),
        "a missing token is signed out"
    );
    e.tokens.set(&e.ada(), &fresh_token()).unwrap();
    let names: Vec<String> = list(&e)
        .await
        .unwrap()
        .iter()
        .map(|w| format!("{}/{}", w.team, w.slug))
        .collect();
    assert_eq!(names, ["acme/shared", "beta/x"], "sorted by team and slug");
}

#[tokio::test]
async fn refresh_role_rewrites_a_changed_role_and_reports_lost_access() {
    let s = MockServer::start().await;
    answer(
        &s,
        "GET",
        "/api/v1/workspaces",
        ResponseTemplate::new(200).set_body_json(json!([ws(
            "https://t.example/git/acme/shared.git",
            "acme",
            "shared",
            "read"
        )])),
    )
    .await;
    let e = env(&s.uri());
    e.signed_in(Some(&fresh_token()));
    let dir = e.paths.team.parent().unwrap().join("ws");
    std::fs::create_dir(&dir).unwrap();
    let put = |slug: &str| {
        let b = BindingRecord::new(
            e.server.as_str(),
            "https://t.example",
            "ada",
            "acme",
            slug,
            "main",
            "write",
        );
        TeamFile::new(&e.paths.team)
            .update(|d| d.put_binding(&dir, b))
            .unwrap()
            .unwrap();
    };
    let refresh = || {
        let (t, p, d) = (Arc::clone(&e.tokens), e.paths.clone(), dir.clone());
        blocking(move || refresh_role(t.as_ref(), &p, &d))
    };
    put("shared");
    assert_eq!(refresh().await.unwrap().role, "read", "the returned role");
    assert_eq!(
        TeamFile::new(&e.paths.team)
            .load()
            .unwrap()
            .binding_for(&dir)
            .unwrap()
            .1
            .role,
        "read",
        "the recorded role"
    );
    put("gone");
    assert!(
        matches!(refresh().await.unwrap_err(), RefreshError::NoAccess),
        "a workspace missing from the list is lost access"
    );
    s.reset().await;
    answer(&s, "GET", "/api/v1/workspaces", ResponseTemplate::new(401)).await;
    put("shared");
    assert!(
        matches!(
            refresh().await.unwrap_err(),
            RefreshError::Api(ApiError::Unauthenticated)
        ),
        "a refused token is the API's error"
    );
}
