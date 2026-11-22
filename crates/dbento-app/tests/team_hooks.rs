//! The team hooks global, and the fixture every team test stands on.
use dbento_app::team::{hooks, install};
use dbento_core::{TeamFile, TokenStore as _};
use gpui_kit::TestAppContext;

use crate::support::team::{TOML, team_fx, team_fx_with};

#[gpui_kit::test]
fn hooks_are_absent_until_installed_and_the_first_install_stays(cx: &mut TestAppContext) {
    let (a, b) = (team_fx("write"), team_fx("read"));
    assert!(cx.update(|cx| hooks(cx)).is_none());
    cx.update(|cx| install(a.hooks(), cx));
    cx.update(|cx| install(b.hooks(), cx));
    let got = cx.update(|cx| hooks(cx)).unwrap();
    assert_eq!(got.paths().team, a.paths.team);
}

#[test]
fn a_bound_fixture_is_a_clone_bound_for_ada_with_a_token() {
    let fx = team_fx("write");
    assert_eq!(
        std::fs::read_to_string(fx.ws.join("dbento.toml")).unwrap(),
        TOML
    );
    assert_eq!(
        fx.origin_show("queries/a.sql").as_deref(),
        Some("select 1;\n")
    );
    let data = TeamFile::new(&fx.paths.team).load().unwrap();
    let (dir, rec) = data.binding_for(&fx.ws).unwrap();
    assert_eq!(dir, fx.ws);
    assert_eq!(
        (rec.username.as_str(), rec.branch.as_str()),
        ("ada", "main")
    );
    assert!(fx.tokens.get(&fx.account()).unwrap().is_some());
}

#[test]
fn an_unbound_fixture_has_no_binding_and_no_token() {
    let fx = team_fx_with("write", false);
    assert!(!fx.ws.join(".git").exists());
    assert!(
        TeamFile::new(&fx.paths.team)
            .load()
            .unwrap()
            .binding_for(&fx.ws)
            .is_none()
    );
    assert!(fx.tokens.get(&fx.account()).unwrap().is_none());
}
