//! Folders in dbento.toml: grammar errors, implied folders and colour inheritance.
use dbento_core::{Color, OpenError, Workspace};

fn open(toml: &str) -> Result<Workspace, OpenError> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    Workspace::open(dir.path())
}

const TREE: &str = r#"dbento = 1
[folders."Prod"]
color = "red"
[folders."Prod/EU"]
[folders."Prod/US"]
color = "orange"
[folders."Dev"]
color = "green"
[folders."Empty"]
[connections.a]
engine = "sqlite"
path = "a.db"
folder = "Prod/EU"
[connections.b]
engine = "sqlite"
path = "b.db"
folder = "Prod/EU"
color = "blue"
[connections.c]
engine = "sqlite"
path = "c.db"
folder = "Dev/x"
[connections.d]
engine = "sqlite"
path = "d.db"
[connections.e]
engine = "sqlite"
path = "e.db"
folder = "Prod/US/z"
[connections.f]
engine = "sqlite"
path = "f.db"
folder = "prod"
"#;

#[test]
fn own_colour_wins_then_the_nearest_folder_with_one() {
    let ws = open(TREE).unwrap();
    let eff = |n: &str| ws.connection(n).unwrap().effective_color();
    assert_eq!(eff("a"), Some(Color::Red));
    assert_eq!(eff("b"), Some(Color::Blue));
    assert_eq!(eff("c"), Some(Color::Green));
    assert_eq!(eff("d"), None);
    assert_eq!(eff("e"), Some(Color::Orange));
    assert_eq!(eff("f"), None, "folder names are case-sensitive");
    let a = ws.connection("a").unwrap();
    assert!(a.is_production() && a.color().is_none());
    assert_eq!(a.folder().map(|f| f.as_str()), Some("Prod/EU"));
    assert!(!ws.connection("b").unwrap().is_production());
    assert!(ws.connection("d").unwrap().folder().is_none());
}

#[test]
fn folders_lists_declared_and_implied_in_tree_order() {
    let ws = open(TREE).unwrap();
    let got: Vec<(String, bool, Option<Color>, Option<Color>)> = ws
        .folders()
        .iter()
        .map(|f| {
            (
                f.path().to_string(),
                f.declared(),
                f.color(),
                f.effective_color(),
            )
        })
        .collect();
    use Color::*;
    let want: Vec<(String, bool, Option<Color>, Option<Color>)> = [
        ("Dev", true, Some(Green), Some(Green)),
        ("Dev/x", false, None, Some(Green)),
        ("Empty", true, None, None),
        ("Prod", true, Some(Red), Some(Red)),
        ("Prod/EU", true, None, Some(Red)),
        ("Prod/US", true, Some(Orange), Some(Orange)),
        ("Prod/US/z", false, None, Some(Orange)),
        ("prod", false, None, None),
    ]
    .into_iter()
    .map(|(p, d, c, e)| (p.to_owned(), d, c, e))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn no_folders_table_means_no_folders() {
    let ws = open(
        "dbento = 1\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\ncolor = \"red\"\n",
    )
    .unwrap();
    assert!(ws.folders().is_empty());
    assert!(ws.connection("a").unwrap().is_production());
}

fn folder_err(toml: &str) -> (String, String) {
    match open(toml).unwrap_err() {
        OpenError::InvalidFolder { folder, message } => (folder, message),
        other => panic!("expected InvalidFolder, got {other:?}"),
    }
}

#[test]
fn bad_folder_tables_are_open_errors() {
    let (f, m) = folder_err("dbento = 1\n[folders.\"Prod//EU\"]\n");
    assert_eq!(
        (f.as_str(), m.as_str()),
        ("Prod//EU", "folder path `Prod//EU` has an empty segment")
    );
    let (_, m) = folder_err("dbento = 1\n[folders.\"Prod \"]\n");
    assert!(m.contains("whitespace"), "{m}");
    let (_, m) = folder_err("dbento = 1\n[folders.\"\"]\n");
    assert_eq!(m, "folder path is empty");
    let (f, m) = folder_err("dbento = 1\n[folders.\"P\"]\ncolor = \"pink\"\n");
    assert_eq!((f.as_str(), m.as_str()), ("P", "unknown color `pink`"));
    let e = open("dbento = 1\n[folders.\"P\"]\ncolor = \"red\"\n").unwrap();
    assert_eq!(e.folders().len(), 1);
    let shown = OpenError::InvalidFolder {
        folder: "A".into(),
        message: "m".into(),
    };
    assert_eq!(shown.to_string(), "folder `A`: m");
}

#[test]
fn unknown_keys_passwords_and_bad_connection_folders_are_rejected() {
    match open("dbento = 1\n[folders.\"P\"]\nread_only = true\n").unwrap_err() {
        OpenError::Parse { message, .. } => assert!(message.contains("read_only"), "{message}"),
        other => panic!("{other:?}"),
    }
    match open("dbento = 1\n[folders.\"P\"]\npassword = \"x\"\n").unwrap_err() {
        OpenError::Password { key } => assert_eq!(key, "folders.P.password"),
        other => panic!("{other:?}"),
    }
    let toml =
        "dbento = 1\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\nfolder = \"/x\"\n";
    match open(toml).unwrap_err() {
        OpenError::Invalid {
            connection,
            message,
        } => {
            assert_eq!(connection, "a");
            assert_eq!(message, "folder path `/x` has an empty segment");
        }
        other => panic!("{other:?}"),
    }
}
