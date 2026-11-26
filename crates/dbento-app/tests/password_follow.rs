//! What a connection form save does to the keychain password.
use dbento_app::password_follow::{
    PasswordFollow, apply_password_follow, password_follow, pin_moves,
};
use dbento_core::{MemoryStore, Secret, SecretKey, SecretStore, Workspace};
use std::path::PathBuf;

fn at_w(body: &str) -> Workspace {
    let dir = PathBuf::from("/w");
    Workspace::from_text(
        dir.clone(),
        &dir.join("dbento.toml"),
        &format!("dbento = 1\n{body}"),
    )
    .unwrap()
}
fn pg(name: &str, host: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"d\"\nuser = \"u\"\n"
    )
}
fn none(body: String) -> String {
    format!("{body}secret = \"none\"\n")
}
fn key(ws: &Workspace, name: &str) -> SecretKey {
    ws.secret_target(name).unwrap()
}

#[test]
fn a_rename_on_the_same_server_moves_the_password() {
    let (base, now) = (at_w(&pg("a", "h")), at_w(&pg("b", "h")));
    let want = PasswordFollow {
        moved: Some((key(&base, "a"), key(&now, "b"))),
        drops: vec![],
        to: Some(key(&now, "b")),
    };
    assert_eq!(
        password_follow(&base, "a", &now, "b", false),
        want,
        "same pin moves"
    );
}

#[test]
fn a_changed_server_drops_the_old_password_with_or_without_a_rename() {
    for new_name in ["a", "b"] {
        let (base, now) = (at_w(&pg("a", "h")), at_w(&pg(new_name, "elsewhere")));
        let f = password_follow(&base, "a", &now, new_name, false);
        assert_eq!(f.moved, None, "nothing follows to {new_name}");
        assert_eq!(
            f.drops,
            vec![key(&base, "a")],
            "the old entry drops for {new_name}"
        );
        assert_eq!(
            f.to,
            Some(key(&now, new_name)),
            "a typed one lands under the new pin"
        );
    }
}

#[test]
fn an_edit_outside_the_target_moves_and_drops_nothing() {
    let base = at_w(&pg("a", "h"));
    let now = at_w(&pg("a", "h").replace("database = \"d\"", "database = \"e\""));
    let want = PasswordFollow {
        moved: None,
        drops: vec![],
        to: Some(key(&now, "a")),
    };
    assert_eq!(
        password_follow(&base, "a", &now, "a", false),
        want,
        "the database is not pinned"
    );
}

#[test]
fn a_typed_password_also_drops_the_legacy_entry() {
    let (base, now) = (at_w(&pg("a", "h")), at_w(&pg("a", "h")));
    let f = password_follow(&base, "a", &now, "a", true);
    assert_eq!(
        f.drops,
        vec![SecretKey::legacy_password("/w", "a")],
        "the legacy entry drops"
    );
}

#[test]
fn leaving_the_keychain_drops_the_password_and_stores_nothing() {
    let (base, now) = (at_w(&pg("a", "h")), at_w(&none(pg("a", "h"))));
    let want = PasswordFollow {
        moved: None,
        drops: vec![key(&base, "a")],
        to: None,
    };
    assert_eq!(
        password_follow(&base, "a", &now, "a", true),
        want,
        "no reader is left"
    );
}

#[test]
fn a_new_connection_has_nothing_to_move() {
    let (base, now) = (at_w(""), at_w(&pg("a", "h")));
    let want = PasswordFollow {
        moved: None,
        drops: vec![],
        to: Some(key(&now, "a")),
    };
    assert_eq!(
        password_follow(&base, "a", &now, "a", false),
        want,
        "nothing existed before"
    );
}

#[test]
fn pin_moves_only_for_a_keychain_password_whose_target_changed() {
    let base = at_w(&pg("a", "h"));
    assert!(
        !pin_moves(&base, "a", &at_w(&pg("b", "h")), "b"),
        "a rename keeps the target"
    );
    assert!(
        pin_moves(&base, "a", &at_w(&pg("a", "h2")), "a"),
        "another host moves it"
    );
    assert!(
        !pin_moves(&base, "a", &at_w(&none(pg("a", "h2"))), "a"),
        "no keychain password after"
    );
    assert!(
        !pin_moves(&at_w(""), "a", &at_w(&pg("a", "h")), "a"),
        "a new connection"
    );
}

#[test]
fn apply_stores_the_typed_password_under_the_new_pin_then_drops() {
    let (base, now) = (at_w(&pg("a", "h")), at_w(&pg("a", "elsewhere")));
    let store = MemoryStore::new();
    let legacy = SecretKey::legacy_password("/w", "a");
    store.set(&key(&base, "a"), &Secret::new("x")).unwrap();
    store.set(&legacy, &Secret::new("x")).unwrap();
    let f = password_follow(&base, "a", &now, "a", true);
    apply_password_follow(&store, &f, Some(&Secret::new("x"))).unwrap();
    assert_eq!(
        store.get(&key(&now, "a")).unwrap().unwrap().expose(),
        "x",
        "typed one stored"
    );
    assert!(
        store.get(&key(&base, "a")).unwrap().is_none(),
        "the old server's entry is gone"
    );
    assert!(
        store.get(&legacy).unwrap().is_none(),
        "the legacy entry is gone"
    );
}
