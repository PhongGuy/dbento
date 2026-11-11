use dbento_core::{Color, Edit, EditError, FolderPath, Workspace, plan_edit};

pub const TREE: &str = "dbento = 1\n\n# Teams.\n[folders.team] # top\ncolor = \"blue\" # team colour\n\n[folders.\"team/a b\"]\ncolor = \"green\"\n\n[connections.prod]\nengine = \"sqlite\"\npath = \"p.db\"\nfolder = \"team/a b\" # nested\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"l.db\"\nfolder = \"team\"\n\n[connections.top]\nengine = \"sqlite\"\npath = \"t.db\"\n";
pub const IMPLIED: &str =
    "dbento = 1\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\nfolder = \"x/y\"\n";

pub fn ws(text: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), text).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}
pub fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}
pub fn text(w: &Workspace, edit: Edit) -> Result<String, EditError> {
    plan_edit(w, &edit).map(|p| p.new_text().to_owned())
}
fn put(w: &Workspace, p: &str, color: Option<Color>) -> Result<String, EditError> {
    text(
        w,
        Edit::PutFolder {
            path: path(p),
            color,
        },
    )
}
fn rename(w: &Workspace, from: &str, to: &str) -> Result<String, EditError> {
    text(
        w,
        Edit::RenameFolder {
            from: path(from),
            to: path(to),
        },
    )
}
fn own_color(w: &Workspace, p: &str) -> Option<Option<Color>> {
    w.folders()
        .iter()
        .find(|f| f.path().as_str() == p)
        .map(|f| f.color())
}

#[test]
fn a_new_folder_is_declared_after_the_last_table() {
    let (_d, w) = ws(TREE);
    let plan = plan_edit(
        &w,
        &Edit::PutFolder {
            path: path("new"),
            color: None,
        },
    )
    .unwrap();
    assert_eq!(plan.new_text(), format!("{TREE}\n[folders.new]\n"));
    let f = plan
        .workspace()
        .folders()
        .iter()
        .find(|f| f.path().as_str() == "new")
        .unwrap();
    assert!(f.declared());
    let (_d, w) = ws("dbento = 1\n");
    assert_eq!(
        put(&w, "n", Some(Color::Red)).unwrap(),
        "dbento = 1\n\n[folders.n]\ncolor = \"red\"\n"
    );
}

#[test]
fn a_recolour_keeps_the_comment_and_a_cleared_colour_goes() {
    let (_d, w) = ws(TREE);
    assert_eq!(
        put(&w, "team", Some(Color::Red)).unwrap(),
        TREE.replace(
            "color = \"blue\" # team colour",
            "color = \"red\" # team colour"
        )
    );
    assert_eq!(
        put(&w, "team", None).unwrap(),
        TREE.replace("color = \"blue\" # team colour\n", "")
    );
    assert_eq!(
        put(&w, "team", Some(Color::Blue)).unwrap(),
        TREE,
        "unchanged"
    );
}

#[test]
fn an_implied_folder_is_declared_only_when_its_colour_changes() {
    let (_d, w) = ws(IMPLIED);
    let plan = plan_edit(
        &w,
        &Edit::PutFolder {
            path: path("x"),
            color: Some(Color::Red),
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        format!("{IMPLIED}\n[folders.x]\ncolor = \"red\"\n")
    );
    assert_eq!(own_color(plan.workspace(), "x"), Some(Some(Color::Red)));
    assert_eq!(put(&w, "x", None).unwrap(), IMPLIED);
}

#[test]
fn odd_names_are_quoted_and_round_trip() {
    let (_d, w) = ws(TREE);
    let odd = "Q \"x\"\\y.z";
    let plan = plan_edit(
        &w,
        &Edit::PutFolder {
            path: path(odd),
            color: Some(Color::Red),
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        format!("{TREE}\n[folders.'Q \"x\"\\y.z']\ncolor = \"red\"\n")
    );
    assert_eq!(own_color(plan.workspace(), odd), Some(Some(Color::Red)));
    let both = "it's \"q\"";
    let plan = plan_edit(
        &w,
        &Edit::PutFolder {
            path: path(both),
            color: None,
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        format!("{TREE}\n[folders.\"it's \\\"q\\\"\"]\n")
    );
    assert!(own_color(plan.workspace(), both).is_some());
}

#[test]
fn rename_keeps_comments_and_quotes_new_keys() {
    let (_d, w) = ws(TREE);
    let want = TREE
        .replace("[folders.team] # top", "[folders.crew] # top")
        .replace("[folders.\"team/a b\"]", "[folders.\"crew/a b\"]")
        .replace(
            "folder = \"team/a b\" # nested",
            "folder = \"crew/a b\" # nested",
        )
        .replace("folder = \"team\"\n", "folder = \"crew\"\n");
    assert_eq!(rename(&w, "team", "crew").unwrap(), want);
    let want = TREE
        .replace("[folders.team] # top", "[folders.\"x/y z\"] # top")
        .replace("[folders.\"team/a b\"]", "[folders.\"x/y z/a b\"]")
        .replace(
            "folder = \"team/a b\" # nested",
            "folder = \"x/y z/a b\" # nested",
        )
        .replace("folder = \"team\"\n", "folder = \"x/y z\"\n");
    assert_eq!(rename(&w, "team", "x/y z").unwrap(), want);
    assert_eq!(rename(&w, "team", "team").unwrap(), TREE, "unchanged");
}

#[test]
fn dotted_and_crlf_files_keep_their_form() {
    let dotted = "dbento = 1\nfolders.team.color = \"blue\"\n\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\nfolder = \"team\"\n";
    let (_d, w) = ws(dotted);
    assert_eq!(
        rename(&w, "team", "crew").unwrap(),
        dotted.replace("team", "crew")
    );
    let crlf = TREE.replace('\n', "\r\n");
    let (_d, w) = ws(&crlf);
    assert_eq!(
        put(&w, "new", None).unwrap(),
        format!("{crlf}\r\n[folders.new]\r\n")
    );
    assert!(
        rename(&w, "team", "crew")
            .unwrap()
            .contains("[folders.crew] # top\r\n")
    );
}

#[test]
fn folder_refusals_name_the_problem() {
    let (_d, w) = ws(TREE);
    assert_eq!(
        rename(&w, "nope", "x").err(),
        Some(EditError::FolderNotFound { path: path("nope") })
    );
    assert_eq!(
        rename(&w, "team/a b", "team").err(),
        Some(EditError::FolderExists { path: path("team") })
    );
    assert_eq!(
        rename(&w, "team", "team/x").err(),
        Some(EditError::IntoItself { path: path("team") })
    );
    let zw = "a\u{200B}b";
    assert_eq!(
        put(&w, zw, None).err(),
        Some(EditError::Invisible { path: path(zw) })
    );
    assert_eq!(
        rename(&w, "team", zw).err(),
        Some(EditError::Invisible { path: path(zw) })
    );
    let message = EditError::Invisible { path: path(zw) }.to_string();
    assert!(message.contains("\\u{200b}"), "{message}");
}

#[test]
fn inline_entries_are_refused() {
    let (_d, w) = ws("dbento = 1\n[folders]\nteam = { color = \"blue\" }\n");
    let unmergeable = |entry: &str| {
        Some(EditError::Unmergeable {
            entry: entry.into(),
        })
    };
    assert_eq!(put(&w, "team", Some(Color::Red)).err(), unmergeable("team"));
    assert_eq!(rename(&w, "team", "crew").err(), unmergeable("team"));
    let (_d, w) = ws("dbento = 1\nfolders = { team = { color = \"blue\" } }\n");
    assert_eq!(put(&w, "new", None).err(), unmergeable("folders"));
    let (_d, w) = ws(
        "dbento = 1\n[connections]\nx = { engine = \"sqlite\", path = \"x.db\", folder = \"team\" }\n",
    );
    assert_eq!(rename(&w, "team", "crew").err(), unmergeable("x"));
}

#[test]
fn bidi_isolates_and_other_invisible_marks_are_refused() {
    let (_d, w) = ws(TREE);
    for c in [
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{061C}', '\u{180E}',
    ] {
        let p = format!("a{c}b");
        assert_eq!(
            put(&w, &p, None).err(),
            Some(EditError::Invisible { path: path(&p) }),
            "{:?}",
            c
        );
    }
}

use dbento_core::WorkspaceItem;

fn mv(w: &Workspace, item: WorkspaceItem, to: Option<&str>) -> Result<String, EditError> {
    text(
        w,
        Edit::Move {
            item,
            to: to.map(path),
        },
    )
}
fn conn(n: &str) -> WorkspaceItem {
    WorkspaceItem::Connection(n.into())
}
fn folder(p: &str) -> WorkspaceItem {
    WorkspaceItem::Folder(path(p))
}
fn delete(w: &Workspace, p: &str) -> Result<String, EditError> {
    text(w, Edit::DeleteFolder { path: path(p) })
}

#[test]
fn moving_a_connection_touches_only_its_folder_key() {
    let (_d, w) = ws(TREE);
    assert_eq!(
        mv(&w, conn("top"), Some("team")).unwrap(),
        format!("{TREE}folder = \"team\"\n")
    );
    assert_eq!(
        mv(&w, conn("prod"), Some("team")).unwrap(),
        TREE.replace(
            "folder = \"team/a b\" # nested",
            "folder = \"team\" # nested"
        )
    );
    assert_eq!(
        mv(&w, conn("lite"), None).unwrap(),
        TREE.replace("path = \"l.db\"\nfolder = \"team\"\n", "path = \"l.db\"\n")
    );
    assert_eq!(
        mv(&w, conn("lite"), Some("team")).unwrap(),
        TREE,
        "unchanged"
    );
    let plan = plan_edit(
        &w,
        &Edit::Move {
            item: conn("top"),
            to: Some(path("new/sub")),
        },
    )
    .unwrap();
    let paths: Vec<&str> = plan
        .workspace()
        .folders()
        .iter()
        .map(|f| f.path().as_str())
        .collect();
    assert!(
        paths.contains(&"new") && paths.contains(&"new/sub"),
        "{paths:?}"
    );
}

#[test]
fn an_implied_folder_goes_with_its_last_connection() {
    let (_d, w) = ws(IMPLIED);
    let plan = plan_edit(
        &w,
        &Edit::Move {
            item: conn("a"),
            to: None,
        },
    )
    .unwrap();
    assert!(plan.workspace().folders().is_empty());
}

#[test]
fn moving_a_folder_renames_its_subtree() {
    let (_d, w) = ws(TREE);
    let want = TREE
        .replace("[folders.\"team/a b\"]", "[folders.\"a b\"]")
        .replace(
            "folder = \"team/a b\" # nested",
            "folder = \"a b\" # nested",
        );
    assert_eq!(mv(&w, folder("team/a b"), None).unwrap(), want);
    assert_eq!(
        mv(&w, folder("team/a b"), Some("team")).unwrap(),
        TREE,
        "unchanged"
    );
    assert_eq!(
        mv(&w, folder("team"), Some("team/a b")).err(),
        Some(EditError::IntoItself { path: path("team") })
    );
    assert_eq!(
        mv(&w, folder("team"), Some("team")).err(),
        Some(EditError::IntoItself { path: path("team") })
    );
    let clash = format!("{TREE}\n[folders.\"a b\"]\n");
    let (_d, w) = ws(&clash);
    assert_eq!(
        mv(&w, folder("team/a b"), None).err(),
        Some(EditError::FolderExists { path: path("a b") })
    );
}

#[test]
fn move_refusals_name_the_problem() {
    let (_d, w) = ws(TREE);
    assert_eq!(
        mv(&w, conn("nope"), None).err(),
        Some(EditError::NotFound {
            name: "nope".into()
        })
    );
    assert_eq!(
        mv(&w, folder("nope"), None).err(),
        Some(EditError::FolderNotFound { path: path("nope") })
    );
    let zw = "a\u{FEFF}";
    assert_eq!(
        mv(&w, conn("top"), Some(zw)).err(),
        Some(EditError::Invisible { path: path(zw) })
    );
    let (_d, w) = ws("dbento = 1\n[connections]\nx = { engine = \"sqlite\", path = \"x.db\" }\n");
    assert_eq!(
        mv(&w, conn("x"), Some("t")).err(),
        Some(EditError::Unmergeable { entry: "x".into() })
    );
}

#[test]
fn deleting_a_folder_moves_its_contents_up() {
    let (_d, w) = ws(TREE);
    let plan = plan_edit(&w, &Edit::DeleteFolder { path: path("team") }).unwrap();
    assert_eq!(
        plan.new_text(),
        "dbento = 1\n\n[folders.\"a b\"]\ncolor = \"green\"\n\n[connections.prod]\nengine = \"sqlite\"\npath = \"p.db\"\nfolder = \"a b\" # nested\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"l.db\"\n\n[connections.top]\nengine = \"sqlite\"\npath = \"t.db\"\n"
    );
    let kept = plan
        .workspace()
        .folders()
        .iter()
        .find(|f| f.path().as_str() == "a b")
        .unwrap();
    assert_eq!(kept.color(), Some(Color::Green), "colours below are kept");
    let want = TREE
        .replace("[folders.\"team/a b\"]\ncolor = \"green\"\n\n", "")
        .replace(
            "folder = \"team/a b\" # nested",
            "folder = \"team\" # nested",
        );
    assert_eq!(delete(&w, "team/a b").unwrap(), want);
    let (_d, w) = ws(IMPLIED);
    assert_eq!(delete(&w, "x").unwrap(), IMPLIED.replace("x/y", "y"));
}

#[test]
fn delete_refusals_name_the_problem() {
    let (_d, w) = ws(TREE);
    assert_eq!(
        delete(&w, "nope").err(),
        Some(EditError::FolderNotFound { path: path("nope") })
    );
    let (_d, w) = ws(&format!("{TREE}\n[folders.\"a b\"]\n"));
    assert_eq!(
        delete(&w, "team").err(),
        Some(EditError::FolderExists { path: path("a b") })
    );
}

#[test]
fn folder_after_maps_old_paths_to_new_ones() {
    let (_d, w) = ws(TREE);
    let after = |edit: Edit, old: &str| plan_edit(&w, &edit).unwrap().folder_after(&path(old));
    let rename = || Edit::RenameFolder {
        from: path("team"),
        to: path("crew"),
    };
    assert_eq!(after(rename(), "team"), Some(path("crew")));
    assert_eq!(after(rename(), "team/a b"), Some(path("crew/a b")));
    assert_eq!(after(rename(), "teams"), Some(path("teams")));
    let moved = || Edit::Move {
        item: folder("team/a b"),
        to: None,
    };
    assert_eq!(after(moved(), "team/a b"), Some(path("a b")));
    assert_eq!(after(moved(), "team"), Some(path("team")));
    let del = || Edit::DeleteFolder { path: path("team") };
    assert_eq!(after(del(), "team"), None);
    assert_eq!(after(del(), "team/a b"), Some(path("a b")));
    assert_eq!(
        after(
            Edit::Move {
                item: conn("top"),
                to: None
            },
            "team"
        ),
        Some(path("team"))
    );
}

#[test]
fn a_folder_edit_applies_and_reopens() {
    let (d, w) = ws(TREE);
    plan_edit(&w, &Edit::DeleteFolder { path: path("team") })
        .unwrap()
        .apply()
        .unwrap();
    let back = Workspace::open(d.path()).unwrap();
    assert_eq!(back.declared("prod").unwrap().folder(), Some(&path("a b")));
    assert!(back.folders().iter().all(|f| f.path().as_str() != "team"));
}

// A hand-written path with a zero-width space or BOM names the folder it looks like.
const ZERO_WIDTH: &str = "dbento = 1\n[folders.\"team\u{200B}\"]\ncolor = \"blue\"\n\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\nfolder = \"\u{FEFF}team\"\n\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\nfolder = \"team/s\u{200B}ub\"\n";

#[test]
fn zero_width_characters_in_hand_written_paths_load_as_one_folder() {
    let (_d, w) = ws(ZERO_WIDTH);
    let paths: Vec<&str> = w.folders().iter().map(|f| f.path().as_str()).collect();
    assert_eq!(paths, ["team", "team/sub"]);
    assert_eq!(own_color(&w, "team"), Some(Some(Color::Blue)));
    assert_eq!(w.declared("a").unwrap().folder(), Some(&path("team")));
    assert_eq!(w.declared("b").unwrap().folder(), Some(&path("team/sub")));
    assert_eq!(w.connection_spec("a").unwrap().folder, Some(path("team")));
}

#[test]
fn folder_edits_find_a_hand_written_key_with_zero_width_characters() {
    let (_d, w) = ws(ZERO_WIDTH);
    let recoloured = put(&w, "team", Some(Color::Red)).unwrap();
    assert_eq!(recoloured, ZERO_WIDTH.replace("\"blue\"", "\"red\""));
    let renamed = rename(&w, "team", "crew").unwrap();
    assert!(!renamed.contains("team"), "{renamed}");
    assert!(
        renamed.contains("[folders.crew]\ncolor = \"blue\""),
        "{renamed}"
    );
    let deleted = delete(&w, "team").unwrap();
    assert!(!deleted.contains("[folders"), "{deleted}");
}
