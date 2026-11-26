//! The connection list the app remembers per workspace, and reading it back.
use dbento_core::{
    RenameMatch, SEEN_TEXT_MAX, SecretKey, Workspace, secret_renames, seen_text, seen_workspace,
};
use std::path::{Path, PathBuf};

fn at(dir: &str, text: &str) -> Workspace {
    let dir = PathBuf::from(dir);
    Workspace::from_text(dir.clone(), &dir.join("dbento.toml"), text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Postgres connection `name` on host `h` as user `u` reading `db`.
fn pg(name: &str, db: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"{db}\"\nuser = \"u\"\n"
    )
}

fn names(ws: &Workspace) -> Vec<String> {
    ws.connections()
        .iter()
        .map(|c| c.name().to_owned())
        .collect()
}

const FAKE: &str = "Hk4pQ9zr";

#[test]
fn a_comment_is_dropped_and_every_connection_kept() {
    let text = format!(
        "dbento = 1\n# the old password was {FAKE}\n{}color = \"red\" # {FAKE}\n\n{}\n[folders.team]\ncolor = \"blue\"\n",
        pg("a", "d"),
        pg("b", "e")
    );
    let ws = at("/w", &text);
    let seen = seen_text(&ws).expect("a small valid file has a snapshot");
    assert!(!seen.contains(FAKE), "{seen}");
    assert!(
        seen.contains("[folders.team]") && seen.contains("red"),
        "{seen}"
    );
    let back = seen_workspace(&seen, &ws).expect("the snapshot validates");
    assert_eq!(names(&back), ["a", "b"]);
    for (was, now) in ws.connections().iter().zip(back.connections()) {
        assert!(was.same_session(now), "{}", was.name());
        assert!(was.differing_keys(now).is_empty(), "{}", was.name());
    }
}

#[test]
fn a_snapshot_over_the_cap_is_none() {
    let body: String = (0..6000)
        .map(|i| format!("[connections.c{i:05}]\nengine = \"sqlite\"\npath = \"a.db\"\n"))
        .collect();
    let big = at("/w", &format!("dbento = 1\n{body}"));
    assert!(big.source_text().len() > SEEN_TEXT_MAX);
    assert_eq!(seen_text(&big), None);
    let small = at("/w", &format!("dbento = 1\n{}", pg("a", "d")));
    assert!(seen_text(&small).is_some_and(|s| s.len() <= SEEN_TEXT_MAX));
}

#[test]
fn a_snapshot_read_back_finds_an_exact_rename() {
    let before = at("/w", &format!("dbento = 1\n{}", pg("a", "d")));
    let seen = seen_text(&before).unwrap();
    let after = at("/w", &format!("dbento = 1\n{}", pg("b", "d")));
    let old = seen_workspace(&seen, &after).unwrap();
    let r = secret_renames(&old, &after);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(
        (r[0].from.as_str(), r[0].to.as_str(), &r[0].matched),
        ("a", "b", &RenameMatch::Exact)
    );
}

#[test]
fn text_that_no_longer_validates_is_none() {
    let current = at("/w", "dbento = 1\n");
    for bad in [
        "",
        "dbento = 2\n",
        "dbento = 1\n[connections.\n",
        "dbento = 1\n[connections.a]\nengine = \"postgres\"\nuri = \"postgres://u:pw@h/d\"\n",
    ] {
        assert!(seen_workspace(bad, &current).is_none(), "{bad:?}");
    }
}

#[test]
fn a_snapshot_reads_back_under_the_folder_being_opened() {
    let seen = seen_text(&at("/old", &format!("dbento = 1\n{}", pg("a", "d")))).unwrap();
    let back = seen_workspace(&seen, &at("/new", "dbento = 1\n")).unwrap();
    assert_eq!(back.dir(), Path::new("/new"));
    let target = back.secret_target("a").unwrap();
    assert_eq!(target.workspace_dir(), Path::new("/new"));
    assert_eq!(target.connection(), "a");
    let pin = back.declared("a").unwrap().secret_pin();
    assert_eq!(target, SecretKey::password("/new", "a", &pin));
}
